//! Codex 的 notify 事件。
//!
//! 调用约定：Codex 在回合结束时拉起本程序，并把事件 JSON 作为
//! **最后一个命令行参数**传进来（不是 stdin，也不是环境变量）：
//!
//! ```text
//! codex-notify.exe {"type":"agent-turn-complete", ...}
//! ```
//!
//! 目前 Codex 只会发 `agent-turn-complete` 这一种类型。
//! 想接「等待审批」之类的事件走的是另一套配置（`[tui] notifications`），
//! 不经过这里。

use serde::Deserialize;

use crate::logfile::log;

/// 一次回合结束事件。
///
/// 所有字段都加了 `default`：Codex 版本之间字段名和有无会变
/// （比如 `thread-id` 在某些版本写作 `thread_id`），
/// 缺字段时给空值继续跑，比直接反序列化失败更稳 —— 少一个字段也不至于丢通知。
#[derive(Debug, Deserialize, Default)]
pub struct Event {
    /// 事件类型，当前只有 `agent-turn-complete`
    #[serde(rename = "type", default)]
    pub kind: String,

    /// 会话 ID。同一个 Codex 会话里连续对话时保持不变。
    /// 除日志排查外，它还是去会话文件里找额度快照的钥匙。
    #[serde(rename = "thread-id", alias = "thread_id", default)]
    pub thread_id: String,

    /// 会话内的回合序号，从 1 开始递增
    #[serde(rename = "turn-id", alias = "turn_id", default)]
    pub turn_id: String,

    /// 该回合的工作目录，用来取项目名
    #[serde(default)]
    pub cwd: String,

    /// 用户这一轮发出的指令（可能多条）
    #[serde(rename = "input-messages", alias = "input_messages", default)]
    pub input_messages: Vec<String>,

    /// Codex 这轮最后说的那句话。可能为空（比如只调了工具没输出文字）
    #[serde(
        rename = "last-assistant-message",
        alias = "last_assistant_message",
        default
    )]
    pub last_assistant_message: Option<String>,
}

/// 从命令行参数里找出那段 JSON。
///
/// 为什么倒着找：Codex 保证事件追加在最后，而前面可能还有别的参数
/// （比如未来某天我们自己也给 notify 加了固定参数）。
/// 倒序扫描能一次命中，也不怕参数里混进其他以 `{` 开头的东西 ——
/// 还要求能真正反序列化成 `Event` 才算数。
///
/// 第二个返回值是原始 JSON 文本，用于原样转发给 chain 上的程序：
/// 转发的必须是原始字节，不能是我们反序列化再序列化一遍的版本，
/// 否则字段顺序、未知字段都会丢。
pub fn parse_event(args: &[String]) -> (Event, Option<String>) {
    for arg in args.iter().rev() {
        let trimmed = arg.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<Event>(trimmed) {
            return (ev, Some(trimmed.to_string()));
        }
    }
    log(&format!("未能解析事件 JSON，参数 {args:?}"));
    (Event::default(), None)
}

/// `--dry-run` 没带事件时用的样例数据。
///
/// 有这个才能「不碰网络、不碰 Codex」地预览消息长什么样，
/// 调文案时很方便。
pub fn sample_event() -> Event {
    Event {
        kind: "agent-turn-complete".into(),
        thread_id: "dry-run".into(),
        turn_id: "42".into(),
        cwd: r"C:\Users\you\projects\demo".into(),
        input_messages: vec!["实现通知功能".into()],
        last_assistant_message: Some(
            "已把 Codex 的 notify 钩子接到 QQ OneBot 接口，并保留原桌面版钩子的链式转发。".into(),
        ),
    }
}

/// `--test` 用的事件：用当前工作目录，这样标题里的项目名是真实的。
pub fn test_event() -> Event {
    Event {
        kind: "agent-turn-complete".into(),
        thread_id: "selftest".into(),
        turn_id: "1".into(),
        cwd: std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        input_messages: vec!["self test".into()],
        last_assistant_message: Some("这是一条来自 codex-notify --test 的测试消息。".into()),
    }
}
