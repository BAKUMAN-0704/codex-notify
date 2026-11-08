//! codex-notify —— 把 Codex 的「回合结束」事件推送到 QQ。
//!
//! # 它是怎么被调用的
//!
//! Codex 在 `config.toml` 里读到一个 `notify` 数组，每次回合结束时
//! 按这个数组拉起一个子进程，并把事件 JSON 作为**最后一个参数**追加进去：
//!
//! ```text
//! notify = ['C:\...\codex-notify.exe']
//!        ↓ Codex 实际执行
//! codex-notify.exe {"type":"agent-turn-complete","cwd":"...","thread-id":"..."}
//! ```
//!
//! 因此本程序是一个**一次性进程**：启动 → 干活 → 退出。
//! 没有常驻、没有异步运行时，所有事情在一个线程里顺序做完。
//! （想让它常驻托盘可以加 `--tray`，见下方 `tray` 模块。）
//!
//! # 三条不可违反的红线
//!
//! 1. **不能阻塞 Codex 的收尾**。Codex 会等这个进程退出，所以 HTTP 必须设超时
//!    （默认 5 秒），且任何失败都不能让进程卡住。
//! 2. **不能往 stdout 乱写**。那会混进 Codex 的输出流。
//!    运行时只写 `notify.log`，stdout 只给 `--check` 这类人工命令用。
//! 3. **不能崩溃**。这里 panic 会让 Codex 的回合结束报错。
//!    所以配置解析失败、文件读不到、网络不通，全部降级处理。
//!
//! # 模块划分
//!
//! | 模块 | 职责 |
//! |---|---|
//! | `event`   | 事件结构与命令行解析 |
//! | `config`  | 读 notify.toml，决定配置放在哪 |
//! | `message` | 把事件拼成「标题 + 正文」 |
//! | `limits`  | 从 Codex 会话日志读 5h / 周额度（只读） |
//! | `qq`      | OneBot HTTP 发送与探测 |
//! | `chain`   | 把事件转发给原本占用 notify 槽位的程序 |
//! | `cli`     | `--init` / `--check` / `--help` |
//! | `tray`    | `--tray` 常驻托盘（仅 Windows） |
//! | `clock` `logfile` `text` | 时间、日志、文本小工具 |

// 编译成 GUI 子系统：Codex 每回合拉起本程序时不再闪一个黑色控制台窗口。
// 代价是命令行模式要看输出就得靠 tray::ensure_console() 去附着父进程控制台，
// 这一步在 main 开头无条件执行。
#![cfg_attr(windows, windows_subsystem = "windows")]

mod chain;
mod cli;
mod clock;
mod config;
mod event;
mod limits;
mod logfile;
mod message;
mod qq;
mod text;

#[cfg(windows)]
mod tray;

use std::env;
use std::time::Duration;

use config::Config;
use event::{parse_event, sample_event, test_event};
use logfile::log;
use message::compose;

