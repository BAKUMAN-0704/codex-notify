//! 配置：定义 notify.toml 的结构，并决定这个文件放在哪。
//!
//! 放置原则：配置和日志都跟着 exe 走，**不往 Codex 自己的配置目录
//! （`~/.codex`）里塞东西**。Codex 那边只保留 `config.toml` 里的一行
//! `notify = [...]`，那是它唯一的入口，躲不掉；其余的都在本项目目录里。

use std::env;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::logfile::log;

/// 顶层配置。
///
/// 每个字段都给了默认值，所以 notify.toml 可以只写关心的那几项。
/// 字段缺失不算错，缺整份文件也不算错（会走 fallback）。
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// 总开关。关掉后钩子直接退出，连日志都不发消息
    #[serde(default = "yes")]
    pub enabled: bool,

    /// 是否只通知主 Agent。
    ///
    /// Codex 的子 Agent、内部审查回合结束时也会触发 notify，但那不代表「活干完了」。
    /// 开启后按 thread-id 查会话日志的 thread_source，只给主会话发消息。
    /// 查不到 / 字段不认识时一律照常通知（宁可多发，不能漏推）。
    #[serde(default = "yes")]
    pub only_main_agent: bool,

    /// 摘要最多保留多少个字符（按字数，不是字节）
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,

    /// 单次 HTTP 请求超时（秒）。
    /// 这个值直接决定「回合结束后最多卡多久」，不要设大。
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// 消息标题前缀，默认 `[Codex]`
    #[serde(default = "default_prefix")]
    pub prefix: String,

    /// 是否在消息里带 5h / 周额度
    #[serde(default = "yes")]
    pub show_limits: bool,

    /// 额度后面是否带「什么时候重置」
    #[serde(default = "yes")]
    pub show_reset: bool,

    /// 额度按哪种口径显示：
    ///   "used"      —— 已用 62%
    ///   "remaining" —— 剩余 38%
    ///
    /// 用字符串而不是枚举：手写配置更直观，以后加新口径也不用改反序列化。
    #[serde(default = "default_limit_style")]
    pub limit_style: String,

    /// 是否在消息末尾带 `(turn N · thread XXXXXXXX)`
    ///
    /// 默认关：开启后每轮都多一行，只有同时开多个 Codex 会话时才值得开。
    #[serde(default = "no")]
    pub show_footer: bool,

    /// 链式转发的原有 notify 命令，例如
    /// ["C:\\...\\codex-computer-use.exe", "turn-ended"]
    #[serde(default)]
    pub chain: Vec<String>,

    /// QQ 通道配置。整段不写就是不发。
    #[serde(default)]
    pub qq: Option<Qq>,

    /// NapCat 托管配置。整段不写就是不管它。
    #[serde(default)]
    pub napcat: Option<Napcat>,
}

/// NapCat 托管：让托盘启动时一并拉起它，退出时一起结束。
///
/// 只在托盘模式下生效，钩子路径完全不碰这一段。
#[derive(Debug, Deserialize, Clone)]
pub struct Napcat {
    /// 启动命令，数组形式（第一个元素是可执行文件或 .bat）。
    /// 留空表示不托管 —— 即使托盘在跑，也不会去动 NapCat。
    ///
    /// 例：`['D:\tools\NapCat\launcher-user.bat']`
    #[serde(default)]
    pub command: Vec<String>,

    /// 托盘退出时是否把 NapCat 一起结束。
    ///
    /// 只会结束**由本程序启动的**那个；你自己手动开的 NapCat 它不认识，也不会去杀。
    #[serde(default = "yes")]
    pub kill_on_exit: bool,
}

impl Default for Napcat {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            kill_on_exit: true,
        }
    }
}

/// QQ 通道（走 OneBot v11 的 HTTP 接口，由 NapCat / LLOneBot 提供）
#[derive(Debug, Deserialize, Clone)]
pub struct Qq {
    /// NapCat 的 HTTP 服务地址，例如 `http://127.0.0.1:3000`
    #[serde(default = "default_base_url")]
    pub base_url: String,

