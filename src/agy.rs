//! Antigravity（agy）的 Stop 钩子。
//!
//! # 和 Codex 的差别
//!
//! Codex 是「命令行参数传事件、不要输出」；Antigravity 正好相反：
//!
//! ```text
//! Antigravity 停下来了
//!       │  把事件 JSON 写进 stdin
//!       ▼
//! codex-notify.exe --agy
//!       │  stdout 必须且只能输出 {"decision":"stop"}
//!       ▼
//! Antigravity 读到 decision，允许停止
//! ```
//!
//! 这个差异带来两条硬约束：
//!
//! 1. **stdout 只能有那一个 JSON**。我们自己的日志一律走文件，
//!    调试信息走 stderr —— 多打一个字都可能让 Antigravity 解析失败。
//! 2. **不能被 `ensure_console()` 抢走 stdout**。本程序是 GUI 子系统，
//!    `AttachConsole` 会把标准句柄换成控制台，Antigravity 就读不到 decision 了。
//!    所以 `--agy` 分支必须在 `ensure_console()` **之前**处理。
//!
//! # 它是怎么被装上来的
//!
//! `--init-agy` 往 `~/.gemini/config/hooks.json` 写一个 `Stop` 处理器。
//! App / IDE / CLI 三者共用这个全局文件（各自的 payload 里
//! `artifactDirectoryPath` 指向自己的数据目录）。

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;

use crate::config::{Agy, Config};
use crate::logfile::log;

/// Antigravity 的 Stop 事件（stdin 上的 JSON，字段名是 camelCase）。
///
/// 所有字段都给默认值：这套结构是 Antigravity 的内部实现细节，
/// 升级后加字段、少字段都不该让通知整个失效。
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// 会话 UUID。同一个对话多轮共用一个，用来找会话标题。
    #[serde(default)]
    pub conversation_id: String,

    /// 挂载的工作区路径，第一个当作「项目目录」用。
    #[serde(default)]
    pub workspace_paths: Vec<String>,

    /// 会话日志 `transcript.jsonl` 的绝对路径。
    /// 想显示「最后一条回复」时从这里读。
    #[serde(default)]
    pub transcript_path: String,

    /// 会话产物目录（`<app_data>/brain/<conversationId>`）。
    /// 用来反推 app_data 目录，进而找到 `annotations/<id>.pbtxt` 里的标题。
    #[serde(default)]
    pub artifact_directory_path: String,

    /// 这轮用的模型，例如 `gemini-3.6-flash-medium`
    #[serde(default)]
    pub model_name: String,

    /// 用户最后一句输入。拿不到会话标题时兜底用它。
    #[serde(default)]
    pub last_user_input: Option<String>,

    /// 第几次执行（同一会话内递增）
    #[serde(default)]
    pub execution_num: Option<i64>,

    /// 为什么停：`model_stop` / `max_steps_exceeded` / `error`
    #[serde(default)]
    pub termination_reason: Option<String>,

    /// 系统错误信息（`terminationReason` 为 error 时通常有值）
    #[serde(default)]
    pub error: Option<String>,

    /// 是否真的彻底干完了。后台还有命令在跑时是 false。
    #[serde(default)]
    pub fully_idle: Option<bool>,
}

impl Event {
    /// 项目目录：Antigravity 可能挂多个工作区，取第一个。
    pub fn cwd(&self) -> &str {
        self.workspace_paths
            .first()
            .map(String::as_str)
            .unwrap_or("")
    }

    /// 是不是出错结束的。
    ///
    /// 两个判据都看：`terminationReason` 是官方说法，
    /// 但有些版本只填 `error` 字段，不填 reason。
    ///
    /// 比较时忽略大小写：文档写的是小写 `error`，
    /// 而 2.x 实际下发的是大写 `ERROR`，硬比小写会把出错当成功。
    pub fn is_error(&self) -> bool {
        if self
            .termination_reason
            .as_deref()
            .is_some_and(|r| r.eq_ignore_ascii_case("error"))
        {
            return true;
        }
        self.error
            .as_deref()
            .is_some_and(|e| !e.trim().is_empty())
    }

    /// 这次 Stop 属于哪一类。通知开关按这个分类走。
    pub fn kind(&self) -> Kind {
        if self.is_error() {
            return Kind::Error;
        }
        let reason = self
            .termination_reason
            .as_deref()
            .map(|r| r.to_ascii_lowercase())
            .unwrap_or_default();
        if reason == "max_steps_exceeded" {
            Kind::Aborted
        } else {
            Kind::Done
        }
    }

