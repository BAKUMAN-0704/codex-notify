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
    ExtractIconExW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, LoadCursorW, PostQuitMessage,
    RegisterClassW, SetForegroundWindow, TrackPopupMenu, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, IDC_ARROW, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_APP, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};

use crate::config::Config;

/// 托盘图标的回调消息号。`WM_APP` 之后的号段是留给应用自己用的。
const WM_TRAY: u32 = WM_APP + 1;

// 菜单项 ID。随便取，只要和 wndproc 里的分支对得上。
const ID_TEST: usize = 1;
const ID_OPEN_DIR: usize = 2;
const ID_QUIT: usize = 3;
const ID_START_NAPCAT: usize = 4;

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

/// 托盘模式会用到的那份配置，放在静态变量里供窗口过程访问。
///
/// 用 `OnceLock` 而不是全局可变静态：配置在启动时读一次就不变了，
/// 窗口过程只读不写，天然线程安全。
static CFG: std::sync::OnceLock<Config> = std::sync::OnceLock::new();

/// 进入托盘模式，直到用户点「退出」才返回。
pub fn run(cfg: Config) {
    let _ = CFG.set(cfg);

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

        // 4. 如果配了 NapCat 且它没在跑，就顺带拉起来。
        //    已经在跑（端口能连上）说明是你手动开的，不认识也不去接管。
        if let Some(nc) = CFG.get().and_then(|c| c.napcat.as_ref()) {
            if !nc.command.is_empty() && !napcat_online() {
                start_napcat(nc);
            }
        }

        // 5. 标准消息循环。GetMessageW 返回 0 表示收到 WM_QUIT
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 6. 收尾。先结束托管的 NapCat（关 Job 句柄 → 整棵树一起走），
        //    再删托盘图标，否则鼠标划过托盘会留下一个「幽灵图标」。
        if let Ok(mut g) = GUARD.lock() {
            if g.take().is_some() {
                crate::logfile::log("已结束托管的 NapCat");
            }
        }
        Shell_NotifyIconW(NIM_DELETE, &nid);
        crate::logfile::log("托盘已退出");
    }
}

/// 把提示文字写进 NOTIFYICONDATAW 的定长缓冲区（最多 127 个 UTF-16 码元）
fn write_tip(nid: &mut NOTIFYICONDATAW, text: &str) {
    let mut buf: Vec<u16> = text.encode_utf16().take(127).collect();
    buf.push(0);
    nid.szTip[..buf.len()].copy_from_slice(&buf);
}

/// 取当前状态的提示文字：顺带当作「NapCat 在不在」的体检
fn tip_text() -> String {
    if napcat_online() {
        "codex-notify · NapCat 在线".to_string()
    } else {
        "codex-notify · NapCat 未连接".to_string()
    }
}

/// 判断 NapCat 是否在监听。
///
/// 只做一次 TCP 连接试探，超时 300ms —— 菜单点开要立刻响应，
/// 不能在这里发 HTTP 请求（NapCat 没开时要等满 5 秒，界面会卡住）。
fn napcat_online() -> bool {
    let base = CFG
        .get()
        .and_then(|c| c.qq.as_ref())
        .map(|q| q.base_url.clone())
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

    // 第一项是不可点的状态文字，纯粹给人看
    let status = if napcat_online() {
        "状态：NapCat 在线"
    } else {
        "状态：NapCat 未连接"
    };
    let status_w = wide(status);
    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, status_w.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

    // 只有配了 napcat.command 才显示这一项；已经在跑就置灰（说明是手动开的，我们不管）
    let has_napcat_cfg = CFG
        .get()
        .and_then(|c| c.napcat.as_ref())
        .is_some_and(|n| !n.command.is_empty());
    if has_napcat_cfg {
        let running = napcat_online();
        let label = if running {
            "NapCat 已在运行"
        } else {
            "启动 NapCat"
        };
        let w = wide(label);
        let flags = if running {
            MF_STRING | MF_GRAYED
        } else {
            MF_STRING
        };
        AppendMenuW(menu, flags, ID_START_NAPCAT, w.as_ptr());
    }

    let test_w = wide("发送测试消息");
    let open_w = wide("打开配置目录");
    let quit_w = wide("退出");
    AppendMenuW(menu, MF_STRING, ID_TEST, test_w.as_ptr());
    AppendMenuW(menu, MF_STRING, ID_OPEN_DIR, open_w.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
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

    // 菜单用完要销毁，否则每次右键都漏一个菜单句柄
    DestroyMenu(menu);

    match cmd {
        ID_TEST => send_test(),
        ID_OPEN_DIR => open_config_dir(),
        ID_START_NAPCAT => {
            if let Some(nc) = CFG.get().and_then(|c| c.napcat.as_ref()) {
                start_napcat(nc);
            }
        }
        ID_QUIT => {
            // 先删图标再关窗口：顺序反了会在托盘留下残影
            // 该结构体没有 Default 实现，直接清零（所有字段的零值都是合法初始状态）
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

/// 托盘菜单里的「发送测试消息」：走和 `--test` 完全一样的路径
fn send_test() {
    let Some(cfg) = CFG.get() else { return };
    let event = crate::event::test_event();
    let msg = crate::message::compose(&event, cfg);
    crate::deliver(cfg, &msg, &event.cwd);
}

/// 拉起 NapCat 并接管它的生命周期。
///
/// 已经在托管中就不重复启动（防止连点两下菜单开出两个 QQ）。
fn start_napcat(nc: &crate::config::Napcat) {
    // 已经有一个在托管，直接忽略
    if GUARD.lock().map(|g| g.is_some()).unwrap_or(false) {
        return;
    }
    match crate::napcat::start(&nc.command, nc.kill_on_exit) {
        Ok(Some(guard)) => {
            if let Ok(mut g) = GUARD.lock() {
                *g = Some(guard);
            }
            crate::logfile::log("NapCat 已纳入托管，退出托盘时会一起结束");
        }
        Ok(None) => {
            // kill_on_exit = false，或者加不进 Job：进程已在跑，只是不归我们管
            crate::logfile::log("NapCat 已启动，但未纳入托管");
        }
        Err(e) => crate::logfile::log(&format!("NapCat 启动失败: {e}")),
    }
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
