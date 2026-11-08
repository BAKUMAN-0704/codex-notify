//! 托盘常驻模式（`--tray`）与 Windows 控制台处理。
//!
//! # 为什么需要它
//!
//! 这个程序被编译成 **GUI 子系统**（见 main.rs 顶部的 `windows_subsystem`），
//! 好处是 Codex 每回合拉起它时不会再闪一个黑色控制台窗口。
//! 代价是：从终端里跑 `--check` 之类也不会自动有控制台，
//! 所以要靠 [`ensure_console`] 去「借」父进程的控制台。
//!
//! 托盘模式则提供一个看得见、关得掉的入口：
//! 双击 exe（或加 `--tray`）后常驻，右键出菜单，
//! 可以看 NapCat 在不在、发一条测试、打开配置目录、退出。
//!
//! # 实现说明
//!
//! 全程手写 Win32：注册一个隐藏窗口收消息 → `Shell_NotifyIconW` 挂图标 →
//! 右键时临时拼一个弹出菜单 → 处理完继续等消息。
//! 没引入任何 GUI 框架，因为这些 API 本身就很稳定，多一层封装反而不好排查。

use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::Console::{
    AttachConsole, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    ExtractIconExW, ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, KillTimer, LoadCursorW,
    PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer, TrackPopupMenu, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG,
    SM_CXSMICON, SW_SHOWNORMAL, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP,
    WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};

use crate::config::Config;

/// 托盘图标的回调消息号。`WM_APP` 之后的号段是留给应用自己用的。
const WM_TRAY: u32 = WM_APP + 1;

// 菜单项 ID。随便取，只要和 wndproc 里的分支对得上。
const ID_TEST: usize = 1;
const ID_OPEN_DIR: usize = 2;
const ID_QUIT: usize = 3;
const ID_START_NAPCAT: usize = 4;
const ID_STOP_NAPCAT: usize = 5;
// 设置子菜单里的勾选项
const ID_TOGGLE_LIMITS: usize = 10;
const ID_TOGGLE_RESET: usize = 11;
const ID_TOGGLE_REMAINING: usize = 12;
const ID_TOGGLE_FOOTER: usize = 13;
const ID_TOGGLE_ENABLED: usize = 14;
const ID_TOGGLE_KILL_EXIT: usize = 15;
const ID_CYCLE_MAX_CHARS: usize = 16;
const ID_CYCLE_TIMEOUT: usize = 17;
const ID_TOGGLE_MAIN_ONLY: usize = 18;
const ID_CYCLE_SUMMARY: usize = 19;
const ID_CYCLE_SHELL: usize = 20;

/// 摘要长度的档位。菜单没法输数字，用「点一下换下一档」代替输入框。
/// 260 是默认档；80 适合只想瞄一眼，600 基本是全文。
const MAX_CHARS_STEPS: [i64; 5] = [80, 160, 260, 400, 600];
/// HTTP 超时的档位（秒）。本机 NapCat 通常 5ms 就返回，档位只影响最坏情况。
const TIMEOUT_STEPS: [i64; 4] = [1, 3, 5, 10];
const SUMMARY_SOURCES: [&str; 3] = ["thread", "message", "none"];
const SHELL_STEPS: [&str; 3] = ["cmd", "pwsh", "pwsh7"];

/// 由本程序启动的 NapCat。放在静态里是为了让菜单（窗口过程）也能访问。
///
/// 它一旦被 `drop`，内部的 Job 句柄关闭，整棵 NapCat 进程树随之结束 ——
/// 所以「退出时带着 NapCat 退出」只是把这个 Option 置空而已。
static GUARD: std::sync::Mutex<Option<crate::napcat::Managed>> = std::sync::Mutex::new(None);