    /// 结束原因对应的说法，返回 (标题里的词, 正文里的词)。
    ///
    /// `max_steps_exceeded` 单独拎出来：它是「跑到步数上限」而不是「干完了」，
    /// 混成一句「任务完成」会误导人 —— 活其实没做完。
    pub fn outcome(&self) -> (&'static str, &'static str) {
        match self.kind() {
            Kind::Error => ("任务出错", "出错"),
            Kind::Aborted => ("任务中止", "中止（达到步数上限）"),
            Kind::Done => ("任务完成", "完成"),
        }
    }

    /// 错误信息，压成一行给正文用。
    pub fn error_line(&self) -> Option<String> {
        self.error
            .as_deref()
            .map(|e| e.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|e| !e.is_empty())
    }
}

/// 这次 Stop 属于哪一类。配置里的 `notify_*` 开关就是按它分流的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 真的干完了
    Done,
    /// 到达步数上限而停下，活没做完
    Aborted,
    /// 出错结束
    Error,
}

impl Kind {
    /// 日志里用的说法
    pub fn label(self) -> &'static str {
        match self {
            Kind::Done => "完成",
            Kind::Aborted => "中止",
            Kind::Error => "出错",
        }
    }
}

/// 这类事件要不要发通知。
///
/// 默认只发「完成」和「出错」—— 这两件是必须知道的；
/// 「步数上限中止」和「还没收工的占位通知」默认静音，嫌吵的人不用管，
/// 想全都要的把对应开关打开即可。
fn wants(agy: &Agy, kind: Kind) -> bool {
    match kind {
        Kind::Done => agy.notify_done,
        Kind::Error => agy.notify_error,
        Kind::Aborted => agy.notify_aborted,
    }
}

/// 读完 stdin 上的事件 JSON。
///
/// 读失败返回空串，由调用方按「解析不出事件」处理 —— 这里不 panic，
/// 因为 panic 会让 Antigravity 拿不到 decision。
pub fn read_stdin() -> String {
    let mut raw = String::new();
    // 忽略错误：读到一半失败也比直接崩好
    let _ = std::io::stdin().read_to_string(&mut raw);
    raw
}

/// 解析事件。空输入 / 坏 JSON 都返回 `None`。
pub fn parse(raw: &str) -> Option<Event> {
    serde_json::from_str::<Event>(raw.trim()).ok()
}

/// 读会话标题：`<app_data>/annotations/<conversationId>.pbtxt` 里的 `title:"..."`。
///
/// 这个文件用的是 protobuf 的文本格式，一行里塞多个字段，例如：
///
/// ```text
/// title:"Refactor auth module" last_user_view_time:{...}
/// ```
///
/// 所以不能按 `=` 切，只能认出 `title` 后面那对引号。
///
/// 路径来源有两个：
///   1. 从 payload 的 `artifactDirectoryPath`（`<app_data>/brain/<id>`）上溯两级
///   2. 兜底扫三个已知的 app_data 目录（App / IDE / CLI 各一个）
pub fn conversation_title(conv_id: &str, artifact_dir: &str) -> Option<String> {
    let id = conv_id.trim();
    if id.is_empty() {
        return None;
    }

    let mut candidates: Vec<PathBuf> = Vec::new();

    let artifact = expand_home(artifact_dir.trim());
    if let Some(app_data) = artifact
        .parent() // <app_data>/brain
        .and_then(|p| p.parent()) // <app_data>
    {
        candidates.push(app_data.join("annotations").join(format!("{id}.pbtxt")));
    }

    if let Some(home) = user_home() {
        for app in ["antigravity", "antigravity-ide", "antigravity-cli"] {
            candidates.push(
                home.join(".gemini")
                    .join(app)
                    .join("annotations")
                    .join(format!("{id}.pbtxt")),
            );
        }
    }

    candidates
        .iter()
        .find_map(|p| fs::read_to_string(p).ok().and_then(|t| parse_title(&t)))
}

/// 从 pbtxt 文本里抠出 `title:"..."`。
///
/// 手写扫描而不是上解析器：这里只需要一个字符串字段，
/// 为它引进 protobuf 依赖不值得。转义按 protobuf 的规则简单处理（`\"` `\\`）。
fn parse_title(text: &str) -> Option<String> {
    let at = text.find("title:")?;
    let rest = text[at + "title:".len()..].trim_start();
    let mut chars = rest.strip_prefix('"')?.chars();

    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                }
            }
            '"' => {
                let s = out.trim().to_string();
                return if s.is_empty() { None } else { Some(s) };
            }
            _ => out.push(c),
        }
    }
    None
}

