//! 会话名：从 Codex 的 `session_index.jsonl` 读 `thread_name`。
//!
//! Codex 会给每个会话起一个短名字（自动生成，或用 `/rename` 改），记在
//! `~/.codex/session_index.jsonl`，一行一条：
//!
//! ```text
//! {"id":"<thread-id>","thread_name":"设计算法部分细节文档","updated_at":"..."}
//! ```
//!
//! 同一个 id 可能出现多行（改名），取**最后一条**。
//! `state_5.sqlite` 的 `threads.name` 是同一份数据，但为了读一个字段给这个
//! 单文件程序塞进 SQLite 驱动不值得 —— 索引文件是纯文本，只读即可。
//!
//! 找不到文件、找不到 id、名字为空，一律返回 `None`，由调用方退回原来的摘要。
//! 这里**只读** `~/.codex`，绝不写入。

use std::fs;
use std::path::Path;

use serde_json::Value;

pub fn read_thread_name(thread_id: &str) -> Option<String> {
    read_from(
        &crate::limits::codex_home().join("session_index.jsonl"),
        thread_id,
    )
}

fn read_from(path: &Path, thread_id: &str) -> Option<String> {
    let id = thread_id.trim();
    if id.is_empty() {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;

    let mut found = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue; // 某行写坏了不影响其它记录
        };
        if v.get("id").and_then(|x| x.as_str()) != Some(id) {
            continue;
        }
        if let Some(name) = v.get("thread_name").and_then(|x| x.as_str()) {
            let name = name.trim();
            if !name.is_empty() {
                found = Some(name.to_string());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn latest_name_wins_and_missing_falls_back() {
        let path = std::env::temp_dir()
            .join(format!("codex-notify-index-{}.jsonl", std::process::id()));
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"id":"aaa","thread_name":"旧名字"}}"#).unwrap();
        writeln!(f, r#"{{"id":"aaa","thread_name":"新名字"}}"#).unwrap();
        writeln!(f, r#"{{"id":"bbb","thread_name":"  "}}"#).unwrap();
        writeln!(f, "这一行是坏的，不影响其它记录").unwrap();

        assert_eq!(read_from(&path, "aaa").as_deref(), Some("新名字"));
        assert_eq!(read_from(&path, "bbb"), None);
        assert_eq!(read_from(&path, "不存在"), None);
        assert_eq!(read_from(&path, ""), None);

        let _ = fs::remove_file(path);
    }
}
