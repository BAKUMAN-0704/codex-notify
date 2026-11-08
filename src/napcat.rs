//! 托管 NapCat：托盘启动时一并拉起，退出时一起结束。
//!
//! # 为什么用 Job Object
//!
//! NapCat 不是单个进程。看它的启动脚本就知道：
//!
//! ```text
//! <外壳> → cmd（解释 .bat）→ NapCatWinBootMain.exe → QQ.exe（+ 若干渲染/GPU 子进程）
//! ```
//!
//! `<外壳>` 由 `[napcat].shell` 决定：`cmd` / `pwsh`（5.1）/ `pwsh7`（7）。
//! 三档都只是换外层窗口，`.bat` 始终由系统隐式调 `cmd.exe` 解释。
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

/// 启动 NapCat 的外层宿主，对应 `[napcat].shell`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// `cmd /c <脚本>`
    Cmd,
    /// Windows PowerShell 5.1（`powershell.exe`）
    Pwsh,
    /// PowerShell 7（`pwsh.exe`）
    Pwsh7,
}

impl Shell {
    pub fn label(self) -> &'static str {
        match self {
            Shell::Cmd => "cmd",
            Shell::Pwsh => "pwsh",
            Shell::Pwsh7 => "pwsh7",
        }
    }

    /// 是否要额外弹二维码 PNG。
    ///
    /// 按用户设定：**只有 `pwsh`（5.1）不弹**，直接看控制台里的字符画；
    /// `cmd` 和 `pwsh7` 都会弹。这不是按「谁的编码更好」推出来的，
    /// 是明确要求的行为，别自作主张改。
    pub fn should_pop_qrcode(self) -> bool {
        !matches!(self, Shell::Pwsh)
    }
}

/// 解析配置值。未知值一律按 `pwsh`（默认档），避免写错字就退化成 cmd。
pub fn parse_shell(s: &str) -> Shell {
    match s.trim().to_ascii_lowercase().as_str() {
        "cmd" => Shell::Cmd,
        "pwsh7" | "pwsh-7" => Shell::Pwsh7,
        _ => Shell::Pwsh,
    }
}

/// 由本程序启动的 NapCat。
///
/// 持有 job 句柄，`Drop` 时关闭它 —— 如果启用了 kill_on_exit，
/// 这一步会连带结束整棵进程树（见模块文档）。
pub struct Managed {
    job: HANDLE,
}

/// `start()` 的结果。
///
/// `used` 是**实际**用上的外壳：选了 pwsh / pwsh7 但机器上找不到时，
/// 会降级为 `Cmd`。托盘据此决定是否弹二维码图片。
pub struct Started {
    pub managed: Option<Managed>,
    pub used: Shell,
}

/// NapCat 把登录二维码写到 `<NapCat目录>/cache/qrcode.png`。
///
/// 控制台里的二维码是字符画，cmd 的字体和编码经常渲染成乱码，
/// 所以某些外壳下需要把这张干净的 147×147 PNG 弹出来扫码
/// （弹不弹由 `Shell::should_pop_qrcode` 决定）。
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

/// 5.1 的候选路径：PATH 里的 `powershell.exe`，再加 System32 完整路径。
fn pwsh5_exes() -> Vec<String> {
    let mut v = vec!["powershell.exe".to_string()];
    if let Ok(root) = std::env::var("SystemRoot") {
        v.push(format!(
            "{root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"
        ));
    }
    v
}

/// 7 的候选路径：PATH 里的 `pwsh.exe`，再加标准安装目录。
fn pwsh7_exes() -> Vec<String> {
    let mut v = vec!["pwsh.exe".to_string()];
    if let Ok(pf) = std::env::var("ProgramFiles") {
        v.push(format!("{pf}\\PowerShell\\7\\pwsh.exe"));
    }
    v
}

