//! Data model shared by every backend, the health engine, history and the UI.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum MediaType {
    Hdd,
    Ssd,
    Nvme,
    #[default]
    Unknown,
}

impl MediaType {
    pub fn label(self) -> &'static str {
        match self {
            MediaType::Hdd => "Hard disk (HDD)",
            MediaType::Ssd => "Solid state (SATA SSD)",
            MediaType::Nvme => "NVMe SSD",
            MediaType::Unknown => "Disk",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            MediaType::Hdd => "HDD",
            MediaType::Ssd => "SSD",
            MediaType::Nvme => "NVMe",
            MediaType::Unknown => "DISK",
        }
    }
    pub fn is_solid_state(self) -> bool {
        matches!(self, MediaType::Ssd | MediaType::Nvme)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum BusType {
    Sata,
    Ata,
    Nvme,
    Usb,
    Sas,
    Scsi,
    Raid,
    Sd,
    Mmc,
    Virtual,
    Other,
    #[default]
    Unknown,
}

impl BusType {
    pub fn label(self) -> &'static str {
        match self {
            BusType::Sata => "SATA",
            BusType::Ata => "ATA",
            BusType::Nvme => "NVMe / PCIe",
            BusType::Usb => "USB",
            BusType::Sas => "SAS",
            BusType::Scsi => "SCSI",
            BusType::Raid => "RAID",
            BusType::Sd => "SD",
            BusType::Mmc => "MMC",
            BusType::Virtual => "Virtual",
            BusType::Other => "Other",
            BusType::Unknown => "Unknown",
        }
    }
}

/// Feature information decoded from ATA IDENTIFY DEVICE.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AtaFeatures {
    pub ata_version: Option<String>,
    pub sata_max_gen: Option<u8>,
    pub sata_cur_gen: Option<u8>,
    pub lba48: bool,
    pub ncq: bool,
    pub ncq_depth: Option<u8>,
    pub trim: bool,
    pub write_cache_supported: bool,
    pub write_cache_enabled: bool,
    pub apm_supported: bool,
    pub apm_enabled: bool,
    pub apm_level: Option<u8>,
    pub aam_supported: bool,
    pub aam_enabled: bool,
    pub aam_level: Option<u8>,
    pub security_supported: bool,
    pub security_enabled: bool,
    pub self_test_supported: bool,
    pub logical_sector_size: u32,
    pub physical_sector_size: u32,
}

pub fn sata_gen_speed(gen: u8) -> &'static str {
    match gen {
        1 => "SATA I (1.5 Gb/s)",
        2 => "SATA II (3.0 Gb/s)",
        3 => "SATA III (6.0 Gb/s)",
        _ => "unknown",
    }
}

/// Controller information decoded from NVMe IDENTIFY CONTROLLER.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NvmeInfo {
    pub version: Option<String>,
    pub pci_vendor_id: u16,
    pub namespaces: u32,
    pub warning_temp_c: Option<i32>,
    pub critical_temp_c: Option<i32>,
    pub self_test_supported: bool,
    pub extended_test_minutes: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct DriveInfo {
    /// OS device path, e.g. `\\.\PhysicalDrive0` or `/dev/sda`.
    pub path: String,
    pub index: Option<u32>,
    pub model: String,
    pub serial: String,
    pub firmware: String,
    pub vendor: Option<String>,
    pub capacity_bytes: u64,
    pub media: MediaType,
    pub bus: BusType,
    pub rotation_rpm: Option<u32>,
    pub smart_supported: Option<bool>,
    pub smart_enabled: Option<bool>,
    pub removable: bool,
    pub volumes: Vec<String>,
    pub ata: Option<AtaFeatures>,
    pub nvme: Option<NvmeInfo>,
    /// Which backend produced the SMART data: "native", "smartctl" or "demo".
    pub backend: String,
    /// How the device was reached, e.g. "ATA pass-through", "SAT (ATA PASS-THROUGH 16)".
    pub transport: Option<String>,
    /// Device name / type to use when talking to smartctl.
    pub smartctl_device: Option<String>,
    pub smartctl_type: Option<String>,
}

