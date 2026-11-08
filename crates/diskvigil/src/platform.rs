//! Small OS integrations: console attach, elevation relaunch, opening files.

use anyhow::Result;
use std::path::Path;
use std::process::Command;

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        pub fn AttachConsole(pid: u32) -> i32;
    }
    #[link(name = "shell32")]
    extern "system" {
        pub fn ShellExecuteW(
            hwnd: *mut c_void,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
    }
}

/// Lets a GUI-subsystem build print to the console it was started from.
pub fn attach_console() {
    #[cfg(windows)]
    // SAFETY: ATTACH_PARENT_PROCESS; failure (no parent console) is harmless.
    unsafe {
        win::AttachConsole(0xFFFF_FFFF);
    }
}

/// Starts a new elevated instance. The caller should exit on success.
pub fn relaunch_elevated(extra_args: &[String]) -> Result<()> {
    let exe = std::env::current_exe()?;
    #[cfg(windows)]
    {
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let params = extra_args
            .iter()
            .map(|a| format!("\"{}\"", a.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ");
        let (op, file, par) = (wide("runas"), wide(&exe.to_string_lossy()), wide(&params));
        // SAFETY: all strings are valid NUL-terminated wide strings.
        let r = unsafe {
            win::ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                file.as_ptr(),
                par.as_ptr(),
                std::ptr::null(),
                1,
            )
        };
        if r <= 32 {
            anyhow::bail!("elevation was cancelled or failed (code {r})");
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let mut cmd = Command::new("pkexec");
        cmd.arg("env");
        for var in [
            "DISPLAY",
            "XAUTHORITY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
        ] {
            if let Ok(v) = std::env::var(var) {
                cmd.arg(format!("{var}={v}"));
            }
        }
        cmd.arg(exe).args(extra_args);
        cmd.spawn()?;
        Ok(())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (exe, extra_args);
        anyhow::bail!("run DiskVigil with sudo to read SMART data");
    }
}

pub fn open_path(p: &Path) {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("explorer");
        c.arg(p);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(p);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(p);
        c
    };
    let _ = cmd.spawn();
}