/// 程序入口。所有分支都从这里分出去，但每条分支都以「立即返回」结束。
fn main() {
    // GUI 子系统的进程没有控制台，先从父进程那里借一个过来，
    // 否则 `--check` 之类从终端调用时会一个字都看不到。
    #[cfg(windows)]
    tray::ensure_console();

    // 跳过第 0 个参数（程序自身路径）
    let args: Vec<String> = env::args().skip(1).collect();

    // 找出子命令。允许出现在任意位置，这样 `codex-notify <JSON> --dry-run`
    // 这种把事件和开关混着写的用法也能工作。
    let flag = args
        .iter()
        .find(|a| a.starts_with("--") || *a == "-h" || *a == "/?")
        .cloned()
        .unwrap_or_default();

    // --help / --init 不需要读配置，放在最前面，配置坏了也能用
    match flag.as_str() {
        "--help" | "-h" | "/?" => {
            cli::usage();
            return;
        }
        "--init" => {
            cli::init_config();
            return;
        }
        _ => {}
    }

    let cfg = Config::load();

    if flag == "--check" {
        cli::check(&cfg);
        return;
    }

    // 常驻托盘：跑起来就一直待着，直到用户在菜单里点「退出」
    #[cfg(windows)]
    {
        // 两种进入方式：显式 `--tray`，或者干脆双击 exe（一个参数都没有）。
        // 后者才符合「默认在托盘」的直觉——双击一个 GUI 程序，
        // 期望的是它安静地缩到右下角，而不是什么都不发生。
        //
        // 不会和钩子路径打架：Codex 调用时一定会带上事件 JSON，参数不为空。
        let no_args = args.is_empty();
        if flag == "--tray" || no_args {
            tray::run(cfg);
            return;
        }
    }

    // 没有子命令 = 由 Codex 的 notify 钩子调用。
    //
    // 只有这条路径受 enabled 开关约束：--test / --dry-run 是人工调试，
    // 用户明确要求执行，不该被总开关挡住。
    let is_hook = flag.is_empty();
    if is_hook && !cfg.enabled {
        log("enabled = false，跳过");
        return;
    }

    // 调试用：造一条事件，走真实的发送流程（但不触发 chain，避免惊动桌宠）
    if flag == "--test" {
        let event = test_event();
        deliver(&cfg, &compose(&event, &cfg), &event.cwd);
        return;
    }

    // 正经路径：从命令行里把事件 JSON 抠出来
    let (mut event, raw) = parse_event(&args);

    // 调试用：只打印不发请求。没给事件就用样例，方便调文案
    if flag == "--dry-run" || flag == "--print" {
        if event.kind.is_empty() {
            event = sample_event();
        }
        let msg = compose(&event, &cfg);
        println!("--- 标题 ---");
        println!("{}", msg.title);
        println!("--- 正文 ---");
        println!("{}", msg.body);
        println!("--- 目标 ---");
        println!("{}", if cfg.qq.is_some() { "QQ（OneBot）" } else { "(未配置)" });
        return;
    }

    // 只认 agent-turn-complete。Codex 未来若新增事件类型，
    // 我们安静退出即可，不要用不认识的字段拼出一条莫名其妙的通知。
    if event.kind != "agent-turn-complete" {
        log(&format!("忽略事件类型 {:?}", event.kind));
        return;
    }

    // 先把事件原样交给原有钩子（桌面版桌宠），再发我们自己的消息。
    // 顺序很重要：万一后面网络卡住，桌宠也已经动起来了。
    if let Some(raw) = raw {
        chain::run(&cfg.chain, &raw);
    }

    deliver(&cfg, &compose(&event, &cfg), &event.cwd);
}

/// 唯一的发送出口。目前只有 QQ 一个通道。
///
/// 失败处理原则：记日志 + 往 stderr 说一句，然后正常返回（退出码 0）。
/// 不用非 0 退出码，是因为 Codex 可能把它当成「钩子执行失败」而报错，
/// 但我们只是「少发了一条通知」，不构成需要用户处理的故障。
pub(crate) fn deliver(cfg: &Config, msg: &message::Message, cwd: &str) {
    // target 为空视同没配置：这样想临时关掉通知，清空那一行就够了
    let Some(qq_cfg) = cfg.qq.as_ref().filter(|q| !q.target.trim().is_empty()) else {
        log("未配置 [qq].target，跳过发送");
        eprintln!("没有推送目标：在 notify.toml 里填 [qq].target");
        return;
    };

    let project = text::project_name(cwd);
    let timeout = Duration::from_secs(cfg.timeout_secs);

    match qq::send(qq_cfg, &msg.flat(), timeout) {
        Ok(()) => log(&format!("已发送 QQ {} · {project}", qq_cfg.target)),
        Err(e) => {
            log(&format!("QQ 发送失败: {e}"));
            eprintln!("发送失败: {e}");
        }
    }
}