/// 会话日志里最后一条模型文本回复。
///
/// transcript.jsonl 每行一条记录，字段是 snake_case：
///   * `USER_INPUT` —— 你发的
///   * `PLANNER_RESPONSE` —— 模型这一轮的动作（`thinking` + `tool_calls`）
///   * `GENERIC` —— 工具返回的内容
///   * `ERROR_MESSAGE` —— 报错
///
/// 注意：**实测（Antigravity 2.x）`PLANNER_RESPONSE` 里没有 `content`**，
/// 只有 `thinking` 和 `tool_calls`，也就是说 Antigravity 不会把
/// 「对用户说的最后一句话」落到本地日志里。所以这个函数现在多数时候返回
/// `None`，由调用方退回 `lastUserInput`。
///
/// 留着它是因为这属于内部实现细节：万一某个版本开始写 `content` 了，
/// 这里就能直接用上，不用改配置。
pub fn last_reply(transcript_path: &str) -> Option<String> {
    let path = expand_home(transcript_path.trim());
    let text = read_limited(&path)?;

    let mut found = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // 单行写坏不影响其它记录
        };
        if v.get("source").and_then(|x| x.as_str()) != Some("MODEL") {
            continue;
        }
        if v.get("type").and_then(|x| x.as_str()) != Some("PLANNER_RESPONSE") {
            continue;
        }
        if let Some(c) = v.get("content").and_then(|x| x.as_str()) {
            let c = c.trim();
            if !c.is_empty() {
                found = Some(c.to_string());
            }
        }
    }
    found
}

/// 会话日志最后停在哪一步。
///
/// # 为什么需要这个
///
/// `Stop` 的触发点比「任务完成」宽得多：它是**执行循环终止**时触发，
/// 而 Antigravity 的循环经常自己接着跑下一轮。实测两种典型误报：
///
///   * 工具刚返回结果（最后一条是 `GENERIC`），模型还没来得及想下一步；
///   * 模型说了句「正在等后台计算完成」，但任务远没结束。
///
/// 光看 `terminationReason` 分不出来（两者都是 `NO_TOOL_CALL`），
/// 所以得回头看日志尾巴。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    /// 模型把话说完、没跟任何工具调用 —— 这一轮真的停下来了
    Spoken,
    /// 最后一条是工具输出，或模型还在调工具 —— 下一步还没开始
    Busy,
    /// 读不到（路径为空 / 文件不存在 / 格式变了）
    Unknown,
}

/// 读会话日志的最后一条记录，判断执行循环停在哪。
pub fn tail_state(transcript_path: &str) -> Tail {
    let path = expand_home(transcript_path.trim());
    if path.as_os_str().is_empty() {
        return Tail::Unknown;
    }
    let Some(text) = read_limited(&path) else {
        return Tail::Unknown;
    };

    // 只留最后一条能解析的记录：中间可能有写坏的半行，跳过即可
    let mut last: Option<serde_json::Value> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            last = Some(v);
        }
    }

    let Some(v) = last else {
        return Tail::Unknown;
    };

    match v.get("type").and_then(|x| x.as_str()).unwrap_or("") {
        // 模型这一轮的动作。带工具调用 = 还要接着干
        "PLANNER_RESPONSE" => {
            if has_tool_calls(&v) {
                Tail::Busy
            } else {
                Tail::Spoken
            }
        }
        // 工具输出回来了，模型还没接上；系统/错误消息同理
        "GENERIC" | "ERROR_MESSAGE" | "SYSTEM_MESSAGE" => Tail::Busy,
        // 用户刚发话，等模型接手。Stop 里基本见不到，保守当没完
        "USER_INPUT" => Tail::Busy,
        _ => Tail::Unknown,
    }
}

/// `tool_calls` 字段是不是非空。
///
/// 这个字段的形态随版本变过：有时是数组，有时被序列化成字符串。
/// 只认「明确为空」为否，其它一律当成有调用 —— 宁可少发一条，
/// 也不要在它还要接着干的时候喊「完成」。
fn has_tool_calls(v: &serde_json::Value) -> bool {
    match v.get("tool_calls") {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Array(a)) => !a.is_empty(),
        Some(serde_json::Value::String(s)) => {
            let t = s.trim();
            !t.is_empty() && t != "[]"
        }
        Some(_) => true,
    }
}

/// 这次 Stop 该不该发通知。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 活干完了，发
    Done,
    /// 还在干活，这次先扣着（交给后台复查）
    Busy(&'static str),
}

/// 判据：官方字段优先，日志尾巴兜底。
pub fn verdict(ev: &Event) -> Verdict {
    // 后台命令 / 异步任务还在跑 —— 文档里写得很明确，最硬的信号
    if ev.fully_idle == Some(false) {
        return Verdict::Busy("后台任务还在运行");
    }
    match tail_state(&ev.transcript_path) {
        Tail::Busy => Verdict::Busy("工具刚返回，模型还没开始下一步"),
        // 读不到日志时不能扣着不发：这套路径是内部实现细节，
        // 变了就应该退回「照常通知」，而不是从此静默
        Tail::Spoken | Tail::Unknown => Verdict::Done,
    }
}

