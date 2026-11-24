//! 识别 Codex 的「内部辅助回合」。
//!
//! Codex 每轮会顺带跑一些不给用户看的短回合：生成后续建议、会话标题、
//! 子 Agent 之间的通信等。它们的 `last-assistant-message` 是结构化 JSON
//! （`{"suggestions":[…]}` / `{"exclude":[]}`），但照样会触发
//! `agent-turn-complete` —— 于是主任务还在跑，手机先收到一条「任务完成」。
//!
//! 这类回合还有个特点：用的是临时 thread-id，**没有 rollout 会话文件、
//! 没有状态库记录、也没有会话名**。所以判据收紧为两条同时成立才跳过：
//!
//!   1. 最后一条回复是合法 JSON（以 `{` / `[` 开头且真能解析）
//!   2. 该 thread 既找不到 rollout 文件，也找不到会话名
//!
//! 正常会话里用户让 Codex 输出一段 JSON 时，第 2 条不成立，照常通知。
//! 只有消息本身就是 JSON 时才会去查文件，平均开销可忽略。
//!
//! **只读**：和 agent.rs / thread_name.rs 一样，绝不写 `~/.codex`。

use serde_json::Value;

use crate::event::Event;

pub fn is_internal_turn(ev: &Event) -> bool {
    let Some(msg) = ev.last_assistant_message.as_deref() else {
        return false;
    };
    if !is_structured_payload(msg) {
        return false;
    }

    // Verdict.file 是找到的 rollout 文件；None 表示连文件都没有。
    let has_rollout = crate::agent::classify(&ev.thread_id).file.is_some();
    let has_name = crate::thread_name::read_thread_name(&ev.thread_id).is_some();
    !has_rollout && !has_name
}

/// 是否是结构化输出。要求真能解析成 JSON：
/// `{这不是 JSON}` 这种碰巧带大括号的正常回答不会被误伤。
fn is_structured_payload(msg: &str) -> bool {
    let t = msg.trim();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return false;
    }
    serde_json::from_str::<Value>(t).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_json_counts_as_structured() {
        assert!(is_structured_payload(r#"{"exclude":[]}"#));
        assert!(is_structured_payload(r#"{"suggestions":[{"title":"继续"}]}"#));
        assert!(is_structured_payload("  [1, 2]  "));
        assert!(!is_structured_payload("{这不是 JSON}"));
        assert!(!is_structured_payload("已把 notify 钩子接到 QQ 接口，测试通过。"));
        assert!(!is_structured_payload(""));
    }
}
