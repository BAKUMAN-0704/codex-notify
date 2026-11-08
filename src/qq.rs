//! QQ 通道：走 OneBot v11 的 HTTP 接口（由 NapCat / LLOneBot 提供）。
//!
//! 关键认知：本程序**不实现** QQ 协议，也不会直接连腾讯服务器。
//! 它只是往本机 NapCat 的 HTTP 接口发一条 POST，
//! 由 NapCat（用你扫码登录的那个 QQ 号）把消息真正发出去。
//!
//! 所以整条链路是：
//!   codex-notify → 127.0.0.1:3000（NapCat）→ 腾讯 → 你的手机
//!
//! 接口约定（OneBot v11）：
//!   私聊  POST /send_private_msg  {"user_id": 123, "message": "文本"}
//!   群聊  POST /send_group_msg    {"group_id": 456, "message": "文本"}
//!   鉴权  请求头 Authorization: Bearer <access_token>
//!   成功  {"status":"ok","retcode":0,"data":{...}}

use std::time::Duration;

use serde_json::{json, Value};

use crate::config::Qq;
use crate::text::clip;

/// 发送一条文本消息。
///
/// 只发纯文本，不用 CQ 码，所以不会涉及图片/at/@ 之类的能力。
/// 需要的话可以在 `message` 里直接写 CQ 码，OneBot 会解析。
pub fn send(qq: &Qq, text: &str, timeout: Duration) -> Result<(), String> {
    // target 形如 "private:123456789" 或 "group:987654321"，
    // 冒号前是类型，冒号后是号码。
    let (kind, id) = qq.target.split_once(':').ok_or_else(|| {
        format!(
            "target 格式应为 private:QQ号 或 group:群号，当前是 {:?}",
            qq.target
        )
    })?;

    // 类型决定调哪个接口、以及请求体里 ID 字段叫什么
    let (endpoint, id_field) = match kind.trim() {
        "private" => ("send_private_msg", "user_id"),
        "group" => ("send_group_msg", "group_id"),
        other => return Err(format!("不支持的 target 类型: {other}")),
    };

    let id = id.trim();
    // 纯数字按数字发（标准做法）；不是数字就退回字符串，
    // 兼容个别实现或测试环境用的非数字 ID。
    let id_value: Value = id
        .parse::<i64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::String(id.to_string()));

    let url = format!("{}/{endpoint}", qq.base_url.trim_end_matches('/'));
    let body = json!({ id_field: id_value, "message": text });

    let raw = post_json(&url, &qq.access_token, &body, timeout)?;

    // 判成功的规则：retcode == 0（OneBot 标准）或者 status == "ok"（部分实现）。
    // 解析不出 JSON 时按成功处理——HTTP 层都 200 了，多半只是返回体不规范，
    // 不值得因此判失败。
    let ok = serde_json::from_str::<Value>(&raw)
        .ok()
        .map(|v| {
            v.get("retcode").and_then(Value::as_i64).unwrap_or(0) == 0
                || v.get("status").and_then(Value::as_str) == Some("ok")
        })
        .unwrap_or(true);

    if ok {
        Ok(())
    } else {
        Err(format!("服务端返回异常: {}", clip(&raw, 200)))
    }
}

/// `--check` 用：探查 NapCat 是否在线，并顺带拿到当前登录的 QQ 号。
///
/// 选 `get_login_info` 这个接口，是因为它不需要任何参数、
/// 又能同时证明两件事：HTTP 服务开着、且已经登录成功。
pub fn probe(qq: &Qq, timeout: Duration) -> Result<String, String> {
    let url = format!("{}/get_login_info", qq.base_url.trim_end_matches('/'));
    get(&url, &qq.access_token, timeout)
}

/// 发一个 JSON POST，返回响应体文本。
fn post_json(
    url: &str,
    token: &str,
    body: &Value,
    timeout: Duration,
) -> Result<String, String> {
    let mut req = ureq::post(url)
        .timeout(timeout)
        .set("Content-Type", "application/json");

    // token 只在配置了才带。空字符串会让某些实现认为「有鉴权头但值是错的」。
    if !token.trim().is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", token.trim()));
    }

    match req.send_string(&body.to_string()) {
        Ok(resp) => Ok(resp.into_string().unwrap_or_default()),
        // 服务端返回了非 2xx：把响应体一起带上，方便判断是 token 错还是参数错
        Err(ureq::Error::Status(code, resp)) => {
            let raw = resp.into_string().unwrap_or_default();
            Err(format!("HTTP {code}: {}", clip(&raw, 200)))
        }
        // 连接层面的失败：NapCat 没启动、端口不对、超时
        Err(e) => Err(format!("请求失败: {e}")),
    }
}

/// 发一个 GET，返回响应体文本。
fn get(url: &str, token: &str, timeout: Duration) -> Result<String, String> {
    let mut req = ureq::get(url)
        .timeout(timeout)
        .set("Content-Type", "application/json");

    if !token.trim().is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", token.trim()));
    }

    match req.call() {
        Ok(resp) => Ok(resp.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, resp)) => {
            let raw = resp.into_string().unwrap_or_default();
            Err(format!("HTTP {code}: {}", clip(&raw, 200)))
        }
        Err(e) => Err(format!("请求失败: {e}")),
    }
}
