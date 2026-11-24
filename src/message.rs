//! 把一次回合结束事件拼成一条像人话的通知。
//!
//! 为什么拆成「标题 + 正文」两个字段，而不是直接拼成一个字符串：
//! 不同通道对结构的要求不一样 —— QQ 只需要一段文本（我们内部拼成一条），
//! 而 PushPlus 之类要求 title 和 content 分开传。
//! 保留结构，以后加通道就不用反过来拆字符串。

use crate::clock::hhmm;
use crate::config::{Agy, Config};
use crate::event::Event;
use crate::limits::read_limits;
use crate::text::{clip, project_name};

/// 一条待发送的通知
pub struct Message {
    /// 标题：一行，不含换行
    pub title: String,
    /// 正文：可以多行
    pub body: String,
}

impl Message {
    /// 拍平成一段文本，给只有「一个 message 字段」的通道用（QQ 就是这种）。
    ///
    /// 正文为空时只发标题，避免留下一个空行。
    pub fn flat(&self) -> String {
        if self.body.is_empty() {
            self.title.clone()
        } else {
            format!("{}\n{}", self.title, self.body)
        }
    }
}

/// 拼一个额度片段，例如 `5h 已用 62%（18:20 重置）` 或 `5h 剩余 38%`。
///
/// `show_remaining` 决定口径；`show_reset` 由调用方在外面控制（这里只管值）。
/// 百分比取整：手机上看到「62.3%」既占地方又没有额外信息。
fn limit_segment(
    label: &str,
    used: f64,
    resets_at: Option<i64>,
    now: i64,
    show_remaining: bool,
) -> String {
    let (word, value) = if show_remaining {
        ("剩余", (100.0 - used).clamp(0.0, 100.0))
    } else {
        ("已用", used.clamp(0.0, 100.0))
    };

    let mut s = format!("{label} {word} {value:.0}%");

    // 重置时间由调用方通过 resets_at 传入；这里不判断开关，
    // 因为关掉 show_reset 时调用方根本不传 Some
    if let Some(ts) = resets_at {
        s.push_str(&format!("（{} 重置）", crate::clock::reset_label(ts, now)));
    }
    s
}

/// 旧版摘要：优先用 Codex 最后说的话；只调了工具、没输出文字时，
/// 退回你发出的最后一句指令，至少能知道是哪个回合结束了。
fn message_summary(ev: &Event, max_chars: usize) -> Option<String> {
    ev.last_assistant_message
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .or_else(|| ev.input_messages.last().cloned())
        .map(|s| clip(&s, max_chars))
        .filter(|s| !s.is_empty())
}

