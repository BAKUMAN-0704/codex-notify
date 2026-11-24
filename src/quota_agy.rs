//! Antigravity 的额度查询。
//!
//! 配额只在 Google 服务端，本地没有快照 —— 这一点和 Codex 不同，
//! 后者会把 rate limit 写进会话日志。Antigravity 的登录态存在
//! Windows 凭据管理器里（target = `gemini:antigravity`），里面是
//! Google 的 access_token；拿它去调 `retrieveUserQuotaSummary`，
//! 就能拿到和界面上那两条一致的额度。
//!
//! 这条路依赖 Antigravity 的私有接口和凭据存储方式，任何一环变了
//! 都只影响这一行字：查不到就当没有，既不影响通知，也不报错。
//!
//! 为什么用 WinHTTP 而不是 ureq：本程序的 ureq 关掉了 TLS 特性
//! （只访问本机 NapCat），为了一个 HTTPS 请求把 rustls/ring 整坨
//! 依赖拖进来，二进制会从 1 MB 涨到 2 MB 以上。WinHTTP 走系统
//! TLS，零新增依赖，体积几乎不变。

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;

use serde::Deserialize;
use windows_sys::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE,
};
use windows_sys::Win32::Security::Credentials::{
    CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
};

/// Antigravity 的配额接口。`daily-` 前缀是它的日常环境，
/// 和 CLI 日志里出现过的地址一致。
const HOST: &str = "daily-cloudcode-pa.googleapis.com";
const PATH: &str = "/v1internal:retrieveUserQuotaSummary";

/// 登录态在凭据管理器里的 target 名。
const CRED_TARGET: &str = "gemini:antigravity";

/// 缓存有效期（秒）。额度以分钟为单位变化，没必要每个回合都去问。
const CACHE_TTL: i64 = 60;

/// 单次请求的超时（毫秒）。钩子总共只有 15 秒预算，
/// 这里必须留足余量 —— 查不到就不显示，不能拖慢通知。
const TIMEOUT_MS: i32 = 3500;

/// 一个额度条。字段名跟接口的 camelCase 对齐。
#[derive(Deserialize, serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    #[serde(default)]
    pub window: String,
    /// 剩余比例，1.0 表示满额
    #[serde(default)]
    pub remaining_fraction: f64,
    /// ISO8601 的重置时刻，例如 `2026-11-24T12:18:10Z`
    #[serde(default)]
    pub reset_time: String,
}

#[derive(Deserialize)]
struct Group {
    #[serde(default)]
    buckets: Vec<Bucket>,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    groups: Vec<Group>,
}

/// 缓存文件的结构。只存原始数值，不存格式化结果 ——
/// 显示样式（已用/剩余、要不要带重置时间）改了不用等缓存过期。
#[derive(serde::Serialize, Deserialize)]
struct Cache {
    at: i64,
    buckets: Vec<Bucket>,
}

/// 拿额度，返回形如 `5h 剩 92% · 周 剩 41%` 的一段文字。
///
/// 任何一步失败都返回 `None` —— 调用方直接跳过这一段，
/// 消息照发。
pub fn segment(show_remaining: bool) -> Option<String> {
    let buckets = load(show_remaining)?;
    if buckets.is_empty() {
        return None;
    }
    Some(buckets.join(" · "))
}

/// 取当前额度条文字，带一层内存外的磁盘缓存。
fn load(show_remaining: bool) -> Option<Vec<String>> {
    let now = crate::clock::now_unix();
    let path = cache_path();

    if let Some(c) = read_cache(&path) {
        if now - c.at < CACHE_TTL {
            return Some(format_buckets(&c.buckets, show_remaining));
        }
    }

    match fetch() {
        Ok(buckets) => {
            write_cache(&path, &Cache { at: now, buckets: buckets.clone() });
            Some(format_buckets(&buckets, show_remaining))
        }
        // 请求失败就退回旧缓存（哪怕过期），总比突然不显示强；
        // 连缓存都没有就静默跳过。
        Err(e) => {
            crate::logfile::log(&format!("agy: 额度查询失败（{e}），跳过这一段"));
            read_cache(&path).map(|c| format_buckets(&c.buckets, show_remaining))
        }
    }
}

/// 把 bucket 列表压成 `5h 剩 92%` 这样的短句。
///
/// 同一个窗口可能出现在多个分组里（比如 Gemini 一组、Claude/GPT 一组），
/// 这时取**最紧的那个**：用户想知道的是「还能撑多久」，不是平均值。
fn format_buckets(buckets: &[Bucket], show_remaining: bool) -> Vec<String> {
    // 按窗口归并，保留最紧的剩余比例
    let mut order: Vec<String> = Vec::new();
    let mut tightest: Vec<f64> = Vec::new();

    for b in buckets {
        let label = window_label(&b.window);
        if label.is_empty() {
            continue;
        }
        match order.iter().position(|w| *w == b.window) {
            Some(i) => {
                if b.remaining_fraction < tightest[i] {
                    tightest[i] = b.remaining_fraction;
                }
            }
            None => {
                order.push(b.window.clone());
                tightest.push(b.remaining_fraction);
            }
        }
    }

    // 5h 在前、周在后：短窗口先耗尽，读的人最关心它
    let mut idx: Vec<usize> = (0..order.len()).collect();
    idx.sort_by_key(|&i| window_rank(&order[i]));

    idx.into_iter()
        .map(|i| {
            let remaining = (tightest[i] * 100.0).clamp(0.0, 100.0);
            let label = window_label(&order[i]);
            if show_remaining {
                format!("{label} 剩 {remaining:.0}%")
            } else {
                format!("{label} 已用 {:.0}%", 100.0 - remaining)
            }
        })
        .collect()
}