/// 把 `&str` 转成 Win32 需要的以 0 结尾的 UTF-16
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 从终端启动时，把父进程的控制台「接」过来。
///
/// GUI 子系统的进程默认没有控制台，`println!` 会静默丢弃。
/// `AttachConsole` 成功时，Windows 会把标准输入输出句柄指向父控制台，
/// 之后 Rust 的 stdout 就能正常打印了。
///
/// 失败是正常情况（比如被 Codex 或双击拉起，父进程根本没有控制台），
/// 所以这里不报错，直接忽略。
pub fn ensure_console() {
    unsafe {
        // 父进程没有控制台时这个调用会失败，返回值不用管
        AttachConsole(ATTACH_PARENT_PROCESS);

        // 保险起见把标准句柄重设一遍：某些宿主环境下 AttachConsole
        // 不会自动更新这些句柄，导致输出仍然丢失。
        let out = windows_sys::Win32::System::Console::GetStdHandle(STD_OUTPUT_HANDLE);
        let err = windows_sys::Win32::System::Console::GetStdHandle(STD_ERROR_HANDLE);
        if !out.is_null() {
            SetStdHandle(STD_OUTPUT_HANDLE, out);
        }
        if !err.is_null() {
            SetStdHandle(STD_ERROR_HANDLE, err);
        }
    }
}

/// 托盘模式会用到的那份配置。
///
/// 用 `OnceLock<RwLock<Config>>` 而不是 `OnceLock<Config>`：
/// 右键菜单里的「设置」要能改它 —— 勾一下立刻生效，不用重启托盘。
static CFG: std::sync::OnceLock<std::sync::RwLock<Config>> = std::sync::OnceLock::new();

/// 取配置的快捷方式。`run()` 一开始就初始化好了，所以这里不会失败。
fn cfg() -> &'static std::sync::RwLock<Config> {
    CFG.get().expect("配置应在 run() 开头初始化")
}

/// 取一份配置副本。
///
/// 为什么不直接持有读锁往下传：`std::sync::RwLock` 在 Windows 上用的是
/// SRW 锁，同线程递归获取读锁可能死锁。而这些函数内部还会再调
/// `napcat_online()` 之类的辅助函数（它们也要读配置），
/// 所以统一「先克隆、立刻放锁」，把锁的生命周期压到最短。
fn snapshot() -> Config {
    cfg()
        .read()
        .map(|c| c.clone())
        .unwrap_or_else(|_| Config::fallback())
}

/// 进入托盘模式，直到用户点「退出」才返回。
pub fn run(cfg: Config) {
    let _ = CFG.set(std::sync::RwLock::new(cfg));

    unsafe {
        let module = GetModuleHandleW(std::ptr::null());

        // 1. 注册窗口类。样式里的 CS_HREDRAW/CS_VREDRAW 其实用不上
        //    （窗口从头到尾不显示），但带着无害，等于告诉系统这是普通窗口类。
        let class_name = wide("codex_notify_tray");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        if RegisterClassW(&wc) == 0 {
            eprintln!("托盘：注册窗口类失败");
            return;
        }

        // 2. 建一个不显示的窗口，纯粹用来接收托盘消息
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide("codex-notify").as_ptr(),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            eprintln!("托盘：创建窗口失败");
            return;
        }

        // 3. 挂托盘图标。图标直接从自己的 exe 里取，
        //    这样和资源管理器里看到的完全一致，不用重复指定路径。
        let icon = exe_icon();
        // 该结构体没有 Default 实现，直接清零（所有字段的零值都是合法初始状态）
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = icon;
        write_tip(&mut nid, &tip_text());

        if Shell_NotifyIconW(NIM_ADD, &nid) == 0 {
            eprintln!("托盘：添加图标失败");
            DestroyWindow(hwnd);
            return;
        }

        crate::logfile::log("托盘已启动");

        // 4. 定时刷新托盘提示文字。
        //    之前只在创建图标时写一次，结果永远显示启动那一刻的状态
        //    （"未运行"），鼠标移上去看到的就是过期信息。
        //    5 秒一次：状态判断要跑 tasklist，太频繁没必要。
        const TIMER_REFRESH: usize = 1;
        SetTimer(hwnd, TIMER_REFRESH, 5000, None);

        // 5. 如果配了 NapCat 且它没在跑，就顺带拉起来。
        //    已经在跑（端口能连上）说明是你手动开的，不认识也不去接管。
        if let Some(nc) = snapshot().napcat {
            if !nc.command.is_empty() && !napcat_online() {
                start_napcat(
                    &nc.command,
                    nc.kill_on_exit,
                    crate::napcat::parse_shell(&nc.shell),
                );
            }
        }

        // 6. 标准消息循环。GetMessageW 返回 0 表示收到 WM_QUIT
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 6. 收尾。先结束托管的 NapCat（关 Job 句柄 → 整棵树一起走），
        //    再删托盘图标，否则鼠标划过托盘会留下一个「幽灵图标」。
        KillTimer(hwnd, TIMER_REFRESH);
        if let Ok(mut g) = GUARD.lock() {
            if g.take().is_some() {
                crate::logfile::log("已结束托管的 NapCat");
            }
        }
        Shell_NotifyIconW(NIM_DELETE, &nid);
        crate::logfile::log("托盘已退出");
    }
}