    /// OneBot 的 access_token。NapCat 侧配了就要填一致的，没配就留空。
    /// 留空时不带鉴权头 —— 因为这个接口能以你的 QQ 身份发消息，
    /// 建议至少留一个 token，别完全裸奔。
    #[serde(default)]
    pub access_token: String,

    /// 发送目标，两种格式：
    ///   `private:QQ号` —— 私聊发给自己（最常用）
    ///   `group:群号`   —— 发到群里
    pub target: String,
}

// ---- 各字段的默认值。写成函数是因为 serde 的 default 只接受函数路径 ----

fn yes() -> bool {
    true
}
fn no() -> bool {
    false
}
fn default_max_chars() -> usize {
    260
}
fn default_timeout() -> u64 {
    5
}
fn default_base_url() -> String {
    "http://127.0.0.1:3000".to_string()
}
fn default_prefix() -> String {
    "[Codex]".to_string()
}
fn default_limit_style() -> String {
    "used".to_string()
}

impl Config {
    /// 读配置。任何异常都退回内置默认值，不 panic。
    ///
    /// 为什么这么「宽容」：钩子跑在 Codex 的收尾流程里，
    /// 这里一旦崩了会让 Codex 的回合结束报错，代价远大于「少发一条通知」。
    pub fn load() -> Self {
        let path = config_path();
        match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => c,
                Err(e) => {
                    // 这是最常见的故障：手改配置漏了引号或写错字段类型
                    log(&format!("配置解析失败 ({}): {e}", path.display()));
                    Config::fallback()
                }
            },
            // 文件不存在也走默认值：首次运行、或用户故意删掉
            Err(_) => Config::fallback(),
        }
    }

    /// 兜底配置：除了默认值，两个通道都不启用（宁可不发，不可乱发）
    pub fn fallback() -> Self {
        Config {
            enabled: true,
            only_main_agent: true,
            max_chars: default_max_chars(),
            timeout_secs: default_timeout(),
            prefix: default_prefix(),
            show_limits: true,
            show_reset: true,
            limit_style: default_limit_style(),
            show_footer: false,
            chain: Vec::new(),
            qq: None,
            napcat: None,
        }
    }
}

/// 找到 notify.toml 的完整路径。
///
/// 查找顺序（先命中先用）：
///   1. 环境变量 `CODEX_NOTIFY_DIR` 指定的目录 —— 给「把配置放到别处」留的口子
///   2. exe 所在目录
///   3. exe 的上一级、上两级 —— 这样从 `target\release` 里直接跑也能找到项目根的配置
///
/// 一个都没找到时，返回第 1 个候选路径（用于 `--init` 写文件、以及日志目录推导）。
///
/// 之所以要往上找：开发时用 `cargo run` 或直接跑 target 下的产物，
/// 而配置习惯放在项目根，多找两级能省掉反复复制配置的麻烦。
pub fn config_path() -> PathBuf {
    let mut roots: Vec<PathBuf> = Vec::new();

    if let Ok(v) = env::var("CODEX_NOTIFY_DIR") {
        if !v.trim().is_empty() {
            roots.push(PathBuf::from(v));
        }
    }

    if let Ok(exe) = env::current_exe() {
        let mut cur = exe.parent().map(PathBuf::from);
        for _ in 0..3 {
            let Some(dir) = cur else { break };
            roots.push(dir.clone());
            cur = dir.parent().map(PathBuf::from);
        }
    }

    // current_exe 理论上总会成功，这里只是防御
    if roots.is_empty() {
        roots.push(PathBuf::from("."));
    }

    roots
        .iter()
        .map(|d| d.join("notify.toml"))
        .find(|p| p.exists())
        .unwrap_or_else(|| roots[0].join("notify.toml"))
}

