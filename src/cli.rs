//! 命令行子功能：打印帮助、生成配置、自检。
//!
//! 这几个入口和「钩子主流程」是分开的：它们给人看、给人跑，
//! 允许往 stdout 输出；而钩子流程必须保持安静，只写日志。

use std::fs;
use std::time::Duration;

use crate::config::{config_path, data_dir, Config, CONFIG_TEMPLATE};
use crate::qq;
use crate::text::clip;

/// 打印用法。路径都是运行时算出来的，所以显示的一定是本机真实位置。
/// 版本号取自 Cargo.toml，避免改了版本却忘了改这里。
pub fn usage() {
    println!(
        r#"codex-notify {} — Codex 回合完成 -> QQ 通知

用法:
  codex-notify.exe '<事件JSON>'   由 Codex 的 notify 钩子调用
  codex-notify.exe                不带参数启动 = 常驻托盘（仅 Windows）
  codex-notify.exe --set key=value  改一项配置（如 --set show_limits=false）
  codex-notify.exe --classify <id> 查某个 thread-id 会被判为主/子 Agent
  codex-notify.exe --tray         同上，显式指定
  codex-notify.exe --test         用一条假事件真发一条，验证整条链路
  codex-notify.exe --dry-run      只打印将要发送的内容，不发请求
  codex-notify.exe --init         生成默认配置（放在 exe 旁边）
  codex-notify.exe --check        检查配置与 NapCat 连通性
  codex-notify.exe --help         显示本帮助

配置文件: {}
日志文件: {}\notify.log"#,
        env!("CARGO_PKG_VERSION"),
        config_path().display(),
        data_dir().display()
    );
}

/// `--set key=value`：命令行改配置，和托盘菜单走同一条写回路径。
///
/// 值的类型自动识别（true/false → 布尔，数字 → 整数，其余 → 字符串）。
/// 键支持点路径，例如 `--set napcat.kill_on_exit=false`。
///
/// 有这个入口除了方便脚本化，也是给菜单写入逻辑留一条可验证的路：
/// 两条路径共用 `save_item`，命令行测通了菜单就通了。
pub fn set(pair: &str) {
    let Some((key, value)) = pair.split_once('=') else {
        println!("格式应为 --set key=value，例如 --set show_limits=false");
        return;
    };
    let key = key.trim();
    let value = value.trim();

    let result = if value == "true" || value == "false" {
        crate::config::save_bool(key, value == "true")
    } else if let Ok(n) = value.parse::<i64>() {
        crate::config::save_int(key, n)
    } else {
        crate::config::save_str(key, value)
    };

    match result {
        Ok(()) => println!("已写入 {key} = {value}（{}）", config_path().display()),
        Err(e) => println!("写入失败: {e}"),
    }
}

/// `--classify <thread-id>`：把「这条事件会不会被过滤」摊开给人看。
///
/// 不发消息、不改文件、不读配置，纯粹是排查工具：
/// 收到一条意外的通知（或该来的没来）时，拿事件 JSON 里的 thread-id 查一下即可。
pub fn classify(thread_id: &str) {
    let v = crate::agent::classify(thread_id);
    println!("thread-id:     {thread_id}");
    println!(
        "会话文件:     {}",
        v.file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(未找到)".to_string())
    );
    println!(
        "thread_source: {}",
        v.thread_source.as_deref().unwrap_or("(无此字段)")
    );
    println!("判定:         {}", v.origin.label());
    println!(
        "结果:         {}",
        if v.origin.should_skip() {
            "会跳过通知"
        } else {
            "会照常通知"
        }
    );
}

/// 生成默认配置。已存在就不覆盖 —— 用户手改过的配置比模板值钱。
pub fn init_config() {
    let path = config_path();
    if path.exists() {
        println!("已存在，未覆盖: {}", path.display());
        return;
    }

    if let Err(e) = fs::create_dir_all(data_dir()) {
        eprintln!("创建目录失败: {e}");
        std::process::exit(1);
    }

    match fs::write(&path, CONFIG_TEMPLATE) {
        Ok(_) => println!("已生成: {}\n改完 target 后跑一次 --check", path.display()),
        Err(e) => {
            eprintln!("写入失败: {e}");
            std::process::exit(1);
        }
    }
}

/// 自检：把配置里「会怎么发」和「能不能发」都摊开给人看。
///
/// 分两步：
///   1. 静态检查 —— 配置读到了什么、哪些通道启用、敏感值是否已设置
///   2. 动态探测 —— 真的请求一次 NapCat，确认它活着且已登录
///
/// 敏感值（access_token）只显示「已设置 / 空」，不打印内容：
/// 这个输出经常被截图贴到聊天里求助，不能把 token 泄露出去。
pub fn check(cfg: &Config) {
    println!("配置文件: {}", config_path().display());
    println!("enabled: {} / only_main_agent: {}", cfg.enabled, cfg.only_main_agent);
    println!("prefix: {}", cfg.prefix);
    println!(
        "show_limits: {} / show_reset: {} / limit_style: {} / show_footer: {} / summary_source: {}",
        cfg.show_limits, cfg.show_reset, cfg.limit_style, cfg.show_footer, cfg.summary_source
    );
    println!("chain: {} 个", cfg.chain.len());
    println!(
        "napcat.shell: {}",
        cfg.napcat.as_ref().map(|n| n.shell.as_str()).unwrap_or("pwsh")
    );
    println!();

    println!("== QQ（走本机 NapCat）==");
    match cfg.qq.as_ref().filter(|q| !q.target.trim().is_empty()) {
        None => println!("未启用（target 为空）"),
        Some(q) => {
            println!("base_url: {}", q.base_url);
            println!("target:   {}", q.target);
            println!(
                "token:    {}",
                if q.access_token.is_empty() {
                    "(空)"
                } else {
                    "(已设置)"
                }
            );

            // 动态探测：连不上就把最常见的三个原因列出来，
            // 省得用户对着 "connection refused" 发愣
            match qq::probe(q, Duration::from_secs(cfg.timeout_secs)) {
                Ok(raw) => println!("连通性:   OK -> {}", clip(&raw, 160)),
                Err(e) => {
                    println!("连通性:   失败 ({e})");
                    println!("  · NapCat 是否在运行、HTTP 服务器是否已启用？");
                    println!("  · base_url 的端口是否和 NapCat 里配的一致？");
                    println!("  · access_token 是否和 NapCat 里配的一致？");
                }
            }
        }
    }

    println!();
    if cfg.qq.as_ref().is_none_or(|q| q.target.trim().is_empty()) {
        println!("没有可用的推送目标：在 notify.toml 里填 [qq].target");
    } else {
        println!("下一步: codex-notify.exe --dry-run  然后  --test");
    }
}