/// 刷新托盘提示文字。
///
/// 只带 `NIF_TIP` 调 `NIM_MODIFY`，其他字段不动 —— 这样图标和回调消息
/// 都不会被重置，改的只有鼠标悬停时显示的那句话。
unsafe fn refresh_tip(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_TIP;
    write_tip(&mut nid, &tip_text());
    Shell_NotifyIconW(NIM_MODIFY, &nid);
}

/// 把提示文字写进 NOTIFYICONDATAW 的定长缓冲区（最多 127 个 UTF-16 码元）
fn write_tip(nid: &mut NOTIFYICONDATAW, text: &str) {
    let mut buf: Vec<u16> = text.encode_utf16().take(127).collect();
    buf.push(0);
    nid.szTip[..buf.len()].copy_from_slice(&buf);
}

/// 菜单里那句灰色状态文字。
///
/// 三层判断，缺一层就会误导：
///   1. 端口通      → 真的登录好了，能发消息
///   2. 进程在但端口不通 → NapCat 起来了、卡在扫码登录。**不是"未运行"**
///   3. 都没有      → 确实没跑
fn status_text() -> &'static str {
    if napcat_online() {
        "状态：NapCat 在线"
    } else if napcat_managed() || napcat_process_alive() {
        "状态：NapCat 已启动，等待扫码登录"
    } else {
        "状态：NapCat 未运行"
    }
}

/// NapCat 的引导进程是否还活着。
///
/// 为什么不复用端口判断：未登录时 OneBot 的 HTTP 服务根本不启动，
/// 端口一定是关的。用户看到 QQ 窗口在，托盘却说"未运行"，就会觉得坏了。
fn napcat_process_alive() -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    std::process::Command::new("tasklist")
        .args([
            "/FI",
            "IMAGENAME eq NapCatWinBootMain.exe",
            "/NH",
            "/FO",
            "CSV",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout).contains("NapCatWinBootMain.exe")
        })
        .unwrap_or(false)
}

/// 当前是否持有托管句柄（不管端口通不通）
fn napcat_managed() -> bool {
    GUARD.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// 鼠标悬停在托盘图标上时的提示
fn tip_text() -> String {
    format!("codex-notify · {}", status_text().trim_start_matches("状态："))
}

/// 判断 NapCat 是否在监听。
///
/// 只做一次 TCP 连接试探，超时 300ms —— 菜单点开要立刻响应，
/// 不能在这里发 HTTP 请求（NapCat 没开时要等满 5 秒，界面会卡住）。
fn napcat_online() -> bool {
    let base = snapshot()
        .qq
        .map(|q| q.base_url)
        .unwrap_or_else(|| "http://127.0.0.1:3000".to_string());

    // 从 http://host:port 里抠出 host 和 port，抠不出来就按默认值试
    let addr = base
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let host_port = if addr.contains(':') {
        addr.to_string()
    } else {
        format!("{addr}:3000")
    };

    TcpStream::connect_timeout(
        &host_port.parse().unwrap_or_else(|_| "127.0.0.1:3000".parse().unwrap()),
        Duration::from_millis(300),
    )
    .is_ok()
}

/// 从自己的 exe 里取出小尺寸图标
fn exe_icon() -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
    unsafe {
        let path = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let w = wide(&path);
        let mut large = std::ptr::null_mut();
        let mut small = std::ptr::null_mut();

        // 0 表示取第一个图标组；只要小图标（托盘用）
        ExtractIconExW(w.as_ptr(), 0, &mut large, &mut small, 1);
        if !large.is_null() {
            // 大图标用不上，销毁掉避免泄漏
            windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon(large);
        }
        small
    }
}

