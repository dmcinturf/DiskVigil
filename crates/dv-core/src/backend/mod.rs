//! Device access backends and the [`Engine`] that combines them.

pub mod demo;
pub mod smartctl;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;

use crate::ata::FeatureChange;
use crate::config::BackendMode;
use crate::model::*;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;

/// A drive as returned by a backend, before health evaluation.
#[derive(Debug, Clone)]
pub struct RawDrive {
    pub info: DriveInfo,
    pub smart: SmartData,
    /// Temperature from a non-SMART source (OS sensor), used when SMART has none.
    pub fallback_temp: Option<i32>,
    pub warnings: Vec<String>,
}

impl RawDrive {
    pub fn new(info: DriveInfo) -> Self {
        RawDrive {
            info,
            smart: SmartData::default(),
            fallback_temp: None,
            warnings: Vec::new(),
        }
    }
}

/// OS-native pass-through access.
pub trait NativeBackend: Send {
    fn scan(&mut self) -> Result<Vec<RawDrive>>;
    fn self_test(&mut self, info: &DriveInfo, kind: SelfTestKind) -> Result<()>;
    fn set_feature(&mut self, info: &DriveInfo, change: FeatureChange) -> Result<()>;
}

pub fn native_backend() -> Option<Box<dyn NativeBackend>> {
    #[cfg(windows)]
    {
        return Some(Box::new(windows::WindowsBackend::new()));
    }
    #[cfg(target_os = "linux")]
    {
        return Some(Box::new(linux::LinuxBackend::new()));
    }
    #[allow(unreachable_code)]
    None
}

/// Whether the process has the privileges needed for raw device access.
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        return windows::is_elevated();
    }
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions.
        return unsafe { libc::geteuid() } == 0;
    }
    #[allow(unreachable_code)]
    true
}

pub struct Engine {
    mode: BackendMode,
    native: Option<Box<dyn NativeBackend>>,
    smartctl: Option<smartctl::Smartctl>,
    demo: Option<demo::DemoBackend>,
    pub last_errors: Vec<String>,
}

impl Engine {
    pub fn new(mode: BackendMode, smartctl_path: Option<&str>) -> Self {
        let smartctl = smartctl::Smartctl::locate(smartctl_path);
        let native = match mode {
            BackendMode::Auto | BackendMode::Native => native_backend(),
            _ => None,
        };
        Engine {
            mode,
            native,
            smartctl,
            demo: (mode == BackendMode::Demo).then(demo::DemoBackend::new),
            last_errors: Vec::new(),
        }
    }

    pub fn mode(&self) -> BackendMode {
        self.mode
    }

    pub fn smartctl_path(&self) -> Option<String> {
        self.smartctl.as_ref().map(|s| s.exe.display().to_string())
    }

    pub fn has_native(&self) -> bool {
        self.native.is_some()
    }

    pub fn scan(&mut self) -> Vec<DriveSnapshot> {
        self.last_errors.clear();
        let mut raws: Vec<RawDrive> = Vec::new();
        match self.mode {
            BackendMode::Demo => {
                if let Some(d) = &mut self.demo {
                    raws = d.scan();
                }
            }
            BackendMode::Smartctl => match &self.smartctl {
                Some(sc) => match sc.scan() {
                    Ok(v) => raws = v,
                    Err(e) => self.last_errors.push(format!("smartctl: {e:#}")),
                },
                None => self.last_errors.push("smartctl was not found (put smartctl next to DiskVigil or install smartmontools)".into()),
            },
            BackendMode::Native | BackendMode::Auto => {
                if let Some(n) = &mut self.native {
                    match n.scan() {
                        Ok(v) => raws = v,
                        Err(e) => self.last_errors.push(format!("native: {e:#}")),
                    }
                }
                if self.mode == BackendMode::Auto {
                    if let Some(sc) = &self.smartctl {
                        if raws.is_empty() {
                            match sc.scan() {
                                Ok(v) => raws = v,
                                Err(e) => self.last_errors.push(format!("smartctl: {e:#}")),
                            }
                        } else {
                            for r in raws.iter_mut().filter(|r| !r.smart.is_available()) {
                                let dev = smartctl::device_name_for(&r.info);
                                if let Ok(x) = sc.read(&dev, None) {
                                    if x.smart.is_available() {
                                        merge_smartctl(r, x);
                                    }
                                }
                            }
                        }
                    }
                } else if self.native.is_none() {
                    self.last_errors.push("No native backend on this platform; use the smartctl backend".into());
                }
            }
        }
        let now = Utc::now();
        raws.into_iter()
            .map(|r| {
                let mut s = DriveSnapshot::new(now, r.info, r.smart);
                s.temperature_c = r.fallback_temp;
                s.warnings = r.warnings;
                crate::health::derive(&mut s);
                s
            })
            .collect()
    }

    pub fn self_test(&mut self, info: &DriveInfo, kind: SelfTestKind) -> Result<()> {
        match info.backend.as_str() {
            "demo" => self
                .demo
                .as_mut()
                .ok_or_else(|| anyhow!("demo backend not active"))?
                .self_test(info, kind),
            "smartctl" => {
                let sc = self
                    .smartctl
                    .as_ref()
                    .ok_or_else(|| anyhow!("smartctl not available"))?;
                sc.self_test(info, kind)
            }
            _ => match &mut self.native {
                Some(n) => n.self_test(info, kind),
                None => bail!("no native backend available"),
            },
        }
    }

    pub fn set_feature(&mut self, info: &DriveInfo, change: FeatureChange) -> Result<()> {
        match info.backend.as_str() {
            "demo" => self
                .demo
                .as_mut()
                .ok_or_else(|| anyhow!("demo backend not active"))?
                .set_feature(info, change),
            "native" => match &mut self.native {
                Some(n) => n.set_feature(info, change),
                None => bail!("no native backend available"),
            },
            _ => bail!("APM/AAM changes require the native backend"),
        }
    }
}

fn merge_smartctl(r: &mut RawDrive, x: RawDrive) {
    let i = &mut r.info;
    if i.model.is_empty() {
        i.model = x.info.model;
    }
    if i.serial.is_empty() {
        i.serial = x.info.serial;
    }
    if i.firmware.is_empty() {
        i.firmware = x.info.firmware;
    }
    if i.capacity_bytes == 0 {
        i.capacity_bytes = x.info.capacity_bytes;
    }
    if x.info.media != MediaType::Unknown {
        i.media = x.info.media;
    }
    i.rotation_rpm = i.rotation_rpm.or(x.info.rotation_rpm);
    i.ata = i.ata.take().or(x.info.ata);
    i.nvme = i.nvme.take().or(x.info.nvme);
    i.smart_supported = x.info.smart_supported;
    i.smart_enabled = x.info.smart_enabled;
    i.backend = "smartctl".into();
    i.transport = x.info.transport;
    i.smartctl_device = x.info.smartctl_device;
    i.smartctl_type = x.info.smartctl_type;
    r.smart = x.smart;
    r.fallback_temp = x.fallback_temp.or(r.fallback_temp);
    r.warnings.extend(x.warnings);
}