/// 在实际配置下，这次 Stop 到底等不等。
///
/// 出错是终态，直接放行：出错时日志最后一条往往是 `ERROR_MESSAGE`，
/// 若照常判成「还没收工」，这条最该看到的通知会被扣住。
fn verdict_for(ev: &Event, agy: &Agy) -> Verdict {
    if ev.kind() == Kind::Error || !agy.only_when_finished {
        Verdict::Done
    } else {
        verdict(ev)
    }
}

/// 读文件，超过 8 MB 只读尾部。
///
/// 会话日志会随着对话一直追加，长会话能到几十 MB。
/// 我们只要最后一条回复，读尾部就够了；`split_once('\n')` 是为了丢掉
/// 被截断的那半行 —— 半行 JSON 一定解析失败，留着也没用。
fn read_limited(path: &Path) -> Option<String> {
    const LIMIT: u64 = 8 * 1024 * 1024;
    let meta = fs::metadata(path).ok()?;

    if meta.len() <= LIMIT {
        return fs::read_to_string(path).ok();
    }

    let bytes = fs::read(path).ok()?;
    let start = bytes.len().saturating_sub(LIMIT as usize);
    let tail = String::from_utf8_lossy(&bytes[start..]).to_string();
    Some(match tail.split_once('\n') {
        Some((_, rest)) => rest.to_string(),
        None => tail,
    })
}

/// 用户最后一句输入，去掉 Antigravity 包在用户请求外面的标签。
///
/// 实测 `lastUserInput` 可能长这样：
///
/// ```text
/// <USER_REQUEST>
/// 帮我看下这段代码……
/// </USER_REQUEST>
/// <ADDITIONAL_METADATA>…</ADDITIONAL_METADATA>
/// ```
///
/// 标签和元数据都不是用户写的，留在通知里只会占地方。
pub fn clean_input(raw: &str) -> String {
    let mut s = raw.trim();

    if let Some(start) = s.find("<USER_REQUEST>") {
        s = &s[start + "<USER_REQUEST>".len()..];
    }
    if let Some(end) = s.find("</USER_REQUEST>") {
        s = &s[..end];
    }
    if let Some(end) = s.find("<ADDITIONAL_METADATA>") {
        s = &s[..end];
    }
    s.trim().to_string()
}

