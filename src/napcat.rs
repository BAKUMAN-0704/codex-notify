//! 托管 NapCat：托盘启动时一并拉起，退出时一起结束。
//!
//! # 为什么用 Job Object
//!
//! NapCat 不是单个进程。看它的启动脚本就知道：
//!
//! ```text
//! launcher-user.bat → cmd → NapCatWinBootMain.exe → QQ.exe（+ 若干渲染/GPU 子进程）
//! ```
//!
//! 想「退出时一起关掉」，逐个记 PID 再 kill 是不可靠的：子进程会漏、PID 会复用、
//! 中途重启就对不上了。
//!
//! Windows 的 Job Object 正好解决这个：把顶层进程放进一个作业，
//! 给作业设置 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`——**最后一个句柄关闭时，
//! 作业内所有进程一起结束**。我们进程一退，句柄自动关闭，整棵树跟着消失，
//! 不需要写任何清理代码。
//!
//! # 边界
//!
//! 只管理**自己启动的**那一个。NapCat 已经在跑（端口能连上）时不启动、也不接管，
//! 所以退出时不会误杀你手动开的那个。

use std::os::windows::process::CommandExt;
use std::process::Command;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JobObjectExtendedLimitInformation,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

use crate::logfile::log;

/// CREATE_NO_WINDOW：不给 cmd 创建控制台窗口。
/// 不加的话，托盘在后台拉起一个 .bat 会闪一下黑框，白改 GUI 子系统了。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 由本程序启动的 NapCat。
///
/// 持有 job 句柄，`Drop` 时关闭它 —— 如果启用了 kill_on_exit，
/// 这一步会连带结束整棵进程树（见模块文档）。
pub struct Managed {
    job: HANDLE,
}

// 句柄本身只是个整数，跨线程传递没问题
unsafe impl Send for Managed {}

impl Drop for Managed {
    fn drop(&mut self) {
        if !self.job.is_null() {
            unsafe { CloseHandle(self.job) };
        }
    }
}

/// 按配置里的命令启动 NapCat。
///
/// `kill_on_exit = true` 时把它放进作业对象，本进程退出即连带结束；
/// 否则就是普通地拉起来，撒手不管。
pub fn start(command: &[String], kill_on_exit: bool) -> Result<Option<Managed>, String> {
    if command.is_empty() {
        return Err("没有配置 [napcat].command".to_string());
    }

    let mut cmd = Command::new("cmd");
    // .bat 必须经由 cmd 执行；/c 表示跑完就退出
    cmd.arg("/c").arg(&command[0]);
    for arg in &command[1..] {
        cmd.arg(arg);
    }
    cmd.creation_flags(CREATE_NO_WINDOW);

    let child = cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
    let pid = child.id();
    log(&format!("已启动 NapCat pid={pid}"));

    if !kill_on_exit {
        // 不托管生命周期：子进程继续跑，我们退出也不管它
        return Ok(None);
    }

    // 作业句柄在 unsafe 块里创建，成功后才赋值给外面这个变量，
    // 再交给调用方（托盘）持有 —— 句柄活着，作业就受管控。
    let job: HANDLE;
    unsafe {
        let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if h.is_null() {
            return Err("创建 Job Object 失败".to_string());
        }

        // 关键设置：作业内最后一个句柄关闭 → 结束所有进程
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            h,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            CloseHandle(h);
            return Err("设置 Job Object 失败".to_string());
        }

        // 把刚拉起的 cmd 放进去。它的子进程（NapCatWinBootMain、QQ）
        // 默认会继承这个作业，所以整棵树都在管控范围内。
        let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
        if proc.is_null() {
            // 极少数情况会失败（权限不足）。此时不回滚：NapCat 已经在跑了，
            // 只是失去托管能力。宁可「能启动但退不干净」，也别「启动失败」。
            log("无法打开 NapCat 进程，退出时不会自动结束它");
            CloseHandle(h);
            return Ok(None);
        }
        let assigned = AssignProcessToJobObject(h, proc);
        CloseHandle(proc);
        if assigned == 0 {
            log("把 NapCat 加入 Job Object 失败，退出时不会自动结束它");
            CloseHandle(h);
            return Ok(None);
        }

        job = h;
    }

    // 走到这里说明作业已建好、进程已放进去，把句柄带出去交给调用方持有
    Ok(Some(Managed { job }))
}
