//! 判断一条 notify 事件来自「主 Agent」还是「子 Agent」。
//!
//! Codex 的 notify 事件 JSON 里**没有**父子标记（只有 type / thread-id / turn-id /
//! cwd / input-messages / last-assistant-message），但子 Agent 结束回合时同样会触发
//! notify —— 于是主任务还在跑，手机就先收到一堆「干完了」的推送。
//!
//! 判据来自 Codex 自己写的会话日志：每个 thread 的 rollout 文件第一行是
//! `session_meta`，里面的 `thread_source` 标了来源：
//!
//! ```text
//! {"type":"session_meta","payload":{
//!    "id":"<thread-id>",
//!    "thread_source":"user",              // 主会话
//!    "agent_path":"/root/xxx",            // 只有子 Agent 才有
//!    ...}}
//! ```
//!
//! 实测（Codex Desktop 0.167）出现过的值：
//!   `user`             用户发起的主会话
//!   `subagent`         派出去的子 Agent，同时带 `agent_path` / `agent_nickname`
//!   `guardian_review`  内部审查回合
//!
//! `state_5.sqlite` 的 `threads.source` 与之对应（`vscode` / `{"subagent":...}`），
//! 但为了读一张表给这个单文件程序塞进整个 SQLite 驱动不值得 —— 会话日志是纯文本，
//! 而且项目本来就在读它（见 limits.rs）。
//!
//! **只读**：和 limits.rs 一样，绝不往 `~/.codex` 写任何东西。
//!
//! **查不到就当主会话**：这些都是 Codex 的内部实现细节，改版可能让字段消失或改名。
//! 过滤错了最多多推一条，把真正的「主 Agent 干完了」吞掉才是事故。

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// 第一行最多读这么多字节。`session_meta` 里可能带一整段 base_instructions，
/// 但通常只有几 KB；128 KB 足够，读不完就当判定失败（照常通知）。
const HEAD_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// 用户发起的主会话 —— 该通知
    Main,
    /// 子 Agent / 内部审查回合 —— 不该通知
    Sub,
    /// 找不到会话文件、字段缺失或格式变了 —— 按主会话处理（fail-open）
    Unknown,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Main => "主 Agent",
            Origin::Sub => "子 Agent / 内部回合",
            Origin::Unknown => "无法判定（按主会话处理）",
        }
    }

    /// 只有**明确**判定为子 Agent 才过滤；Unknown 必须放行。
    pub fn should_skip(self) -> bool {
        self == Origin::Sub
    }
}

/// 一次判定的完整结果，带上依据方便 `--classify` 排查。
#[derive(Debug)]
pub struct Verdict {
    pub origin: Origin,
    pub thread_source: Option<String>,
    pub file: Option<PathBuf>,
}

pub fn classify(thread_id: &str) -> Verdict {
    classify_in(&crate::limits::codex_home().join("sessions"), thread_id)
}

/// 把根目录拆成参数，单元测试就能用临时目录跑完整路径，不必动环境变量。
fn classify_in(sessions_root: &Path, thread_id: &str) -> Verdict {
    let id = thread_id.trim();
    if id.is_empty() {
        return Verdict {
            origin: Origin::Unknown,
            thread_source: None,
            file: None,
        };
    }

    let Some(file) = find_rollout(sessions_root, id) else {
        return Verdict {
            origin: Origin::Unknown,
            thread_source: None,
            file: None,
        };
    };

    let Some(meta) = first_line(&file) else {
        return Verdict {
            origin: Origin::Unknown,
            thread_source: None,
            file: Some(file),
        };
    };

    let thread_source = str_field(&meta, "thread_source")
        .or_else(|| str_field(&meta, "thread-source"));
    let agent_path = str_field(&meta, "agent_path").or_else(|| str_field(&meta, "agent-path"));
    // rollout 里 source 是 "vscode" 这类字符串；有些版本会把 subagent 信息塞进来，
    // 所以对整个值做字符串判断（是对象时 to_string 也能命中）。
    let source = meta
        .get("payload")
        .and_then(|p| p.get("source"))
        .map(|v| v.to_string())
        .unwrap_or_default();

    let mut origin = thread_source
        .as_deref()
        .map(source_to_origin)
        .unwrap_or(Origin::Unknown);

    // thread_source 缺失或没认出来时，用另外两个信号兜底。
    // 注意方向：这两个信号只能把 Unknown 变成 Sub，永远不能把 Sub 变成 Main。
    if origin == Origin::Unknown
        && (agent_path.is_some_and(|p| !p.is_empty()) || source.contains("subagent"))
    {
        origin = Origin::Sub;
    }

    Verdict {
        origin,
        thread_source,
        file: Some(file),
    }
}