fn expand_home(p: &str) -> PathBuf {
    if p.is_empty() {
        return PathBuf::new();
    }
    // 文档示例里写的是 `~/.gemini/...`，实际运行时通常是绝对路径，
    // 但多处理一种写法不亏
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        if let Some(home) = user_home() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

fn user_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// `--agy` 的入口。
///
/// 无论中间发生什么（配置读不到、发消息失败、事件解析不了），
/// **最后一定会输出 `{"decision":"stop"}`**。
/// 这是和 Antigravity 的契约：没有 decision，它不知道要不要继续跑。
pub fn run(args: &[String]) {
    let dry_run = args.iter().any(|a| a == "--dry-run" || a == "--print");

    // 所有可能出错的事情都收在这里，外层不关心结果 ——
    // 唯一不能少的是下面那句 decision
    if let Err(e) = handle(dry_run) {
        log(&format!("agy 钩子处理失败: {e}"));
    }

    // stdout 只允许这一行。
    //
    // 用 writeln! 而不是 println!：后者在句柄无效时会 panic，
    // 而 panic 的话进程直接 abort，Antigravity 连 decision 都拿不到。
    // 写失败也没别的办法，忽略即可。
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{{\"decision\":\"stop\"}}");
    let _ = out.flush();
}

fn handle(dry_run: bool) -> Result<(), String> {
    let raw = read_stdin();
    if raw.trim().is_empty() {
        log("agy: stdin 为空，跳过发送");
        return Ok(());
    }

    let Some(ev) = parse(&raw) else {
        log(&format!("agy: 事件解析失败，前 200 字: {}", first_chars(&raw, 200)));
        return Ok(());
    };

    let cfg = Config::load();
    let agy = cfg.agy_config();

    if !cfg.enabled || !agy.enabled {
        log("agy: enabled = false，跳过发送");
        return Ok(());
    }

    // 这次 Stop 到底是「收工」还是「喘口气」，先问清楚再决定发不发。
    let kind = ev.kind();
    let verdict = verdict_for(&ev, &agy);

    log(&format!(
        "agy: 会话 {} · {} · 原因 {} · 类别 {} · 判定 {:?}",
        short_id(&ev.conversation_id),
        ev.model_name,
        ev.termination_reason.as_deref().unwrap_or("(未提供)"),
        kind.label(),
        verdict
    ));

    // 这一类事件被关掉了：不发，也不必挂起。
    // 顺手把同一会话的待复查撤掉 —— 后来这次 Stop 已经取代它了。
    if !wants(&agy, kind) {
        log(&format!("agy: {} 这一类没开通知，跳过", kind.label()));
        if !dry_run {
            forget_retry(&ev.conversation_id);
        }
        return Ok(());
    }

    if let Verdict::Busy(why) = verdict {
        if dry_run {
            eprintln!("（判定还没收工：{why}，正式运行时这次不发，等安静下来再补）");
        } else {
            log(&format!("agy: 还没收工（{why}），先不发"));
            // 挂不上（托盘没开 / 写不了盘）就还是发出去，
            // 措辞换成「等待中」，别让这条彻底消失。
            // 这类占位通知默认关掉，由 notify_pending 控制。
            if !schedule_retry(raw.trim()) {
                if agy.notify_pending {
                    let msg = crate::message::compose_agy_pending(&ev, &cfg, &agy);
                    crate::deliver(&cfg, &msg, ev.cwd());
                } else {
                    log("agy: 托盘没在运行，没人复查，且 notify_pending = false，这次不发");
                }
            }
        }
        return Ok(());
    }

    let msg = crate::message::compose_agy(&ev, &cfg, &agy);

    if dry_run {
        // 调试信息只能走 stderr：stdout 被 decision 占了
        eprintln!("--- 标题 ---\n{}", msg.title);
        eprintln!("--- 正文 ---\n{}", msg.body);
        return Ok(());
    }

    crate::deliver(&cfg, &msg, ev.cwd());

    // 真发过了就把待复查的那份撤掉，免得后台复查再补一条重复的
    forget_retry(&ev.conversation_id);
    Ok(())
}

// ---- 还没收工时：挂起，等托盘复查 ----

/// 日志安静多久才算「停下来了」。
///
/// 15 秒能跨过「工具返回 → 模型下一句」的间隙，又不至于让人干等。
const QUIET_SECS: u64 = 15;

/// 待复查事件的存放位置：`<data>/agy-wait/<会话id>.json`。
///
/// 一个会话只留一份，后来的 Stop 直接覆盖。判断全靠文件 mtime 推，
/// 不依赖内存状态 —— 托盘中途重启也不会乱。
fn pending_dir() -> PathBuf {
    crate::config::data_dir().join("agy-wait")
}

fn pending_path(conv_id: &str) -> PathBuf {
    pending_dir().join(format!("{conv_id}.json"))
}

fn forget_retry(conv_id: &str) {
    let id = conv_id.trim();
    if id.is_empty() {
        return;
    }
    let _ = fs::remove_file(pending_path(id));
}

/// 把这次事件挂起，等托盘过一会儿复查。
///
/// 返回 `false` 表示没挂上（托盘没开 / 写不了盘），由调用方退回「直接发」。
fn schedule_retry(raw: &str) -> bool {
    let Some(ev) = parse(raw) else {
        return false;
    };
    let id = ev.conversation_id.trim();
    if id.is_empty() {
        return false;
    }

    // 托盘不在就没人复查。这时候扣着不发等于漏报，
    // 不如立刻发出去 —— 至少让人知道有进展，点开看一眼即可。
    if !tray_alive() {
        log("agy: 托盘没在运行，没人复查，没法挂起");
        return false;
    }

    if fs::create_dir_all(pending_dir()).is_err() {
        return false;
    }
    if fs::write(pending_path(id), raw).is_err() {
        log("agy: 写挂起文件失败");
        return false;
    }
    true
}

/// 托盘每 15 秒调一次：把挂起的事件过一遍，真的安静了才发。
///
/// 为什么放在托盘里而不是钩子里 fork 一个后台进程：
/// 钩子是被宿主同步拉起的，进程一退出，它派生的子进程会被
/// 作业对象一起收走 —— 实测子进程能启动，但活不过父进程。
pub fn poll_pending() {
    let cfg = Config::load();
    let agy = cfg.agy_config();
    if !cfg.enabled || !agy.enabled || !agy.only_when_finished {
        return;
    }

    let Ok(entries) = fs::read_dir(pending_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        check_pending(&path, &cfg, &agy);
    }
}

fn check_pending(path: &Path, cfg: &Config, agy: &Agy) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Some(ev) = parse(&text) else {
        let _ = fs::remove_file(path);
        return;
    };

    let Ok(pending_at) = fs::metadata(path).and_then(|m| m.modified()) else {
        return;
    };
    let transcript_at = fs::metadata(expand_home(ev.transcript_path.trim()))
        .and_then(|m| m.modified())
        .ok();

    let age = SystemTime::now()
        .duration_since(pending_at)
        .unwrap_or_default();

    // 放太久了：多半是当时托盘不在，现在早过劲了，补发只会误导
    if age.as_secs() > agy.wait_seconds.saturating_mul(2).max(1200) {
        log(&format!("agy: 挂起事件已过期（{}s），清理", age.as_secs()));
        let _ = fs::remove_file(path);
        return;
    }

    // Stop 之后日志有没有再动过
    let grew = transcript_at.map(|t| t > pending_at).unwrap_or(false);
    // 现在安不安静：最后一条记录至少 QUIET_SECS 秒没更新
    let quiet = transcript_at
        .map(|t| {
            SystemTime::now()
                .duration_since(t)
                .map(|d| d.as_secs() >= QUIET_SECS)
                .unwrap_or(true)
        })
        .unwrap_or(true);

    let tail = tail_state(&ev.transcript_path);
    // fullyIdle = false 的那次，必须看到它真的动过才认，否则可能只是
    // 后台命令还在跑、日志一直不动
    let ready = quiet
        && matches!(tail, Tail::Spoken | Tail::Unknown)
        && (ev.fully_idle != Some(false) || grew);

    if ready {
        let _ = fs::remove_file(path);
        // 复查期间配置可能被改过，这里再问一次
        if !wants(agy, ev.kind()) {
            log("agy: 复查确认收工，但这一类没开通知，丢掉");
            return;
        }
        log("agy: 复查确认收工，补发");
        let msg = crate::message::compose_agy(&ev, cfg, agy);
        crate::deliver(cfg, &msg, ev.cwd());
        return;
    }

    if age.as_secs() >= agy.wait_seconds {
        let _ = fs::remove_file(path);
        if agy.notify_pending {
            log("agy: 复查超时，改发「等待中」");
            let msg = crate::message::compose_agy_pending(&ev, cfg, agy);
            crate::deliver(cfg, &msg, ev.cwd());
        } else {
            log("agy: 复查超时，notify_pending = false，丢掉");
        }
    }
}

/// 托盘进程在不在跑。
///
/// 它启动时创建这个命名互斥体，钩子这边试着打开一下就知道。
/// 用 `Local\` 而不是 `Global\`：两边总是同一个登录会话，够用，
/// 也不需要额外权限。
#[cfg(windows)]
fn tray_alive() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::OpenMutexW;

    const SYNCHRONIZE: u32 = 0x0010_0000;

    let name = wide("Local\\codex-notify-tray");
    unsafe {
        let h = OpenMutexW(SYNCHRONIZE, 0, name.as_ptr());
        if h.is_null() {
            false
        } else {
            let _ = CloseHandle(h);
            true
        }
    }
}

