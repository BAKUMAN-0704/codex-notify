//! 额度：从 Codex 的会话日志里读出 5 小时窗口和周窗口的已用百分比。
//!
//! 为什么不用接口查：Codex 没有公开的额度查询接口，但它在每次回合收尾时
//! 会把额度快照写进会话日志。读本地文件既不需要 token，也不产生网络请求。
//!
//! 日志位置与结构：
//! ```text
//! ~/.codex/sessions/2026/11/08/rollout-2026-11-08T11-33-36-<thread-id>.jsonl
//! {"payload":{"rate_limits":{
//!     "primary":  {"used_percent":83.0,"window_minutes":300,  "resets_at":...},
//!     "secondary":{"used_percent":13.0,"window_minutes":10080,"resets_at":...}}}}
//! ```
//! `window_minutes` 300 = 5 小时窗口，10080 = 7 天（周窗口）。
//!
//! **只读**：本模块绝不会在 `~/.codex` 下创建或修改任何文件。

use std::env;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::clock::{civil_from_days, local_now, TZ_OFFSET_SECS};

/// 只读文件尾部这么多字节。
///
/// 会话日志可能涨到上百 MB（长对话 + 大段工具输出），
/// 而我们要找的额度快照永远在最近一次收尾处，也就是文件末尾。
/// 读 512KB 足够覆盖最近若干条记录，耗时在毫秒级。
const TAIL_BYTES: u64 = 512 * 1024;

/// 一次额度快照
pub struct Limits {
    /// 5 小时窗口已用百分比（0~100）
    pub five_hour: f64,
    /// 周窗口已用百分比（0~100）
    pub weekly: f64,
    /// 5 小时窗口的重置时刻（Unix 秒）。日志里没这个字段时为空。
    pub five_hour_resets_at: Option<i64>,
    /// 周窗口的重置时刻（Unix 秒）
    pub weekly_resets_at: Option<i64>,
}

/// Codex 的配置目录。只用于**读取**会话日志。
///
/// 优先用 `CODEX_HOME` 环境变量（Codex 官方支持自定义），
/// 否则退回 Windows 的 `%USERPROFILE%\.codex`。
pub(crate) fn codex_home() -> PathBuf {
    if let Ok(v) = env::var("CODEX_HOME") {
        if !v.trim().is_empty() {
            return PathBuf::from(v);
        }
    }
    PathBuf::from(env::var("USERPROFILE").unwrap_or_default()).join(".codex")
}

/// 找额度快照。先按当前会话找，找不到再退回「当天最新的会话」。
///
/// 为什么要回退：`--test` / `--dry-run` 用的是假 thread-id，
/// 不可能匹配到任何文件；另外跨天续聊的会话也可能不在今天的目录里。
/// 回退拿到的额度虽然未必属于当前回合，但至少是最近的真实数字，比不显示强。
pub fn read_limits(thread_id: &str) -> Option<Limits> {
    scan_sessions(thread_id).or_else(|| scan_sessions(""))
}

/// 在最近两天的会话目录里找匹配的文件，取最后一条额度快照。
fn scan_sessions(thread_id: &str) -> Option<Limits> {
    let now = local_now();

    // 跨零点的长会话可能还躺在昨天的目录里，所以两天都看
    let prev_day = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| (d.as_secs() + TZ_OFFSET_SECS).saturating_sub(86_400) / 86_400)
        .unwrap_or(0) as i64;
    let (py, pm, pd) = civil_from_days(prev_day);

    for (y, m, d) in [(now.0, now.1, now.2), (py, pm, pd)] {
        let dir = codex_home()
            .join("sessions")
            .join(format!("{y:04}"))
            .join(format!("{m:02}"))
            .join(format!("{d:02}"));
        let Ok(entries) = fs::read_dir(&dir) else {
            continue; // 目录不存在（那天没写过会话），换下一天
        };

        // 收集候选文件，带修改时间，稍后按时间倒序
        let mut candidates: Vec<(SystemTime, PathBuf)> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            // thread-id 为空表示「不限会话」，用于回退路径
            .filter(|p| {
                thread_id.is_empty()
                    || p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.contains(thread_id))
            })
            .filter_map(|p| Some((fs::metadata(&p).ok()?.modified().ok()?, p)))
            .collect();
        candidates.sort_by(|a, b| b.0.cmp(&a.0)); // 最新的排前面

        // 逐个试：某个文件里没有额度记录（比如刚建还没跑完）就看下一个
        for (_, path) in candidates {
            if let Some(l) = limits_from_tail(&path) {
                return Some(l);
            }
        }
    }
    None
}

/// 从单个会话文件的尾部找出最后一条 `rate_limits`。
fn limits_from_tail(path: &PathBuf) -> Option<Limits> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();

    // 从「末尾往前 512KB」开始读。注意可能从某一行中间开始，
    // 但我们是倒着找最新记录的，被截断的首行不影响结果。
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);

    // 倒序扫描：最后一次出现的额度和当前状态最接近
    for line in text.lines().rev() {
        // 先做廉价的字符串判断，避免对每一行都跑一次 JSON 解析
        if !line.contains("\"rate_limits\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue; // 可能是被截断的半行，跳过
        };

        // 字段位置随 Codex 版本略有差异，两种都试
        let rl = v
            .get("payload")
            .and_then(|p| p.get("rate_limits"))
            .or_else(|| v.get("rate_limits"))?;

        let pct = |key: &str| {
            rl.get(key)
                .and_then(|w| w.get("used_percent"))
                .and_then(Value::as_f64)
        };
        // resets_at 是 Unix 秒。不同版本可能没有，缺失就当没有重置信息
        let resets = |key: &str| {
            rl.get(key)
                .and_then(|w| w.get("resets_at"))
                .and_then(Value::as_i64)
        };

        // 两个窗口都要有才返回：只显示一个的话，容易让人误解成「只剩这个额度」
        if let (Some(five_hour), Some(weekly)) = (pct("primary"), pct("secondary")) {
            return Some(Limits {
                five_hour,
                weekly,
                five_hour_resets_at: resets("primary"),
                weekly_resets_at: resets("secondary"),
            });
        }
    }
    None
}