/// 窗口过程：只处理托盘图标发来的消息
unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY => {
            // lParam 的低位是鼠标事件类型
            let event = (lparam as u32) & 0xFFFF;
            if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
                show_menu(hwnd);
            }
            0
        }
        // 定时刷新提示文字。菜单里那句状态是每次右键现算的，
        // 但鼠标悬停的 tooltip 不会自己变，只能靠这个定时器推。
        WM_TIMER => {
            refresh_tip(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 在鼠标位置弹出菜单，并根据选择执行动作
unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }

    // 先把配置拷一份出来：菜单项多，逐个读锁既啰嗦又容易在
    // 调用其他函数（它们也要读配置）时踩到递归读锁
    let cfg_now = snapshot();

    // 第一项是不可点的状态文字，纯粹给人看
    let status_w = wide(status_text());
    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, status_w.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

    // NapCat 那项分三种情况，不能只看端口：
    //   托管中  → 给「停止」按钮（卡在扫码时能自救）
    //   在线但非本程序启动 → 置灰说明，避免点了没反应
    //   都没有  → 「启动」
    if cfg_now
        .napcat
        .as_ref()
        .is_some_and(|n| !n.command.is_empty())
    {
        let (label, id, grayed) = if napcat_managed() {
            ("停止 NapCat", ID_STOP_NAPCAT, false)
        } else if napcat_online() {
            ("NapCat 已在运行（非本程序启动）", 0, true)
        } else {
            ("启动 NapCat", ID_START_NAPCAT, false)
        };

        let w = wide(label);
        let flags = if grayed {
            MF_STRING | MF_GRAYED
        } else {
            MF_STRING
        };
        AppendMenuW(menu, flags, id, w.as_ptr());
    }

    let test_w = wide("发送测试消息");
    let open_w = wide("打开配置目录");
    AppendMenuW(menu, MF_STRING, ID_TEST, test_w.as_ptr());
    AppendMenuW(menu, MF_STRING, ID_OPEN_DIR, open_w.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

    // ---- 设置子菜单 ----
    // 勾选状态每次都按当前配置重建，所以点完立刻能看到变化。
    // 勾选后会同时改内存和 notify.toml（toml_edit 保留注释）。
    //
    // 文本/路径类的配置（prefix、chain、QQ 账号、NapCat 路径）
    // 没法在菜单里输入，统一走上面的「打开配置目录」。
    let sub = CreatePopupMenu();
    if !sub.is_null() {
        // 第一组：总开关
        let enabled_w = wide("启用通知");
        let flags = if cfg_now.enabled {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        AppendMenuW(sub, flags, ID_TOGGLE_ENABLED, enabled_w.as_ptr());

        // 子 Agent 结束也会触发 notify，但它们不代表「活干完了」
        let main_only_w = wide("只通知主 Agent");
        let flags = if cfg_now.only_main_agent {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        AppendMenuW(sub, flags, ID_TOGGLE_MAIN_ONLY, main_only_w.as_ptr());
        AppendMenuW(sub, MF_SEPARATOR, 0, std::ptr::null());

        // 第二组：显示相关
        let items: [(usize, &str, bool); 4] = [
            (ID_TOGGLE_LIMITS, "显示额度", cfg_now.show_limits),
            (ID_TOGGLE_RESET, "显示重置时间", cfg_now.show_reset),
            (
                ID_TOGGLE_REMAINING,
                "额度显示为「剩余」",
                cfg_now.limit_style.eq_ignore_ascii_case("remaining"),
            ),
            (ID_TOGGLE_FOOTER, "显示会话标记 turn/thread", cfg_now.show_footer),
        ];
        for (id, label, on) in items {
            let w = wide(label);
            let flags = if on {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            AppendMenuW(sub, flags, id, w.as_ptr());
        }

        // 摘要内容三档循环，标签里带当前档位
        let summary_label = match cfg_now.summary_source.trim().to_ascii_lowercase().as_str() {
            "message" => "摘要内容：最后一条回复",
            "none" => "摘要内容：不显示",
            _ => "摘要内容：会话名",
        };
        let summary_w = wide(summary_label);
        AppendMenuW(sub, MF_STRING, ID_CYCLE_SUMMARY, summary_w.as_ptr());
        AppendMenuW(sub, MF_SEPARATOR, 0, std::ptr::null());

        // 第三组：数值档位。标签里带当前值，点一下切到下一档
        let max_label = format!("摘要长度：{} 字", cfg_now.max_chars);
        let max_w = wide(&max_label);
        AppendMenuW(sub, MF_STRING, ID_CYCLE_MAX_CHARS, max_w.as_ptr());

        let timeout_label = format!("HTTP 超时：{} 秒", cfg_now.timeout_secs);
        let timeout_w = wide(&timeout_label);
        AppendMenuW(sub, MF_STRING, ID_CYCLE_TIMEOUT, timeout_w.as_ptr());

        let shell_now = cfg_now
            .napcat
            .as_ref()
            .map(|n| n.shell.as_str())
            .unwrap_or("pwsh");
        let shell_w = wide(&format!("启动外壳：{shell_now}"));
        AppendMenuW(sub, MF_STRING, ID_CYCLE_SHELL, shell_w.as_ptr());

        let kill_on = cfg_now.napcat.as_ref().is_some_and(|n| n.kill_on_exit);
        let kill_w = wide("退出托盘时一起关闭 NapCat");
        let flags = if kill_on {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        AppendMenuW(sub, flags, ID_TOGGLE_KILL_EXIT, kill_w.as_ptr());

        // MF_POPUP：这一项本身不点，展开成子菜单。
        // 句柄按整数传，第三个参数就是子菜单的 HMENU。
        let set_w = wide("设置");
        AppendMenuW(menu, MF_POPUP, sub as usize, set_w.as_ptr());
    }

    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    let quit_w = wide("退出");
    AppendMenuW(menu, MF_STRING, ID_QUIT, quit_w.as_ptr());

    // 弹出菜单必须先把焦点抢过来，否则点空白处菜单不会消失
    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);

    let cmd = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        0,
        hwnd,
        std::ptr::null(),
    ) as usize;

    // 菜单用完要销毁，否则每次右键都漏一个菜单句柄。
    // 子菜单挂在父菜单上，DestroyMenu 父级会一并销毁，不用单独处理。
    DestroyMenu(menu);

    match cmd {
        ID_TEST => send_test(),
        ID_OPEN_DIR => open_config_dir(),
        ID_START_NAPCAT => {
            if let Some(nc) = snapshot().napcat {
                start_napcat(
                    &nc.command,
                    nc.kill_on_exit,
                    crate::napcat::parse_shell(&nc.shell),
                );
            }
        }
        ID_STOP_NAPCAT => stop_napcat(),
        ID_TOGGLE_LIMITS => toggle_bool("show_limits", cfg_now.show_limits),
        ID_TOGGLE_RESET => toggle_bool("show_reset", cfg_now.show_reset),
        ID_TOGGLE_FOOTER => toggle_bool("show_footer", cfg_now.show_footer),
        ID_TOGGLE_REMAINING => toggle_limit_style(),
        ID_TOGGLE_ENABLED => toggle_bool("enabled", cfg_now.enabled),
        ID_TOGGLE_MAIN_ONLY => toggle_bool("only_main_agent", cfg_now.only_main_agent),
        ID_TOGGLE_KILL_EXIT => {
            let on = cfg_now.napcat.as_ref().is_some_and(|n| n.kill_on_exit);
            toggle_bool("napcat.kill_on_exit", on);
        }
        ID_CYCLE_MAX_CHARS => cycle_int("max_chars", cfg_now.max_chars as i64, &MAX_CHARS_STEPS),
        ID_CYCLE_TIMEOUT => cycle_int("timeout_secs", cfg_now.timeout_secs as i64, &TIMEOUT_STEPS),
        ID_CYCLE_SUMMARY => cycle_str("summary_source", &cfg_now.summary_source, &SUMMARY_SOURCES),
        ID_CYCLE_SHELL => cycle_str(
            "napcat.shell",
            cfg_now.napcat.as_ref().map(|n| n.shell.as_str()).unwrap_or("pwsh"),
            &SHELL_STEPS,
        ),
        ID_QUIT => {
            // 先删图标再关窗口：顺序反了会在托盘留下残影
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = 1;
            Shell_NotifyIconW(NIM_DELETE, &nid);
            DestroyWindow(hwnd);
        }
        _ => {}
    }
}

/// 翻转一个布尔设置：先改内存（立刻生效），再写回文件（下次启动还在）。
///
/// `key` 支持点路径（如 `napcat.kill_on_exit`），和文件里的位置对应。
/// 落盘失败只记日志 —— 内存里已经改了，这次运行期间照常工作，
/// 只是重启后会回到旧值。没必要因为这个弹窗打断用户。
fn toggle_bool(key: &str, current: bool) {
    let new = !current;

    if let Ok(mut c) = cfg().write() {
        match key {
            "enabled" => c.enabled = new,
            "only_main_agent" => c.only_main_agent = new,
            "show_limits" => c.show_limits = new,
            "show_reset" => c.show_reset = new,
            "show_footer" => c.show_footer = new,
            // kill_on_exit 在嵌套表里；表不存在就先建一份默认的，
            // 否则「勾了但文件里没有 [napcat] 段」会静默丢失
            "napcat.kill_on_exit" => {
                c.napcat.get_or_insert_with(Default::default).kill_on_exit = new;
            }
            _ => {}
        }
    }

    match crate::config::save_bool(key, new) {
        Ok(()) => crate::logfile::log(&format!("设置 {key} = {new}")),
        Err(e) => crate::logfile::log(&format!("保存 {key} 失败: {e}")),
    }
}

/// 数值档位循环：菜单没法输入数字，点一下切到下一档。
///
/// 当前值不在档位表里（比如用户手写了 130）时，
/// 跳回第一档 —— 比报错或原地不动都好，至少行为可预期。
fn cycle_int(key: &str, current: i64, steps: &[i64]) {
    let next = steps
        .iter()
        .position(|&v| v == current)
        .map(|i| steps[(i + 1) % steps.len()])
        .unwrap_or(steps[0]);

    if let Ok(mut c) = cfg().write() {
        match key {
            "max_chars" => c.max_chars = next.max(1) as usize,
            "timeout_secs" => c.timeout_secs = next.max(1) as u64,
            _ => {}
        }
    }

    match crate::config::save_int(key, next) {
        Ok(()) => crate::logfile::log(&format!("设置 {key} = {next}")),
        Err(e) => crate::logfile::log(&format!("保存 {key} 失败: {e}")),
    }
}

/// 在几个字符串档位之间循环（目前只有 summary_source 用）
fn cycle_str(key: &str, current: &str, steps: &[&str]) {
    let next = steps
        .iter()
        .position(|&v| v.eq_ignore_ascii_case(current.trim()))
        .map(|i| steps[(i + 1) % steps.len()])
        .unwrap_or(steps[0]);

    if let Ok(mut c) = cfg().write() {
        match key {
            "summary_source" => c.summary_source = next.to_string(),
            "napcat.shell" => {
                c.napcat.get_or_insert_with(Default::default).shell = next.to_string();
            }
            _ => {}
        }
    }

    match crate::config::save_str(key, next) {
        Ok(()) => crate::logfile::log(&format!("设置 {key} = {next}")),
        Err(e) => crate::logfile::log(&format!("保存 {key} 失败: {e}")),
    }
}

/// 在「已用 / 剩余」之间切换额度口径
fn toggle_limit_style() {
    let remaining = snapshot()
        .limit_style
        .eq_ignore_ascii_case("remaining");
    let new = if remaining { "used" } else { "remaining" };

    if let Ok(mut c) = cfg().write() {
        c.limit_style = new.to_string();
    }

    match crate::config::save_str("limit_style", new) {
        Ok(()) => crate::logfile::log(&format!("设置 limit_style = {new}")),
        Err(e) => crate::logfile::log(&format!("保存 limit_style 失败: {e}")),
    }
}

/// 托盘菜单里的「发送测试消息」：走和 `--test` 完全一样的路径
fn send_test() {
    let cfg_now = snapshot();
    let event = crate::event::test_event();
    let msg = crate::message::compose(&event, &cfg_now);
    crate::deliver(&cfg_now, &msg, &event.cwd);
}

/// 拉起 NapCat 并接管它的生命周期。
///
/// 已经在托管中就不重复启动（防止连点两下菜单开出两个 QQ），
/// 但**必须记一笔日志** —— 静默返回会让人以为菜单坏了。
fn start_napcat(command: &[String], kill_on_exit: bool, shell: crate::napcat::Shell) {
    if napcat_managed() {
        crate::logfile::log("NapCat 已在托管中，忽略这次启动请求");
        return;
    }
    match crate::napcat::start(command, kill_on_exit, shell) {
        Ok(started) => match started.managed {
            Some(guard) => {
                if let Ok(mut g) = GUARD.lock() {
                    *g = Some(guard);
                }
                crate::logfile::log("NapCat 已纳入托管，退出托盘时会一起结束");

                // 弹不弹图由实际外壳决定：pwsh（5.1）不弹，cmd / pwsh7 都弹。
                // 用实际值而不是配置值 —— 降级到 cmd 时应该照样弹图。
                if started.used.should_pop_qrcode() {
                    watch_qrcode(command);
                } else {
                    crate::logfile::log(&format!(
                        "{} 启动：二维码看控制台，不弹图片",
                        started.used.label()
                    ));
                }
            }
            None => {
                // kill_on_exit = false，或者加不进 Job：进程已在跑，只是不归我们管
                crate::logfile::log("NapCat 已启动，但未纳入托管");
            }
        },
        Err(e) => crate::logfile::log(&format!("NapCat 启动失败: {e}")),
    }
}

/// 手动停止托管的 NapCat：把句柄丢掉即可，Job 关闭会带走整棵进程树。
///
/// 主要给「卡在扫码登录」的情况用 —— 否则用户只能靠退出整个托盘来脱身。
fn stop_napcat() {
    if let Ok(mut g) = GUARD.lock() {
        if g.take().is_some() {
            crate::logfile::log("已手动停止 NapCat");
        }
    }
}

/// 把二维码弹给用户看。
///
/// 不能直接打开 NapCat 的 `cache/qrcode.png`：刷新二维码是**覆盖同一个文件**，
/// 看图程序已经开着它时，Shell 会认为「已经在看了」—— 既不新建窗口也不刷新，
/// 新码就静默躺在磁盘上；有些看图程序还会锁住文件，干扰 NapCat 下次写入。
///
/// 所以先复制成一份唯一名字的快照再打开：
///   · 每次都是「新文件」，看图程序必定新开窗口/标签
///   · 看图程序锁的是副本，NapCat 覆盖原文件不受影响
///   · 用 ShellExecuteW 而不是 explorer.exe 中转，能拿到成败结果并指定 SW_SHOWNORMAL
/// 复制一份带唯一名字的快照。单独拆出来是为了能单测，不弹窗。
fn qrcode_snapshot(src: &std::path::Path, seq: u32) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("codex-notify");
    std::fs::create_dir_all(&dir).map_err(|e| format!("建临时目录失败: {e}"))?;

    // 时间戳 + 序号：同一秒内连续刷新也不会撞名
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dst = dir.join(format!("qrcode-{stamp}-{seq}.png"));
    std::fs::copy(src, &dst).map_err(|e| format!("复制二维码失败: {e}"))?;
    Ok(dst)
}

fn show_qrcode(src: &std::path::Path, seq: u32) -> Result<PathBuf, String> {
    let dst = qrcode_snapshot(src, seq)?;

    let op = wide("open");
    let file = wide(&dst.display().to_string());
    // ShellExecuteW 返回 <= 32 表示失败，那个值本身是错误码
    let ret = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL as i32,
        )
    } as isize;
    if ret <= 32 {
        return Err(format!("ShellExecuteW 返回 {ret}"));
    }
    Ok(dst)
}