#[cfg(not(windows))]
fn tray_alive() -> bool {
    false
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

// ---- 安装 hook ----

/// `--init-agy`：把 Stop 钩子写进 Antigravity 的全局 hooks.json。
///
/// 只动 `codex-notify` 这一个键，其他钩子原样保留；已有同名键时
/// 只替换 `Stop`，不碰它下面的其它事件。
pub fn init_hook() {
    let Some(home) = user_home() else {
        println!("找不到用户目录（USERPROFILE / HOME 都为空）");
        return;
    };
    let path = home.join(".gemini").join("config").join("hooks.json");

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            println!("取不到自身路径: {e}");
            return;
        }
    };
    // JSON 里用正斜杠：反斜杠要么转义成 \\，要么被 JSON 吞掉，
    // 正斜杠在 Windows 上照样能执行，最省事
    let exe = exe.display().to_string().replace('\\', "/");
    let command = format!("\"{exe}\" --agy");

    let mut root: serde_json::Value = if path.exists() {
        match fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        {
            Some(v) if v.is_object() => v,
            _ => {
                println!(
                    "{} 不是合法的 JSON 对象，未改动。\n请手工把这段加进去：\n{}",
                    path.display(),
                    hook_snippet(&command)
                );
                return;
            }
        }
    } else {
        serde_json::json!({})
    };

    let obj = root.as_object_mut().expect("上面已经确认是对象");
    let entry = obj
        .entry("codex-notify".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    entry["Stop"] = serde_json::json!([{
        "type": "command",
        "command": command,
        "timeout": 15
    }]);

    if let Some(dir) = path.parent() {
        if let Err(e) = fs::create_dir_all(dir) {
            println!("创建目录失败 {}: {e}", dir.display());
            return;
        }
    }

    let text = match serde_json::to_string_pretty(&root) {
        Ok(t) => t,
        Err(e) => {
            println!("序列化失败: {e}");
            return;
        }
    };
    match fs::write(&path, text + "\n") {
        Ok(_) => {
            println!("已安装 Antigravity Stop 钩子: {}", path.display());
            println!("命令: {command}");
            println!("重启 Antigravity（或重开会话）后生效；用 --agy --dry-run 可以先本地验一遍。");
        }
        Err(e) => println!("写入失败 {}: {e}", path.display()),
    }
}

