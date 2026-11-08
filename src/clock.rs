//! 时间工具：只依赖 std，不引入 chrono。
//!
//! 为什么不用 chrono：本程序只需要「现在几点」和「今天是几号」，
//! 为了这点需求拉一个日期库不划算，还会拖慢编译。
//! 代价是时区写死 UTC+8 —— 对个人工具来说足够。
//!
//! 核心是一个经典的日历算法 `civil_from_days`（Howard Hinnant 提出），
//! 把「1970-01-01 起的天数」换算成年月日，纯整数运算，没有循环也没有查表。

/// 时区偏移：东八区。想换成别的时区，改这一个常量即可。
pub const TZ_OFFSET_SECS: u64 = 8 * 3600;

/// 取当前本地时间，拆成 `(年, 月, 日, 时, 分, 秒)`。
///
/// 一次取全再按需取用，避免调方各自读系统时钟导致跨秒不一致
/// （例如日志时间戳和消息里的时间相差一秒，排查时会误导）。
pub fn local_now() -> (i64, u32, u32, u64, u64, u64) {
    // 系统时钟被回拨到 1970 之前时 duration_since 会失败（罕见但存在），
    // 用 0 兜底：宁可显示成 1970，也不要 panic —— 钩子进程崩了会污染 Codex 的收尾。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        + TZ_OFFSET_SECS;

    // 拆成「整天数」和「当天的秒数」：前者算日期，后者算时分秒，互不干扰
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// 日志前缀用的时间戳，例如 `2026-11-08 11:37:31`。
/// 精确到秒：事后对着日志排查某一回合发生了什么时，秒级时间更好对齐。
pub fn timestamp() -> String {
    let (y, m, d, h, mi, s) = local_now();
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// 消息正文里用的时间，例如 `11:37`。
/// 只到分钟：手机通知上精确到秒没有意义，反而让这一行变长。
pub fn hhmm() -> String {
    let (_, _, _, h, mi, _) = local_now();
    format!("{h:02}:{mi:02}")
}

/// 当前的 Unix 时间戳（秒）。用来和额度里的 `resets_at` 做比较。
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 把额度重置时刻格式化成「18:20」「明天 03:00」「周四 09:00」。
///
/// 三档是有意分开的，因为读的人当下只关心「还有多久」：
///   - 今天：只要时分，一眼就知道几点回满
///   - 明天：加个「明天」，避免误以为是今天
///   - 更远：带星期几（周窗口通常跨好几天）
///
/// `now_secs` 作为参数传入而不是内部取，方便测试时构造固定时刻。
pub fn reset_label(resets_at: i64, now_secs: i64) -> String {
    let tz = TZ_OFFSET_SECS as i64;

    // 转成本地时间的「第几天」和「当天第几秒」
    let local = resets_at + tz;
    let days = local.div_euclid(86_400);
    let rem = local.rem_euclid(86_400);
    let time = format!("{:02}:{:02}", rem / 3600, (rem % 3600) / 60);

    let now_days = (now_secs + tz).div_euclid(86_400);
    let delta = days - now_days;
    match delta {
        0 => time,
        1 => format!("明天 {time}"),
        _ => {
            // 1970-01-01 是周四，所以 +4 再对 7 取模就得到星期索引（0 = 周日）
            const NAMES: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
            let weekday = (days + 4).rem_euclid(7) as usize;

            // 正好七天后是同一个星期几。不加区分的话，
            // 在周日当天会显示「周日 09:56 重置」，读起来像今天
            if delta == 7 {
                format!("下{} {time}", NAMES[weekday])
            } else {
                format!("{} {time}", NAMES[weekday])
            }
        }
    }
}

/// 把「1970-01-01 以来的天数」换算成 `(年, 月, 日)`。
///
/// 算法思路（Howard Hinnant 的 civil_from_days）：
///   1. 以 400 年为一个「纪元」—— 公历 400 年恰好一循环（含闰年规则），
///      所以先算出落在第几个纪元、以及纪元内的第几天；
///   2. 把 3 月当成一年的起点，这样闰日永远落在「年末」，
///      平年和闰年就能用同一套公式处理，不必写分支判断；
///   3. 最后把「3 月起算的月份」换回正常的 1~12 月。
///
/// 常数时间、没有边界问题，比循环逐年递减更可靠。
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    // 平移到「0000-03-01 起算」，让闰日落在年末
    let z = z + 719_468;

    // 第几个 400 年纪元。负数要向下取整，所以先减 146096 再除
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // 纪元内的第几天，范围 0..=146096

    // 纪元内的第几年：以 365 天为基数，每 4 年补 1 天，每 100 年减 1 天，每 400 年再补回
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // 年内的第几天，以 3 月 1 日为 0

    // 每 5 个月一组换算成「3 月起的月份」，2 月因为要容纳闰日所以落在最后
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // 日
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // 换回 1~12 月

    // 1、2 月被算成了「上一年」的年末，这里补回一年
    (if m <= 2 { y + 1 } else { y }, m, d)
}
