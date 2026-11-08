//! 把一次回合结束事件拼成一条像人话的通知。
//!
//! 为什么拆成「标题 + 正文」两个字段，而不是直接拼成一个字符串：
//! 不同通道对结构的要求不一样 —— QQ 只需要一段文本（我们内部拼成一条），
//! 而 PushPlus 之类要求 title 和 content 分开传。
//! 保留结构，以后加通道就不用反过来拆字符串。

use crate::clock::hhmm;
use crate::config::Config;
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

/// 把事件拼成通知。
///
/// 最终形态（`show_footer = true` 时）：
/// ```text
/// [Codex] 任务完成 · notify          ← title
/// 11:44 完成 · 5h 已用 88% · 周 已用 14%
/// ---
/// 已把 notify 钩子接到 QQ 接口。      ← 摘要，来自 assistant 最后一句话
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
            meta.push_str(&format!(
                " · 5h 已用 {:.0}% · 周 已用 {:.0}%",
                l.five_hour, l.weekly
            ));
        }
    }
    let mut body = format!("{meta}\n");

    // ---- 摘要：优先用 Codex 最后说的话 ----
    // 退回 input_messages 的场景：这一轮 Codex 只调了工具、没输出文字，
    // 此时 last_assistant_message 是空的。用你发出的那句指令当摘要，
    // 至少能知道「是哪个任务的回合结束了」。
    let summary = ev
        .last_assistant_message
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .or_else(|| ev.input_messages.last().cloned())
        .map(|s| clip(&s, cfg.max_chars))
        .unwrap_or_default();
    if !summary.is_empty() {
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
