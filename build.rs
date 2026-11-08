//! 构建脚本：把图标嵌进 Windows 可执行文件。
//!
//! 只在 Windows 上生效；其他平台这里什么也不做。
//!
//! 为什么要用 `if let Err(...)` 而不是直接 `unwrap`：
//! 嵌入图标需要资源编译器（MSVC 下的 `rc.exe`，GNU 工具链下的 `windres`）。
//! 如果机器上没装 Windows SDK，这一部会失败——但图标只是锦上添花，
//! 不应该因此让整个 `cargo build` 挂掉。失败时打一条 warning，产物用默认图标。

fn main() {
    // 图标换了要重新编译，否则 cargo 会命中缓存、继续用旧资源
    println!("cargo:rerun-if-changed=assets/codex-notify.ico");

    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/codex-notify.ico");

        // 逃生口：winres 默认通过注册表找 Windows SDK 的资源编译器。
        // 如果注册表查不到（精简系统、CI、或受限环境），可以显式指定
        // 含 rc.exe 的目录，例如：
        //   set CODEX_NOTIFY_RC_DIR=C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64
        if let Ok(dir) = std::env::var("CODEX_NOTIFY_RC_DIR") {
            if !dir.trim().is_empty() {
                res.set_toolkit_path(dir.trim());
            }
        }

        // 顺手写一点文件属性，鼠标悬停在 exe 上能看到说明
        res.set("FileDescription", "codex-notify");
        res.set("ProductName", "codex-notify");
        res.set("LegalCopyright", "MIT License");

        if let Err(e) = res.compile() {
            println!("cargo:warning=未能嵌入图标（{e}），产物将使用默认图标");
            println!("cargo:warning=可设置 CODEX_NOTIFY_RC_DIR 指向含 rc.exe 的目录后重试");
        }
    }
}