/// 配置和日志所在的目录 —— 就是 notify.toml 的父目录。
/// 这样 `config_path()` 一旦改了查找规则，日志会自动跟着走，不会分家。
pub fn data_dir() -> PathBuf {
    config_path()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 把一项布尔设置写回 notify.toml。
///
/// 为什么用 toml_edit 而不是「重新序列化整个 Config」：
/// 后者会把用户手写的注释、空行、字段顺序全部冲掉 ——
/// 改一个勾选框却毁掉半份配置，比不做还糟。
pub fn save_bool(key: &str, value: bool) -> Result<(), String> {
    save_item(key, toml_edit::value(value))
}

/// 把一项字符串设置写回 notify.toml
pub fn save_str(key: &str, value: &str) -> Result<(), String> {
    save_item(key, toml_edit::value(value))
}

/// 把一项整数设置写回 notify.toml（档位循环用）
pub fn save_int(key: &str, value: i64) -> Result<(), String> {
    save_item(key, toml_edit::value(value))
}

fn save_item(key: &str, item: toml_edit::Item) -> Result<(), String> {
    let path = config_path();

    // 文件不存在就用模板起一份，保证写回去的是一份完整可用的配置
    let text = fs::read_to_string(&path).unwrap_or_else(|_| CONFIG_TEMPLATE.to_string());
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("配置解析失败: {e}"))?;

    // 支持 "napcat.kill_on_exit" 这种点路径：逐级进入子表。
    // toml_edit 的 Index 对完整字符串是按键名整体处理的，
    // 不会自己拆点，所以这里手动走。
    let parts: Vec<&str> = key.split('.').collect();
    if parts.len() == 1 {
        doc[parts[0]] = item;
    } else {
        let table = doc.as_table_mut();
        let mut cur = table;
        for part in &parts[..parts.len() - 1] {
            if cur.get(part).map_or(true, |it| !it.is_table_like()) {
                // 子表不存在（或被占成了普通值）就重建一个
                cur[*part] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            cur = cur[*part].as_table_mut().ok_or("路径中间不是表")?;
        }
        cur[parts[parts.len() - 1]] = item;
    }

    fs::write(&path, doc.to_string()).map_err(|e| format!("写入失败: {e}"))
}

/// `--init` 生成的配置模板。
///
/// 用单引号原始字符串包住：Windows 路径里的反斜杠不用转义，
/// 用户复制粘贴时也不容易出错。
pub const CONFIG_TEMPLATE: &str = r#"# codex-notify 配置（放在 exe 旁边）
enabled = true
# 只通知主 Agent：子 Agent / 内部审查回合完成时也会触发 notify，
# 但它们不代表「活干完了」。按 thread-id 查会话日志的 thread_source 判定；
# 查不到就照常通知，宁可多发也不漏推真正的完成。
only_main_agent = true
# 摘要最多保留多少个字符（按字数算，一个汉字算 1）
max_chars = 260
# 单次 HTTP 超时（秒）。这个值直接决定回合结束后最多卡多久
timeout_secs = 5
# 消息标题前缀
prefix = "[Codex]"
# 消息里是否带上 5h / 周额度（从 ~/.codex 的会话日志里读，只读不写）
show_limits = true
# 额度后面是否带「什么时候重置」
show_reset = true
# 额度口径："used" 显示已用百分比，"remaining" 显示剩余百分比
limit_style = "used"
# 末尾是否带 (turn N · thread XXXXXXXX) 会话标记
show_footer = false

# 链式转发：Codex 的 notify 槽位只有一个，原有程序要在这里接住
# 不需要就留空 []
chain = []

[qq]
# NapCat / LLOneBot 的 HTTP 服务地址（WebUI 里「网络配置 -> HTTP 服务器」）
base_url = "http://127.0.0.1:3000"
# OneBot 的 access_token，和 NapCat 那边保持一致；没设置就留空
access_token = ""
# 发给自己：private:你的QQ号；发到群：group:群号
target = "private:10001"
"#;