impl DriveInfo {
    /// Stable identifier used for history files, alert state and settings.
    pub fn key(&self) -> String {
        let base = if self.serial.trim().is_empty() {
            format!("{}_{}", self.model, self.path)
        } else {
            format!("{}_{}", self.model, self.serial)
        };
        crate::util::sanitize_filename(&base)
    }

    pub fn display_name(&self) -> String {
        if self.model.trim().is_empty() {
            self.path.clone()
        } else {
            self.model.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Info => "Info",
            Severity::Warning => "Warning",
            Severity::Critical => "Critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AttrStatus {
    #[default]
    Ok,
    /// A critical counter is non-zero.
    Warning,
    /// The worst value has been at/below the threshold at some point.
    FailedInPast,
    /// The current value is at/below the threshold.
    FailingNow,
}

impl AttrStatus {
    pub fn label(self) -> &'static str {
        match self {
            AttrStatus::Ok => "OK",
            AttrStatus::Warning => "Warning",
            AttrStatus::FailedInPast => "Failed in past",
            AttrStatus::FailingNow => "FAILING NOW",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AtaAttribute {
    pub id: u8,
    pub name: String,
    pub flags: u16,
    pub value: u8,
    pub worst: u8,
    pub threshold: Option<u8>,
    pub raw: [u8; 6],
    pub raw_value: u64,
    pub raw_display: String,
    pub prefailure: bool,
    pub status: AttrStatus,
    pub critical: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SelfTestEntry {
    pub kind: String,
    pub status: String,
    /// `None` while the test is still running / was aborted (neither pass nor fail).
    pub passed: Option<bool>,
    pub remaining_pct: Option<u8>,
    pub lifetime_hours: u64,
    pub failing_lba: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct AtaSmart {
    pub attributes: Vec<AtaAttribute>,
    pub overall_passed: Option<bool>,
    pub offline_status: u8,
    /// Byte 363 of the SMART data: self-test execution status.
    pub self_test_status: u8,
    pub short_test_minutes: Option<u16>,
    pub extended_test_minutes: Option<u16>,
    pub conveyance_test_minutes: Option<u16>,
    pub self_test_supported: bool,
    pub conveyance_supported: bool,
    pub error_logging: bool,
    pub error_count: Option<u32>,
    pub self_test_log: Vec<SelfTestEntry>,
    pub checksum_ok: bool,
}

impl AtaSmart {
    pub fn attr(&self, id: u8) -> Option<&AtaAttribute> {
        self.attributes.iter().find(|a| a.id == id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct NvmeHealth {
    pub critical_warning: u8,
    pub temperature_c: Option<i32>,
    pub available_spare: u8,
    pub spare_threshold: u8,
    pub percentage_used: u8,
    /// Units of 1000 * 512 bytes.
    pub data_units_read: u64,
    pub data_units_written: u64,
    pub host_reads: u64,
    pub host_writes: u64,
    pub controller_busy_minutes: u64,
    pub power_cycles: u64,
    pub power_on_hours: u64,
    pub unsafe_shutdowns: u64,
    pub media_errors: u64,
    pub error_log_entries: u64,
    pub warning_temp_minutes: u32,
    pub critical_temp_minutes: u32,
    pub sensor_temps_c: Vec<i32>,
    pub thermal_throttle_events: u32,
    pub thermal_throttle_seconds: u32,
    pub current_test: Option<String>,
    pub current_test_pct: Option<u8>,
    pub self_test_log: Vec<SelfTestEntry>,
}

pub fn nvme_critical_warning_bits(cw: u8) -> Vec<&'static str> {
    let mut v = Vec::new();
    if cw & 0x01 != 0 {
        v.push("Available spare below threshold");
    }
    if cw & 0x02 != 0 {
        v.push("Temperature outside allowed range");
    }
    if cw & 0x04 != 0 {
        v.push("NVM subsystem reliability degraded");
    }
    if cw & 0x08 != 0 {
        v.push("Media placed in read-only mode");
    }
    if cw & 0x10 != 0 {
        v.push("Volatile memory backup device failed");
    }
    if cw & 0x20 != 0 {
        v.push("Persistent memory region read-only");
    }
    v
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SmartData {
    Ata(AtaSmart),
    Nvme(NvmeHealth),
    Unavailable { reason: String },
}

impl Default for SmartData {
    fn default() -> Self {
        SmartData::Unavailable {
            reason: "Not read yet".into(),
        }
    }
}

impl SmartData {
    pub fn is_available(&self) -> bool {
        !matches!(self, SmartData::Unavailable { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SelfTestKind {
    Short,
    Extended,
    Conveyance,
    Abort,
}

impl SelfTestKind {
    pub fn label(self) -> &'static str {
        match self {
            SelfTestKind::Short => "Short",
            SelfTestKind::Extended => "Extended",
            SelfTestKind::Conveyance => "Conveyance",
            SelfTestKind::Abort => "Abort",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SelfTestProgress {
    pub running: bool,
    pub remaining_pct: Option<u8>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Issue {
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HealthStatus {
    Excellent,
    Good,
    Fair,
    Poor,
    Critical,
    Failed,
    #[default]
    Unknown,
}

impl HealthStatus {
    pub fn from_score(h: u8) -> Self {
        match h {
            90..=100 => HealthStatus::Excellent,
            70..=89 => HealthStatus::Good,
            50..=69 => HealthStatus::Fair,
            25..=49 => HealthStatus::Poor,
            _ => HealthStatus::Critical,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            HealthStatus::Excellent => "Excellent",
            HealthStatus::Good => "Good",
            HealthStatus::Fair => "Fair",
            HealthStatus::Poor => "Poor",
            HealthStatus::Critical => "Critical",
            HealthStatus::Failed => "FAILED",
            HealthStatus::Unknown => "Unknown",
        }
    }
    pub fn severity(self) -> Severity {
        match self {
            HealthStatus::Excellent | HealthStatus::Good | HealthStatus::Unknown => Severity::Info,
            HealthStatus::Fair | HealthStatus::Poor => Severity::Warning,
            HealthStatus::Critical | HealthStatus::Failed => Severity::Critical,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct HealthReport {
    pub health: Option<u8>,
    pub performance: Option<u8>,
    pub status: HealthStatus,
    pub issues: Vec<Issue>,
}

/// One complete reading of a drive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriveSnapshot {
    pub timestamp: DateTime<Utc>,
    pub info: DriveInfo,
    pub smart: SmartData,
    pub temperature_c: Option<i32>,
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    pub host_written_bytes: Option<u64>,
    pub host_read_bytes: Option<u64>,
    pub life_remaining_pct: Option<u8>,
    pub self_test_progress: Option<SelfTestProgress>,
    pub health: HealthReport,
    /// Non-fatal problems encountered while reading the drive.
    pub warnings: Vec<String>,
}

impl DriveSnapshot {
    pub fn new(timestamp: DateTime<Utc>, info: DriveInfo, smart: SmartData) -> Self {
        DriveSnapshot {
            timestamp,
            info,
            smart,
            temperature_c: None,
            power_on_hours: None,
            power_cycles: None,
            host_written_bytes: None,
            host_read_bytes: None,
            life_remaining_pct: None,
            self_test_progress: None,
            health: HealthReport::default(),
            warnings: Vec::new(),
        }
    }

    pub fn self_test_log(&self) -> &[SelfTestEntry] {
        match &self.smart {
            SmartData::Ata(a) => &a.self_test_log,
            SmartData::Nvme(n) => &n.self_test_log,
            SmartData::Unavailable { .. } => &[],
        }
    }
}
