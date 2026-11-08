//! 日志：追加写入 `notify.log`，超过 256KB 自动轮转。
//!
//! 为什么所有诊断信息都写文件而不是 stdout：
//! 钩子进程是由 Codex 同步拉起的，我们不能往 stdout / stderr 乱写，
//! 否则会混进 Codex 的输出流，甚至被当成工具返回值的一部分。
//! 所以除了命令行子命令（--check 等）之外，运行时只往文件里记。

use std::fs::{self, OpenOptions};
use std::io::Write;

use crate::clock::timestamp;
use crate::config::data_dir;

/// 单个日志文件的大小上限。超过就改名成 `notify.log.1`，只保留一份历史。
/// 200KB 大概能存上千条记录，查最近的故障足够，也不会无限占盘。
const MAX_LOG_BYTES: u64 = 256 * 1024;

/// 追加一条日志。所有失败都被吞掉 —— 写日志本身出错，没有更好的地方去报告。
pub fn log(msg: &str) {
    let dir = data_dir();
    // 目录可能不存在（首次运行），忽略错误：写日志失败不影响主流程
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("notify.log");

    // 轮转：改成 .1 会覆盖上一份历史，所以最多只保留两代
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > MAX_LOG_BYTES {
            let _ = fs::rename(&path, dir.join("notify.log.1"));
        }
    }

    // create + append：文件不存在就建，存在就接着写；每次写完自动关闭
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {}", timestamp(), msg);
    }
}