/// 把事件拼成通知。
///
/// 最终形态（`show_footer = true` 时）：
/// ```text
/// [Codex] 任务完成 · notify          ← title
/// 11:44 完成 · 5h 已用 88% · 周 已用 14%
/// ---
/// 重构支付回调处理逻辑              ← 摘要，默认显示会话名
/// (turn 12 · thread b5f6c1c2)
/// ```
pub fn compose(ev: &Event, cfg: &Config) -> Message {
    // 标题里只放项目名，不放完整路径：手机上完整路径会折成两三行，
    // 而最后一段目录名就足够分辨是哪个项目。
    //
    // 换行会被替换成空格：PushPlus 那类通道的标题含换行会直接报错，
    // 而 cwd 理论上可能包含奇怪的字符，顺手清理掉。
    let title = format!("{} 任务完成 · {}", cfg.prefix, project_name(&ev.cwd))
        .replace(['\n', '\r'], " ")
        .trim()
        .to_string();

    // ---- 第一行：完成时间 + 额度 ----
    let mut meta = format!("{} 完成", hhmm());
    if cfg.show_limits {
        // 读不到额度就不写这一截，不要显示「额度未知」占位 —— 只会让消息变吵
        if let Some(l) = read_limits(&ev.thread_id) {
            let now = crate::clock::now_unix();
            // "used" 是默认；任何其他值都按 "remaining" 处理，
            // 这样手写配置写错也不会突然什么都不显示
            let show_remaining = cfg.limit_style.eq_ignore_ascii_case("remaining");

            // 只显示「还没重置」的窗口。
            //
            // 额度快照是 Codex 上次跑完时写进日志的，可能已经过时。
            // 如果重置时间已经过去，说明这个窗口早就回满了，
            // 此时再报「已用 99%」纯属误导 —— 干脆不提。
            let mut segments: Vec<String> = Vec::new();

            // 时效判断始终用真实的重置时间；要不要显示则看 show_reset
            let r5 = l.five_hour_resets_at;
            let rw = l.weekly_resets_at;
            let shown = |ts: Option<i64>| if cfg.show_reset { ts } else { None };

            if r5.map_or(true, |ts| ts > now) {
                segments.push(limit_segment("5h", l.five_hour, shown(r5), now, show_remaining));
            }
            if rw.map_or(true, |ts| ts > now) {
                segments.push(limit_segment("周", l.weekly, shown(rw), now, show_remaining));
            }

            for s in segments {
                meta.push_str(" · ");
                meta.push_str(&s);
            }
        }
    }
    let mut body = format!("{meta}\n");

    // ---- 摘要 ----
    // 默认显示 Codex 给会话起的名字，比「最后一条回复」更能说明刚才是哪个任务。
    // 索引里查不到名字时退回旧摘要，避免通知变成只有时间的一行。
    let source = cfg.summary_source.trim().to_ascii_lowercase();
    let summary = match source.as_str() {
        "none" => None,
        "message" => message_summary(ev, cfg.max_chars),
        _ => crate::thread_name::read_thread_name(&ev.thread_id)
            .map(|s| clip(&s, cfg.max_chars))
            .or_else(|| message_summary(ev, cfg.max_chars)),
    };
    if let Some(summary) = summary.filter(|s| !s.is_empty()) {
        body.push_str("---\n");
        body.push_str(&summary);
        body.push('\n');
    }

    // ---- 末尾：会话定位信息 ----
    // 同时开多个 Codex 会话时，靠 thread 前 8 位区分是哪条线；
    // turn 则是这条线里的第几个回合。不需要就关掉 show_footer。
    if cfg.show_footer {
        let mut footer = Vec::new();
        if !ev.turn_id.is_empty() {
            footer.push(format!("turn {}", ev.turn_id));
        }
        if !ev.thread_id.is_empty() {
            // 只取前 8 位：完整 UUID 太长，前 8 位足够区分，
            // 真要查完整 ID 可以按前缀去 sessions 目录里找文件。
            let short: String = ev.thread_id.chars().take(8).collect();
            footer.push(format!("thread {short}"));
        }
        if !footer.is_empty() {
            body.push_str(&format!("({})\n", footer.join(" · ")));
        }
    }

    // 统一去掉尾部空行，避免通道那边多出一个空行
    Message {
        title,
        body: body.trim_end().to_string(),
    }
}

/// Antigravity（`--agy`）的版本。
///
/// 和 Codex 版的差别：
/// * **没有额度**。Antigravity 的配额在服务端，本地没有快照可读，
///   硬塞一个 Codex 的额度只会张冠李戴。
/// * **多一个模型名**。Antigravity 会切模型（flash / pro），
///   看一眼就知道这轮是谁干的。
/// * 结束原因有第三种：跑到步数上限。这时候说「任务完成」是撒谎。
pub fn compose_agy(ev: &crate::agy::Event, cfg: &Config, agy: &Agy) -> Message {
    compose_agy_inner(ev, cfg, agy, None)
}