/// 只认「明确是主会话」和「明确是子/内部回合」两种，其余一律 Unknown。
/// Codex 以后加新的来源值，最坏结果是多推一条，不会漏推。
fn source_to_origin(src: &str) -> Origin {
    match src {
        "user" => Origin::Main,
        s if s.contains("subagent") || s.contains("guardian") || s.contains("review") => {
            Origin::Sub
        }
        _ => Origin::Unknown,
    }
}

/// 按文件名里的 thread-id 找 rollout 文件。
///
/// 会话目录是 `sessions/年/月/日/`，主会话的文件可能躺在几个月前（续聊不会换文件），
/// 所以不能只看今天 —— 递归扫日期目录，命中就停。目录名倒序保证先碰到最近的。
/// 只读目录项、不读文件内容，几百个目录也就几十毫秒。
fn find_rollout(root: &Path, thread_id: &str) -> Option<PathBuf> {
    let mut found = None;
    walk(root, 0, thread_id, &mut found);
    found
}

fn walk(dir: &Path, depth: usize, thread_id: &str, found: &mut Option<PathBuf>) {
    if found.is_some() || depth > 4 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return; // 目录不存在 / 没权限：交给 fail-open
    };

    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        } else if path.extension().is_some_and(|x| x == "jsonl")
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(thread_id))
        {
            *found = Some(path);
            return;
        }
    }

    // 日期目录名形如 2026 / 11 / 08，字典序倒过来就是时间倒序
    dirs.sort();
    dirs.reverse();
    for d in dirs {
        walk(&d, depth + 1, thread_id, found);
    }
}

/// 只读第一行并解析。文件可能上百 MB，绝不能整份读进来。
fn first_line(path: &Path) -> Option<Value> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0u8; HEAD_BYTES];
    let n = file.read(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    let line = buf[..n].split(|&b| b == b'\n').next()?;
    serde_json::from_slice(line).ok()
}

fn str_field(meta: &Value, key: &str) -> Option<String> {
    meta.get("payload")?
        .get(key)?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn known_sources() {
        assert_eq!(source_to_origin("user"), Origin::Main);
        assert_eq!(source_to_origin("subagent"), Origin::Sub);
        assert_eq!(source_to_origin("guardian_review"), Origin::Sub);
        // 没见过的值一律放行，不能把主会话吞掉
        assert_eq!(source_to_origin("something_new"), Origin::Unknown);
        assert!(!Origin::Unknown.should_skip());
    }

    fn make_session(dir: &Path, id: &str, meta: &str) -> PathBuf {
        let day = dir.join("2026").join("11").join("08");
        fs::create_dir_all(&day).unwrap();
        let path = day.join(format!("rollout-2026-11-08T10-00-00-{id}.jsonl"));
        let mut f = File::create(&path).unwrap();
        writeln!(f, "{meta}").unwrap();
        // 后面塞点无关内容，确认只读第一行、不读整个文件
        writeln!(f, "{{\"type\":\"event_msg\"}}").unwrap();
        path
    }

    #[test]
    fn subagent_is_skipped_main_is_kept() {
        let tmp = std::env::temp_dir().join(format!("codex-notify-agent-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);

        let main = "main-thread";
        let sub = "sub-thread";
        let review = "review-thread";
        make_session(
            &tmp,
            main,
            &format!(r#"{{"type":"session_meta","payload":{{"id":"{main}","thread_source":"user"}}}}"#),
        );
        make_session(
            &tmp,
            sub,
            &format!(r#"{{"type":"session_meta","payload":{{"id":"{sub}","thread_source":"subagent","agent_path":"/root/audit"}}}}"#),
        );
        make_session(
            &tmp,
            review,
            &format!(r#"{{"type":"session_meta","payload":{{"id":"{review}","thread_source":"guardian_review"}}}}"#),
        );

        assert_eq!(classify_in(&tmp, main).origin, Origin::Main);
        assert_eq!(classify_in(&tmp, sub).origin, Origin::Sub);
        assert_eq!(classify_in(&tmp, review).origin, Origin::Sub);
        // 找不到文件 / 没带字段：照常通知
        assert_eq!(classify_in(&tmp, "no-such-thread").origin, Origin::Unknown);
        assert_eq!(classify_in(&tmp, "").origin, Origin::Unknown);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_thread_source_falls_back_to_agent_path() {
        let tmp = std::env::temp_dir().join(format!("codex-notify-agent-path-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        make_session(
            &tmp,
            "orphan",
            r#"{"type":"session_meta","payload":{"id":"orphan","agent_path":"/root/x"}}"#,
        );
        assert_eq!(classify_in(&tmp, "orphan").origin, Origin::Sub);
        let _ = fs::remove_dir_all(&tmp);
    }
}
