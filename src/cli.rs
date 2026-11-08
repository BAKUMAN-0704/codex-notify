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
pub fn usage() {
    println!(
        r#"codex-notify 0.1.0 — Codex 回合完成 -> QQ 通知

用法:
  codex-notify.exe '<事件JSON>'   由 Codex 的 notify 钩子调用
  codex-notify.exe --test         用一条假事件真发一条，验证整条链路
  codex-notify.exe --dry-run      只打印将要发送的内容，不发请求
  codex-notify.exe --init         生成默认配置（放在 exe 旁边）
  codex-notify.exe --check        检查配置与 NapCat 连通性
  codex-notify.exe --help         显示本帮助

配置文件: {}
日志文件: {}\notify.log"#,
        config_path().display(),
        data_dir().display()
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
    println!("enabled: {}", cfg.enabled);
    println!("prefix: {}", cfg.prefix);
    println!(
        "show_limits: {} / show_footer: {}",
        cfg.show_limits, cfg.show_footer
    );
    println!("chain: {} 个", cfg.chain.len());
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
