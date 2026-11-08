//! 托管 NapCat：托盘启动时一并拉起，退出时一起结束。
//!
//! # 为什么用 Job Object
//!
//! NapCat 不是单个进程。看它的启动脚本就知道：
//!
//! ```text
//! powershell.exe → cmd（解释 .bat）→ NapCatWinBootMain.exe → QQ.exe（+ 若干渲染/GPU 子进程）
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

/// CREATE_NEW_CONSOLE：给 NapCat 一个自己的控制台窗口。
///
/// 这里**不能**用 CREATE_NO_WINDOW 把它藏起来。实测发现：剥掉控制台之后，
/// 启动脚本里的 `reg query` 和 NapCat 引导器都会静默失败 ——
/// 结果是 QQ 起来了，但 NapCat 模块根本没加载，3000 端口永远不通，
/// 而且什么错都不报（连 NapCat 自己的 logs 目录都是空的）。
///
/// 控制台对 NapCat 不是装饰：二维码、登录状态、报错都打在上面。
/// 所以托盘只负责「替你双击一下」，窗口照常出现，用完跟着 Job 一起关掉。
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// 由本程序启动的 NapCat。
///
/// 持有 job 句柄，`Drop` 时关闭它 —— 如果启用了 kill_on_exit，
/// 这一步会连带结束整棵进程树（见模块文档）。
pub struct Managed {
    job: HANDLE,
}

/// NapCat 把登录二维码写到 `<NapCat目录>/cache/qrcode.png`。
///
/// 为什么需要这个：控制台里的二维码是字符画，终端字体和编码经常
/// 渲染成乱码，根本扫不出来。而这张 PNG 是干净的 147×147 图片，
/// 直接打开就能扫 —— 比纠结用哪个终端靠谱得多。
///
/// 路径从启动命令推导：`D:\tools\NapCat\launcher-user.bat`
/// → `D:\tools\NapCat\cache\qrcode.png`
pub fn qrcode_path(command: &[String]) -> Option<std::path::PathBuf> {
    let launcher = command.first()?;
    let dir = std::path::Path::new(launcher).parent()?;
    Some(dir.join("cache").join("qrcode.png"))
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

/// 把配置里的「程序 + 参数」拼成一段 PowerShell 命令。
/// 单引号包住每个 token，内部的单引号写成两个（PowerShell 的转义规则）。
fn powershell_script(command: &[String]) -> String {
    let mut s = String::from("& ");
    for (i, part) in command.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push('\'');
        s.push_str(&part.replace('\'', "''"));
        s.push('\'');
    }
    s
}

fn spawn_via_powershell(command: &[String]) -> Result<std::process::Child, String> {
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"])
        .arg(powershell_script(command))
        .creation_flags(CREATE_NEW_CONSOLE);
    c.spawn().map_err(|e| e.to_string())
}

/// 兜底：机器上找不到 powershell.exe 时，回到原来的方式。
fn spawn_via_cmd(command: &[String]) -> Result<std::process::Child, String> {
    let mut c = Command::new("cmd");
    c.arg("/c").arg(&command[0]);
    for arg in &command[1..] {
        c.arg(arg);
    }
    c.creation_flags(CREATE_NEW_CONSOLE);
    c.spawn().map_err(|e| e.to_string())
}

/// 按配置里的命令启动 NapCat。
///
/// `kill_on_exit = true` 时把它放进作业对象，本进程退出即连带结束；
/// 否则就是普通地拉起来，撒手不管。
pub fn start(command: &[String], kill_on_exit: bool) -> Result<Option<Managed>, String> {
    if command.is_empty() {
        return Err("没有配置 [napcat].command".to_string());
    }

    // PowerShell 当外层宿主：窗口对 UTF-8 / 字符画更友好，.ps1 / .exe 也能用同一套调用。
    // 注意 .bat 本质仍由系统隐式调 cmd.exe 解释，所以只是「外壳」换了，
    // NapCat 脚本本身的内容和行为完全不变。
    //
    // 万一机器上找不到 powershell.exe（精简系统、PATH 被裁），退回原来的 cmd /c ——
    // 不能因为换个窗口就让 NapCat 起不来。
    let child = spawn_via_powershell(command)
        .or_else(|e| {
            log(&format!("PowerShell 启动失败（{e}），回退 cmd"));
            spawn_via_cmd(command)
        })
        .map_err(|e| format!("启动失败: {e}"))?;

    // 不重定向 stdout/stderr：让它占着自己的控制台，和手动双击脚本完全一致。
    // 之前把输出倒进文件、顺便把 stdin 接到 null，结果 NapCat 引导器行为异常，
    // 得不偿失。想看输出就直接看它自己的窗口。

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

        // 把刚拉起的 PowerShell 放进去。它的子进程（cmd、NapCatWinBootMain、QQ）
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_script_quotes_each_token() {
        assert_eq!(
            powershell_script(&[r"D:\tools\NapCat\launcher-user.bat".to_string()]),
            r"& 'D:\tools\NapCat\launcher-user.bat'"
        );
        assert_eq!(
            powershell_script(&["a b.bat".to_string(), "it's".to_string()]),
            "& 'a b.bat' 'it''s'"
        );
    }
}