/// 给「文件坏了、改不了」时打印的手工配置片段。
fn hook_snippet(command: &str) -> String {
    format!(
        r#"{{
  "codex-notify": {{
    "Stop": [
      {{ "type": "command", "command": {}, "timeout": 15 }}
    ]
  }}
}}"#,
        serde_json::Value::String(command.to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stop_payload() {
        let raw = r#"{
            "executionNum": 1,
            "terminationReason": "model_stop",
            "error": "",
            "fullyIdle": true,
            "conversationId": "11111111-1111-4111-8111-111111111111",
            "workspacePaths": ["D:\\repo\\demo"],
            "transcriptPath": "C:/Users/x/.gemini/antigravity/brain/ec33/brain/logs/transcript.jsonl",
            "artifactDirectoryPath": "C:/Users/x/.gemini/antigravity/brain/ec33",
            "modelName": "gemini-3.6-flash-medium",
            "lastUserInput": "<USER_REQUEST>\n修一下测试\n</USER_REQUEST>\n<ADDITIONAL_METADATA>now</ADDITIONAL_METADATA>"
        }"#;
        let ev = parse(raw).unwrap();
        assert_eq!(ev.cwd(), r"D:\repo\demo");
        assert_eq!(ev.model_name, "gemini-3.6-flash-medium");
        assert!(!ev.is_error());
        assert_eq!(ev.outcome().0, "任务完成");
        assert_eq!(clean_input(ev.last_user_input.as_deref().unwrap()), "修一下测试");
    }

    #[test]
    fn error_reason_wins() {
        let mut ev = Event {
            termination_reason: Some("error".into()),
            ..Default::default()
        };
        assert!(ev.is_error());
        assert_eq!(ev.outcome().0, "任务出错");

        // 只有 error 字段、没有 reason 也算出错
        ev = Event {
            error: Some("API error (attempt 7)".into()),
            ..Default::default()
        };
        assert!(ev.is_error());

        ev = Event {
            termination_reason: Some("max_steps_exceeded".into()),
            ..Default::default()
        };
        assert!(!ev.is_error());
        assert_eq!(ev.outcome().0, "任务中止");
    }

    #[test]
    fn parses_pbtxt_title() {
        let text = r#"title:"Refactor auth module" last_user_view_time:{seconds:1795072744} permission_preset:AGENT_PERMISSION_PRESET_TURBO"#;
        assert_eq!(
            parse_title(text).as_deref(),
            Some("Refactor auth module")
        );
        assert_eq!(parse_title("last_user_view_time:{}"), None);
        // 带转义的引号
        assert_eq!(parse_title(r#"title:"a\"b""#).as_deref(), Some(r#"a"b"#));
        // 空标题当没有
        assert_eq!(parse_title(r#"title:"""#), None);
    }

    #[test]
    fn clips_unwrapped_input() {
        assert_eq!(clean_input("  直接的问题  "), "直接的问题");
        assert_eq!(clean_input("<USER_REQUEST>做 A</USER_REQUEST>"), "做 A");
    }

    /// 造一个临时 transcript，返回路径。
    fn tmp_transcript(name: &str, lines: &[&str]) -> String {
        let path = std::env::temp_dir().join(format!("agy-test-{name}.jsonl"));
        fs::write(&path, lines.join("\n")).unwrap();
        path.display().to_string()
    }

    #[test]
    fn reads_tail_state() {
        // 工具刚返回：下一步还没开始
        let p = tmp_transcript(
            "busy-tool",
            &[
                r#"{"type":"PLANNER_RESPONSE","tool_calls":[{"name":"run"}]}"#,
                r#"{"type":"GENERIC","content":"ok"}"#,
            ],
        );
        assert_eq!(tail_state(&p), Tail::Busy);

        // 还在调工具
        let p = tmp_transcript(
            "busy-call",
            &[r#"{"type":"PLANNER_RESPONSE","tool_calls":[{"name":"run"}]}"#],
        );
        assert_eq!(tail_state(&p), Tail::Busy);

        // 话说完了，没跟工具调用
        let p = tmp_transcript(
            "spoken",
            &[r#"{"type":"PLANNER_RESPONSE","content":"做完了"}"#],
        );
        assert_eq!(tail_state(&p), Tail::Spoken);

        // 空数组 / 空串都算「没有调用」
        let p = tmp_transcript(
            "spoken-empty",
            &[r#"{"type":"PLANNER_RESPONSE","tool_calls":[],"content":"好"}"#],
        );
        assert_eq!(tail_state(&p), Tail::Spoken);
        let p = tmp_transcript(
            "spoken-empty-str",
            &[r#"{"type":"PLANNER_RESPONSE","tool_calls":"[]","content":"好"}"#],
        );
        assert_eq!(tail_state(&p), Tail::Spoken);

        // 读不到就是 Unknown，不能因此静默
        assert_eq!(tail_state(""), Tail::Unknown);
        assert_eq!(tail_state(r"D:\definitely\not\here.jsonl"), Tail::Unknown);
        let p = tmp_transcript("empty", &[]);
        assert_eq!(tail_state(&p), Tail::Unknown);
    }

    #[test]
    fn fully_idle_false_means_busy() {
        let ev = Event {
            fully_idle: Some(false),
            ..Default::default()
        };
        assert_eq!(verdict(&ev), Verdict::Busy("后台任务还在运行"));

        // 没有这个字段时看日志尾巴
        let ev = Event {
            transcript_path: String::new(),
            ..Default::default()
        };
        assert_eq!(verdict(&ev), Verdict::Done);
    }

    #[test]
    fn error_reason_is_case_insensitive() {        // 2.x 实际下发的是大写
        let ev = Event {
            termination_reason: Some("ERROR".into()),
            ..Default::default()
        };
        assert!(ev.is_error());
        assert_eq!(ev.outcome().0, "任务出错");

        let ev = Event {
            termination_reason: Some("MAX_STEPS_EXCEEDED".into()),
            ..Default::default()
        };
        assert!(!ev.is_error());
        assert_eq!(ev.outcome().0, "任务中止");

        // 大小写混写的老格式也认
        let ev = Event {
            termination_reason: Some("max_steps_exceeded".into()),
            ..Default::default()
        };
        assert_eq!(ev.outcome().0, "任务中止");
    }

    #[test]
    fn kind_covers_three_cases() {
        let mk = |reason: Option<&str>, error: Option<&str>| Event {
            termination_reason: reason.map(String::from),
            error: error.map(String::from),
            ..Default::default()
        };

        assert_eq!(mk(None, None).kind(), Kind::Done);
        assert_eq!(mk(Some("NO_TOOL_CALL"), None).kind(), Kind::Done);
        assert_eq!(mk(Some("model_stop"), None).kind(), Kind::Done);
        assert_eq!(mk(Some("MAX_STEPS_EXCEEDED"), None).kind(), Kind::Aborted);
        assert_eq!(mk(Some("ERROR"), None).kind(), Kind::Error);
        // 只有 error 字段也算出错
        assert_eq!(mk(None, Some("boom")).kind(), Kind::Error);
    }

    #[test]
    fn error_is_never_held_back() {
        // 出错时日志最后一条常是 ERROR_MESSAGE，按尾巴判是 Busy；
        // 但出错是终态，必须当场发，不能被「还没收工」扣住
        let p = tmp_transcript("error-tail", &[r#"{"type":"ERROR_MESSAGE","content":"boom"}"#]);
        let ev = Event {
            termination_reason: Some("ERROR".into()),
            transcript_path: p,
            fully_idle: Some(false),
            ..Default::default()
        };
        assert_eq!(verdict(&ev), Verdict::Busy("后台任务还在运行"));
        assert_eq!(verdict_for(&ev, &Agy::default()), Verdict::Done);

        // 普通「完成」还是照常筛
        let ev = Event {
            termination_reason: Some("NO_TOOL_CALL".into()),
            fully_idle: Some(false),
            ..Default::default()
        };
        assert_eq!(verdict_for(&ev, &Agy::default()), Verdict::Busy("后台任务还在运行"));
    }

    #[test]
    fn notify_defaults_only_cover_done_and_error() {
        let agy = Agy::default();
        assert!(wants(&agy, Kind::Done));
        assert!(wants(&agy, Kind::Error));
        assert!(!wants(&agy, Kind::Aborted));

        let all_on = Agy {
            notify_aborted: true,
            notify_pending: true,
            ..Agy::default()
        };
        assert!(wants(&all_on, Kind::Aborted));

        let quiet = Agy {
            notify_done: false,
            notify_error: false,
            ..Agy::default()
        };
        assert!(!wants(&quiet, Kind::Done));
        assert!(!wants(&quiet, Kind::Error));
    }
}
