//! 链式转发：把事件原样交给「原本占用 notify 槽位」的程序。
//!
//! 背景：Codex 的 `notify` 全局只有一个槽位。桌面版默认把它用于桌宠动画
//! （`codex-computer-use.exe turn-ended`）。我们占了这个位置之后，
//! 如果不把事件转回去，桌宠就不再响应任何回合结束。
//!
//! 做法：把原命令（含它的固定参数）配到 notify.toml 的 `chain` 数组里，
//! 收到事件后，把「原命令 + 原参数 + 本次事件 JSON」原样拼起来执行一次。
//! 这与 Codex 调用我们自己的方式完全一致，所以原程序感知不到中间隔了一层。

use std::process::Command;

use crate::logfile::log;

/// 启动链路上的原有程序，但**不等待它结束**。
///
/// 为什么用 `spawn` 而不是 `output`：
///   1. 我们不需要它的输出，也不关心成功与否；
///   2. 它可能要去做动画、发 IPC，耗时不确定，等它会拖慢整条通知链路；
///   3. 它是独立进程，我们退出之后它照样能跑完。
///
/// 顺序上必须在「发送通知」之前调用：万一后面的网络请求超时（默认 5 秒），
/// 也已经把事件交出去了，不会连累桌宠。
pub fn run(chain: &[String], payload: &str) {
    // 没配置 chain 就直接返回，属于最常见的正常情况
    if chain.is_empty() {
        return;
    }

    // chain[0] 是程序路径，其余是固定参数（例如 "turn-ended"）
    let mut cmd = Command::new(&chain[0]);
    for arg in &chain[1..] {
        cmd.arg(arg);
    }
    // 事件 JSON 追加在最后，和 Codex 的调用约定保持一致
    cmd.arg(payload);

    match cmd.spawn() {
        Ok(child) => log(&format!("chain 已启动 pid={}", child.id())),
        // 启动失败（路径写错、文件被删）只记日志：原程序挂了不该影响我们的通知
        Err(e) => log(&format!("chain 启动失败 {:?}: {e}", chain[0])),
    }
}