/// `weekly` → `周`，`5h` → `5h`。
fn window_label(window: &str) -> String {
    match window.to_ascii_lowercase().as_str() {
        "weekly" | "week" => "周".to_string(),
        "5h" | "five_hour" | "fivehour" => "5h".to_string(),
        other => other.to_string(),
    }
}

/// 排序权重：越短的时间窗口越靠前。
fn window_rank(window: &str) -> u8 {
    match window.to_ascii_lowercase().as_str() {
        "5h" | "five_hour" | "fivehour" => 0,
        "weekly" | "week" => 2,
        _ => 1,
    }
}

fn cache_path() -> PathBuf {
    crate::config::data_dir().join("agy-quota.json")
}

fn read_cache(path: &PathBuf) -> Option<Cache> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cache(path: &PathBuf, cache: &Cache) {
    if let Ok(text) = serde_json::to_string(cache) {
        let _ = std::fs::write(path, text);
    }
}

/// 读凭据管理器里的 access_token。
fn read_token() -> Option<String> {
    unsafe {
        let target = wide(CRED_TARGET);
        let mut raw: *mut CREDENTIALW = ptr::null_mut();
        if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) == 0 {
            return None;
        }
        let cred = &*raw;
        let blob = std::slice::from_raw_parts(
            cred.CredentialBlob,
            cred.CredentialBlobSize as usize,
        );
        let text = String::from_utf8_lossy(blob).to_string();
        CredFree(raw as *const c_void);

        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        v.get("token")?
            .get("access_token")?
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
}

/// 调一次配额接口。返回 `groups[].buckets[]` 拍平后的列表。
fn fetch() -> Result<Vec<Bucket>, String> {
    let token = read_token().ok_or("读不到 Antigravity 登录态")?;
    let body = br#"{}"#;

    let text = unsafe {
        // User-Agent 必须写成 `antigravity`：这个接口会校验客户端，
        // 换成自己的名字会直接 403「You do not have a valid license」。
        // WinHttpOpen 的 agent 参数就是 UA，所以在这里设。
        let agent = wide("antigravity");
        // AUTOMATIC_PROXY 会沿用系统代理设置（含 PAC），
        // 用户开着代理时才连得上 Google。
        let session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            ptr::null(),
            ptr::null(),
            0,
        );
        if session.is_null() {
            return Err("WinHttpOpen 失败".into());
        }
        let session = Handle(session);

        WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS);

        let host = wide(HOST);
        let connect = WinHttpConnect(session.0, host.as_ptr(), 443, 0);
        if connect.is_null() {
            return Err("WinHttpConnect 失败".into());
        }
        let connect = Handle(connect);

        let verb = wide("POST");
        let path = wide(PATH);
        let request = WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            path.as_ptr(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if request.is_null() {
            return Err("WinHttpOpenRequest 失败".into());
        }
        let request = Handle(request);

        let headers = format!(
            "Authorization: Bearer {token}\r\nContent-Type: application/json\r\n"
        );
        let headers = wide(&headers);

        if WinHttpSendRequest(
            request.0,
            headers.as_ptr(),
            u32::MAX, // 表示以 NUL 结尾
            body.as_ptr() as *const c_void,
            body.len() as u32,
            body.len() as u32,
            0,
        ) == 0
        {
            return Err("WinHttpSendRequest 失败".into());
        }

        if WinHttpReceiveResponse(request.0, ptr::null_mut()) == 0 {
            return Err("WinHttpReceiveResponse 失败".into());
        }

        // 状态码决定是解析还是报错
        let mut status: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            ptr::null(),
            &mut status as *mut u32 as *mut c_void,
            &mut len,
            ptr::null_mut(),
        );

        let mut out: Vec<u8> = Vec::new();
        loop {
            let mut buf = [0u8; 4096];
            let mut read: u32 = 0;
            if WinHttpReadData(
                request.0,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                &mut read,
            ) == 0
            {
                break;
            }
            if read == 0 {
                break;
            }
            out.extend_from_slice(&buf[..read as usize]);
        }

        if status != 200 {
            // 把服务端的说明带出来：403 可能是「没有许可」，
            // 401 是 token 过期，两者处理方式完全不同，日志里得分得清。
            let body = String::from_utf8_lossy(&out).replace(['\n', '\r'], " ");
            let brief: String = body.chars().take(160).collect();
            return Err(format!("HTTP {status}: {brief}"));
        }
        String::from_utf8_lossy(&out).to_string()
    };

    let resp: Response = serde_json::from_str(&text).map_err(|e| format!("解析失败: {e}"))?;
    Ok(resp.groups.into_iter().flat_map(|g| g.buckets).collect())
}

/// HINTERNET 是裸指针，手工包一层保证异常路径也能关掉。
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(window: &str, frac: f64) -> Bucket {
        Bucket {
            window: window.to_string(),
            remaining_fraction: frac,
            reset_time: String::new(),
        }
    }

    #[test]
    fn formats_5h_before_weekly() {
        let got = format_buckets(&[b("weekly", 0.41), b("5h", 0.92)], true);
        assert_eq!(got, vec!["5h 剩 92%", "周 剩 41%"]);
    }

    #[test]
    fn shows_used_when_asked() {
        let got = format_buckets(&[b("5h", 0.88)], false);
        assert_eq!(got, vec!["5h 已用 12%"]);
    }

    /// 两个分组各有一条 5h 时，取更紧的那个
    #[test]
    fn keeps_tightest_across_groups() {
        let got = format_buckets(&[b("5h", 1.0), b("5h", 0.3), b("weekly", 0.9)], true);
        assert_eq!(got, vec!["5h 剩 30%", "周 剩 90%"]);
    }

    #[test]
    fn ignores_empty_window() {
        assert!(format_buckets(&[b("", 0.5)], true).is_empty());
    }
}