/// 盯着 NapCat 的登录二维码，出了新的就自动弹出来。
///
/// 只在托盘自己启动 NapCat 之后调用。最多看 5 分钟、最多弹 3 次 ——
/// 二维码过期时 NapCat 会重写这个文件，所以能覆盖"扫慢了要重扫"的情况，
/// 但不会无限弹窗。
fn watch_qrcode(command: &[String]) {
    let Some(path) = crate::napcat::qrcode_path(command) else {
        return;
    };
    let started = std::time::SystemTime::now();

    std::thread::spawn(move || {
        let mut last: Option<std::time::SystemTime> = None;
        let mut opened: u32 = 0;
        // 上一份快照留着，弹下一张时顺手删掉（看图程序锁着就删不掉，忽略）
        let mut previous: Option<PathBuf> = None;

        // 150 次 × 2 秒 = 5 分钟，足够走完"扫码 → 手机确认"。
        for _ in 0..150 {
            std::thread::sleep(Duration::from_secs(2));

            // 登录成功后就不用再弹了
            if napcat_online() {
                break;
            }

            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let Ok(mtime) = meta.modified() else { continue };

            // 只要本次启动之后生成的那张，且没弹过
            if mtime >= started && last != Some(mtime) && opened < 3 {
                last = Some(mtime);
                opened += 1;

                if let Some(old) = previous.take() {
                    let _ = std::fs::remove_file(old);
                }
                match show_qrcode(&path, opened) {
                    Ok(shown) => {
                        previous = Some(shown.clone());
                        crate::logfile::log(&format!(
                            "已弹出登录二维码: {}（快照 {}）",
                            path.display(),
                            shown.display()
                        ));
                    }
                    Err(e) => {
                        // 兜底：ShellExecute 失败时再试一次 explorer，别静默吞掉
                        let _ = std::process::Command::new("explorer")
                            .arg(path.as_os_str())
                            .spawn();
                        crate::logfile::log(&format!("弹出二维码失败（{e}），已回退 explorer"));
                    }
                }
            }
        }
    });
}

