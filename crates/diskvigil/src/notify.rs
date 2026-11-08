//! Alert delivery: desktop notifications and the user-defined command hook.
//! Uses OS tools (PowerShell toast, notify-send, osascript) so the binary stays
//! dependency-free and portable. Text is passed via env vars / argv, never
//! interpolated into scripts.

use dv_core::alerts::AlertEvent;
use dv_core::config::AlertSettings;
use dv_core::Severity;
use std::process::{Command, Stdio};

pub fn dispatch(events: &[AlertEvent], cfg: &AlertSettings) {
    if events.is_empty() || !cfg.enabled {
        return;
    }
    if cfg.desktop_notifications {
        let worst = events
            .iter()
            .map(|e| e.severity)
            .max()
            .unwrap_or(Severity::Info);
        let (title, body) = if events.len() == 1 {
            let e = &events[0];
            (format!("DiskVigil – {}", e.drive_name), e.message.clone())
        } else {
            let lines: Vec<String> = events
                .iter()
                .take(4)
                .map(|e| format!("{}: {}", e.drive_name, e.message))
                .collect();
            let more = if events.len() > 4 {
                format!("\n…and {} more", events.len() - 4)
            } else {
                String::new()
            };
            (
                format!("DiskVigil – {} alerts ({})", events.len(), worst.label()),
                lines.join("\n") + &more,
            )
        };
        desktop(&title, &body);
    }
    if !cfg.command.trim().is_empty() {
        for e in events {
            run_hook(&cfg.command, e);
        }
    }
}

fn spawn_detached(mut cmd: Command) {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    if let Ok(mut child) = cmd.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

pub fn desktop(title: &str, body: &str) {
    #[cfg(windows)]
    {
        const SCRIPT: &str = r#"
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null
$t = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
$n = $t.GetElementsByTagName('text')
$n.Item(0).AppendChild($t.CreateTextNode($env:DV_TITLE)) > $null
$n.Item(1).AppendChild($t.CreateTextNode($env:DV_BODY)) > $null
$id = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($id).Show([Windows.UI.Notifications.ToastNotification]::new($t))
"#;
        let mut c = Command::new("powershell");
        c.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-Command",
            SCRIPT,
        ])
        .env("DV_TITLE", title)
        .env("DV_BODY", body);
        spawn_detached(c);
    }
    #[cfg(target_os = "macos")]
    {
        let mut c = Command::new("osascript");
        c.args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
            title,
            body,
        ]);
        spawn_detached(c);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut c = Command::new("notify-send");
        c.args(["-a", "DiskVigil", "-i", "drive-harddisk", title, body]);
        spawn_detached(c);
    }
}

fn run_hook(command: &str, e: &AlertEvent) {
    let mut c = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    c.env("DV_SEVERITY", e.severity.label())
        .env("DV_DRIVE", &e.drive_key)
        .env("DV_MODEL", &e.drive_name)
        .env("DV_SERIAL", &e.serial)
        .env("DV_MESSAGE", &e.message)
        .env("DV_TIME", e.time.to_rfc3339());
    spawn_detached(c);
}