/// 复查等太久、判断不了收没收工时的兜底消息。
///
/// 措辞刻意不用「完成」：这时候只知道它很久没动，不知道是干完了
/// 还是在等一个跑不完的后台命令。让用户自己点开看一眼，比误报强。
pub fn compose_agy_pending(ev: &crate::agy::Event, cfg: &Config, agy: &Agy) -> Message {
    compose_agy_inner(
        ev,
        cfg,
        agy,
        Some(("等待中", "长时间没有收尾，后台可能仍在运行")),
    )
}

fn compose_agy_inner(
    ev: &crate::agy::Event,
    cfg: &Config,
    agy: &Agy,
    outcome: Option<(&'static str, &'static str)>,
) -> Message {
    let (title_word, meta_word) = outcome.unwrap_or_else(|| ev.outcome());

    // 工作区可能为空（纯对话、没挂项目），这时候别在标题尾巴留一个孤零零的「·」
    let project = project_name(ev.cwd());
    let mut title = format!("{} {}", agy.prefix, title_word);
    if !project.is_empty() {
        title.push_str(" · ");
        title.push_str(&project);
    }
    let title = title.replace(['\n', '\r'], " ").trim().to_string();

    // ---- 第一行：时间 + 结束原因 + 模型（+ 出错原因）----
    let mut meta = format!("{} {}", hhmm(), meta_word);

    if agy.show_model && !ev.model_name.trim().is_empty() {
        meta.push_str(" · ");
        meta.push_str(ev.model_name.trim());
    }

    // 出错时把原因带上：这类消息最有用的就是「为什么挂的」。
    // 压成一行再截断 —— 原始信息可能是一大段 HTTP 报错。
    if ev.is_error() {
        if let Some(e) = ev.error_line() {
            meta.push_str(" · ");
            meta.push_str(&clip(&e, 120));
        }
    }

    // fullyIdle = false 表示后台还有命令/子任务在跑，这一轮并没有真的收尾。
    // 这种 Stop 之后通常还会再来一次，看到「后台仍在运行」就知道先别急着看结果。
    // 出错时不标：消息的重点是「为什么挂的」，再缀一句「后台仍在运行」只会添乱。
    if ev.fully_idle == Some(false) && !ev.is_error() {
        meta.push_str(" · 后台仍在运行");
    }

    let mut body = format!("{meta}\n");

    // ---- 摘要 ----
    // 默认用会话标题，和 Codex 那边的口径一致：手机上想知道的是
    // 「哪条线结束了」，不是「它最后说了什么」。
    // 标题读不到时退回用户最后一句输入，比空着强。
    let input_summary = || {
        ev.last_user_input
            .as_deref()
            .map(crate::agy::clean_input)
            .map(|s| clip(&s, cfg.max_chars))
            .filter(|s| !s.is_empty())
    };

    let summary = match agy.summary_source.trim().to_ascii_lowercase().as_str() {
        "none" => None,
        "input" => input_summary(),
        "message" => crate::agy::last_reply(&ev.transcript_path)
            .map(|s| clip(&s, cfg.max_chars))
            .filter(|s| !s.is_empty())
            .or_else(input_summary),
        _ => crate::agy::conversation_title(&ev.conversation_id, &ev.artifact_directory_path)
            .map(|s| clip(&s, cfg.max_chars))
            .or_else(input_summary),
    };

    if let Some(summary) = summary.filter(|s| !s.is_empty()) {
        body.push_str("---\n");
        body.push_str(&summary);
        body.push('\n');
    }

    // ---- 末尾：会话定位信息 ----
    if cfg.show_footer {
        let mut footer = Vec::new();
        if let Some(n) = ev.execution_num {
            footer.push(format!("turn {n}"));
        }
        if !ev.conversation_id.is_empty() {
            let short: String = ev.conversation_id.chars().take(8).collect();
            footer.push(format!("thread {short}"));
        }
        if !footer.is_empty() {
            body.push_str(&format!("({})\n", footer.join(" · ")));
        }
    }

    Message {
        title,
        body: body.trim_end().to_string(),
    }
}