/// 用资源管理器打开配置文件所在目录
fn open_config_dir() {
    let dir: PathBuf = crate::config::data_dir();
    // 用 explorer 打开目录是 Windows 上最省事的做法，不依赖额外 crate
    let _ = std::process::Command::new("explorer")
        .arg(dir.as_os_str())
        .spawn();
}

/// 让系统知道我们要用的小图标尺寸（当前未直接使用，保留给后续调尺寸）
#[allow(dead_code)]
fn small_icon_size() -> i32 {
    unsafe { GetSystemMetrics(SM_CXSMICON) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：二维码每次刷新都必须拿到**不同的文件名**。
    /// 旧实现直接打开 NapCat 那个固定名字的 qrcode.png，看图程序开着时刷新就弹不出来。
    #[test]
    fn qrcode_refresh_always_gets_a_fresh_snapshot() {
        let src = std::env::temp_dir().join(format!("codex-notify-src-{}.png", std::process::id()));
        std::fs::write(&src, b"fake png bytes").unwrap();

        let first = qrcode_snapshot(&src, 1).unwrap();
        let second = qrcode_snapshot(&src, 2).unwrap();
        assert_ne!(first, second, "刷新后的快照不能和上一张同名");
        assert_eq!(std::fs::read(&first).unwrap(), b"fake png bytes");
        assert_eq!(std::fs::read(&second).unwrap(), b"fake png bytes");

        let _ = std::fs::remove_file(first);
        let _ = std::fs::remove_file(second);
        let _ = std::fs::remove_file(src);
    }
}
