//! 文本处理小工具：取项目名、按字符截断。
//!
//! 单独放一个模块，是因为这几件事既不属于「配置」也不属于「消息拼装」，
//! 但消息拼装和命令行的自检输出都要用。

/// 从工作目录里取最后一段作为「项目名」。
///
/// 为什么需要它：消息标题如果直接塞完整路径 `C:\Users\you\projects\demo`，
/// 手机上会折行成两三行，一眼看不出是哪个项目。取最后一段 `notify` 更清爽。
///
/// 同时兼容 Windows 的反斜杠和 POSIX 的正斜杠，末尾的分隔符先剃掉，
/// 这样 `D:\repo\app\` 也能正确得到 `app`。
/// 如果 cwd 只是个盘符或空串，就原样返回，不做多余处理。
pub fn project_name(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches(['\\', '/']);
    trimmed
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

/// 按「字符」截断文本，超长时补一个省略号。
///
/// 注意必须用 `chars()` 而不是字节切片：中文和 emoji 是多字节的，
/// 按字节切会把字符劈开、产生乱码甚至 panic。
/// 这也意味着 `max` 的单位是「字数」而非「字节数」。
pub fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if max == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(max).collect();
    // 只有确实被截断了才补省略号，避免正常结尾也多个点
    if text.chars().count() > max {
        out.push('…');
    }
    out
}
