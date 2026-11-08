//! Portable data directory resolution and persisted settings.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BackendMode {
    /// Native pass-through first, smartctl for drives native access cannot read.
    #[default]
    Auto,
    Native,
    Smartctl,
    Demo,
}

impl BackendMode {
    pub fn label(self) -> &'static str {
        match self {
            BackendMode::Auto => "Automatic (native + smartctl fallback)",
            BackendMode::Native => "Native only",
            BackendMode::Smartctl => "smartctl only",
            BackendMode::Demo => "Demo (simulated drives)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TempLimit {
    pub warn: i32,
    pub crit: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertSettings {
    pub enabled: bool,
    pub desktop_notifications: bool,
    /// Shell command run for every alert. Receives DV_SEVERITY, DV_DRIVE, DV_MODEL,
    /// DV_SERIAL, DV_MESSAGE and DV_TIME environment variables.
    pub command: String,
    /// Alert when health drops by at least this many points.
    pub health_drop: u8,
    /// Alert once when SSD remaining life falls to or below this percentage.
    pub life_warn_pct: u8,
    pub temperature_alerts: bool,
    /// Per-drive temperature limits keyed by `DriveInfo::key()`.
    pub temp_overrides: BTreeMap<String, TempLimit>,
}

impl Default for AlertSettings {
    fn default() -> Self {
        AlertSettings {
            enabled: true,
            desktop_notifications: true,
            command: String::new(),
            health_drop: 1,
            life_warn_pct: 10,
            temperature_alerts: true,
            temp_overrides: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub refresh_secs: u64,
    pub history_interval_mins: u64,
    pub fahrenheit: bool,
    pub theme: Theme,
    pub backend: BackendMode,
    pub smartctl_path: Option<String>,
    pub alerts: AlertSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            refresh_secs: 60,
            history_interval_mins: 30,
            fahrenheit: false,
            theme: Theme::Dark,
            backend: BackendMode::Auto,
            smartctl_path: None,
            alerts: AlertSettings::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub portable: bool,
}

impl Paths {
    /// Resolution order: explicit dir, `DV_DATA_DIR`, `<exe dir>/DiskVigilData`
    /// when writable (portable mode), then the per-user data directory.
    pub fn resolve(explicit: Option<PathBuf>) -> Paths {
        if let Some(p) = explicit.or_else(|| std::env::var_os("DV_DATA_DIR").map(PathBuf::from)) {
            let _ = std::fs::create_dir_all(&p);
            return Paths {
                data_dir: p,
                portable: false,
            };
        }
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
        {
            let p = dir.join("DiskVigilData");
            if is_writable_dir(&p) {
                return Paths {
                    data_dir: p,
                    portable: true,
                };
            }
        }
        let p = user_data_dir().join("DiskVigil");
        let _ = std::fs::create_dir_all(&p);
        Paths {
            data_dir: p,
            portable: false,
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }
    pub fn history_dir(&self) -> PathBuf {
        self.data_dir.join("history")
    }
    pub fn reports_dir(&self) -> PathBuf {
        self.data_dir.join("reports")
    }
    pub fn alerts_file(&self) -> PathBuf {
        self.data_dir.join("alerts.jsonl")
    }
}

fn is_writable_dir(p: &Path) -> bool {
    if std::fs::create_dir_all(p).is_err() {
        return false;
    }
    let probe = p.join(".write-test");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

fn user_data_dir() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    if cfg!(windows) {
        env("LOCALAPPDATA")
            .or_else(|| env("APPDATA"))
            .unwrap_or_else(std::env::temp_dir)
    } else if cfg!(target_os = "macos") {
        env("HOME")
            .map(|h| h.join("Library/Application Support"))
            .unwrap_or_else(std::env::temp_dir)
    } else {
        env("XDG_DATA_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".local/share")))
            .unwrap_or_else(std::env::temp_dir)
    }
}

impl Settings {
    pub fn load(paths: &Paths) -> Settings {
        std::fs::read_to_string(paths.settings_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, paths: &Paths) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(paths.settings_file(), json)
    }

    pub fn temp_limits(&self, info: &crate::model::DriveInfo) -> (i32, i32) {
        match self.alerts.temp_overrides.get(&info.key()) {
            Some(l) => (l.warn, l.crit),
            None => crate::health::default_temp_limits(info),
        }
    }
}