/// 依次尝试候选 exe，全部失败才返回错误（交给调用方降级 cmd）。
fn spawn_ps(exes: &[String], command: &[String]) -> Result<std::process::Child, String> {
    let script = powershell_script(command);
    let mut errors = Vec::new();
    for exe in exes {
        let mut c = Command::new(exe);
        c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"])
            .arg(&script)
            .creation_flags(CREATE_NEW_CONSOLE);
        match c.spawn() {
            Ok(child) => return Ok(child),
            Err(e) => errors.push(format!("{exe}: {e}")),
        }
    }
    Err(errors.join("; "))
}

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
/// 指定的 PowerShell 找不到时自动降级 `cmd /c`，不让 NapCat 起不来。
pub fn start(command: &[String], kill_on_exit: bool, shell: Shell) -> Result<Started, String> {
    if command.is_empty() {
        return Err("没有配置 [napcat].command".to_string());
    }

    let (child, used) = match shell {
        Shell::Cmd => (spawn_via_cmd(command)?, Shell::Cmd),
        Shell::Pwsh => match spawn_ps(&pwsh5_exes(), command) {
            Ok(c) => (c, Shell::Pwsh),
            Err(e) => {
                log(&format!("pwsh 启动失败（{e}），回退 cmd"));
                (spawn_via_cmd(command)?, Shell::Cmd)
            }
        },
        Shell::Pwsh7 => match spawn_ps(&pwsh7_exes(), command) {
            Ok(c) => (c, Shell::Pwsh7),
            Err(e) => {
                log(&format!("pwsh7 启动失败（{e}），回退 cmd"));
                (spawn_via_cmd(command)?, Shell::Cmd)
            }
        },
    };

    // 不重定向 stdout/stderr：让它占着自己的控制台，和手动双击脚本完全一致。
    // 之前把输出倒进文件、顺便把 stdin 接到 null，结果 NapCat 引导器行为异常，
    // 得不偿失。想看输出就直接看它自己的窗口。

    let pid = child.id();
    log(&format!("已启动 NapCat pid={pid} · 外壳 {}", used.label()));

    if !kill_on_exit {
        // 不托管生命周期：子进程继续跑，我们退出也不管它
        return Ok(Started {
            managed: None,
            used,
        });
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

        // 把刚拉起的外壳进程放进去。它的子进程（cmd、NapCatWinBootMain、QQ）
        // 默认会继承这个作业，所以整棵树都在管控范围内。
        let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
        if proc.is_null() {
            // 极少数情况会失败（权限不足）。此时不回滚：NapCat 已经在跑了，
            // 只是失去托管能力。宁可「能启动但退不干净」，也别「启动失败」。
            log("无法打开 NapCat 进程，退出时不会自动结束它");
            CloseHandle(h);
            return Ok(Started {
                managed: None,
                used,
            });
        }
        let assigned = AssignProcessToJobObject(h, proc);
        CloseHandle(proc);
        if assigned == 0 {
            log("把 NapCat 加入 Job Object 失败，退出时不会自动结束它");
            CloseHandle(h);
            return Ok(Started {
                managed: None,
                used,
            });
        }

        job = h;
    }

    // 走到这里说明作业已建好、进程已放进去，把句柄带出去交给调用方持有
    Ok(Started {
        managed: Some(Managed { job }),
        used,
    })
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

    #[test]
    fn shell_values_and_qrcode_rule() {
        assert_eq!(parse_shell("cmd"), Shell::Cmd);
        assert_eq!(parse_shell("pwsh"), Shell::Pwsh);
        assert_eq!(parse_shell("pwsh7"), Shell::Pwsh7);
        assert_eq!(parse_shell("PWSH7"), Shell::Pwsh7);
        assert_eq!(parse_shell("写错了"), Shell::Pwsh); // 未知值退回默认档

        // 用户明确要求：只有 pwsh 不弹，cmd 和 pwsh7 都弹
        assert!(Shell::Cmd.should_pop_qrcode());
        assert!(!Shell::Pwsh.should_pop_qrcode());
        assert!(Shell::Pwsh7.should_pop_qrcode());
    }
}
