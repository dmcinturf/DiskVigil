//! NVMe Identify Controller, SMART / Health Information log (02h) and Device
//! Self-test log (06h) decoders.

use crate::model::*;
use crate::util::*;

pub const LOG_SMART: u8 = 0x02;
pub const LOG_SELF_TEST: u8 = 0x06;
pub const SELF_TEST_LOG_LEN: usize = 564;

#[derive(Debug, Clone, Default)]
pub struct NvmeIdentify {
    pub model: String,
    pub serial: String,
    pub firmware: String,
    pub vendor_id: u16,
    pub version: Option<String>,
    pub oacs: u16,
    pub warning_temp_c: Option<i32>,
    pub critical_temp_c: Option<i32>,
    pub total_capacity: Option<u64>,
    pub extended_test_minutes: Option<u16>,
    pub namespaces: u32,
}

pub fn kelvin(k: u16) -> Option<i32> {
    if k == 0 || k == 0xFFFF {
        None
    } else {
        Some(k as i32 - 273)
    }
}

pub fn parse_identify(b: &[u8]) -> NvmeIdentify {
    if b.len() < 1024 {
        return NvmeIdentify::default();
    }
    let ver = le32(b, 80);
    let version = if ver != 0 {
        let ter = ver & 0xFF;
        Some(if ter != 0 {
            format!("{}.{}.{}", ver >> 16, (ver >> 8) & 0xFF, ter)
        } else {
            format!("{}.{}", ver >> 16, (ver >> 8) & 0xFF)
        })
    } else {
        None
    };
    let tnvmcap = le128_sat(b, 280);
    let edstt = le16(b, 316);
    NvmeIdentify {
        model: ascii_trim(&b[24..64]),
        serial: ascii_trim(&b[4..24]),
        firmware: ascii_trim(&b[64..72]),
        vendor_id: le16(b, 0),
        version,
        oacs: le16(b, 256),
        warning_temp_c: kelvin(le16(b, 266)),
        critical_temp_c: kelvin(le16(b, 268)),
        total_capacity: (tnvmcap > 0 && tnvmcap != u64::MAX).then_some(tnvmcap),
        extended_test_minutes: (edstt > 0).then_some(edstt),
        namespaces: le32(b, 516),
    }
}

pub fn apply_identify(info: &mut DriveInfo, id: &NvmeIdentify) {
    if !id.model.is_empty() {
        info.model = id.model.clone();
    }
    if !id.serial.is_empty() {
        info.serial = id.serial.clone();
    }
    if !id.firmware.is_empty() {
        info.firmware = id.firmware.clone();
    }
    if info.capacity_bytes == 0 {
        if let Some(c) = id.total_capacity {
            info.capacity_bytes = c;
        }
    }
    info.media = MediaType::Nvme;
    info.bus = BusType::Nvme;
    info.rotation_rpm = Some(0);
    info.smart_supported = Some(true);
    info.smart_enabled = Some(true);
    info.nvme = Some(NvmeInfo {
        version: id.version.clone(),
        pci_vendor_id: id.vendor_id,
        namespaces: id.namespaces,
        warning_temp_c: id.warning_temp_c,
        critical_temp_c: id.critical_temp_c,
        self_test_supported: id.oacs & (1 << 4) != 0,
        extended_test_minutes: id.extended_test_minutes,
    });
}

pub fn parse_smart_log(b: &[u8]) -> NvmeHealth {
    let mut h = NvmeHealth::default();
    if b.len() < 232 {
        return h;
    }
    h.critical_warning = b[0];
    h.temperature_c = kelvin(le16(b, 1));
    h.available_spare = b[3];
    h.spare_threshold = b[4];
    h.percentage_used = b[5];
    h.data_units_read = le128_sat(b, 32);
    h.data_units_written = le128_sat(b, 48);
    h.host_reads = le128_sat(b, 64);
    h.host_writes = le128_sat(b, 80);
    h.controller_busy_minutes = le128_sat(b, 96);
    h.power_cycles = le128_sat(b, 112);
    h.power_on_hours = le128_sat(b, 128);
    h.unsafe_shutdowns = le128_sat(b, 144);
    h.media_errors = le128_sat(b, 160);
    h.error_log_entries = le128_sat(b, 176);
    h.warning_temp_minutes = le32(b, 192);
    h.critical_temp_minutes = le32(b, 196);
    h.sensor_temps_c = (0..8)
        .filter_map(|i| kelvin(le16(b, 200 + i * 2)))
        .collect();
    h.thermal_throttle_events = le32(b, 216).saturating_add(le32(b, 220));
    h.thermal_throttle_seconds = le32(b, 224).saturating_add(le32(b, 228));
    h
}

fn test_code_text(code: u8) -> String {
    match code {
        0x1 => "Short".into(),
        0x2 => "Extended".into(),
        0xE => "Vendor specific".into(),
        x => format!("Code 0x{x:X}"),
    }
}

fn result_text(r: u8) -> &'static str {
    match r {
        0x0 => "Completed without error",
        0x1 => "Aborted by Device Self-test command",
        0x2 => "Aborted by controller reset",
        0x3 => "Aborted by namespace removal",
        0x4 => "Aborted by Format NVM",
        0x5 => "Fatal or unknown error",
        0x6 => "Failed (unknown segment)",
        0x7 => "Failed (one or more segments)",
        0x8 => "Aborted (unknown reason)",
        0x9 => "Aborted by sanitize",
        _ => "Reserved",
    }
}

/// Returns (current operation, completion %, entries newest first).
pub fn parse_selftest_log(b: &[u8]) -> (Option<String>, Option<u8>, Vec<SelfTestEntry>) {
    if b.len() < 4 {
        return (None, None, Vec::new());
    }
    let cur = b[0] & 0x0F;
    let current = (cur != 0).then(|| test_code_text(cur));
    let pct = current.as_ref().map(|_| b[1] & 0x7F);
    let mut entries = Vec::new();
    for i in 0..20 {
        let o = 4 + i * 28;
        if o + 28 > b.len() {
            break;
        }
        let e = &b[o..o + 28];
        let result = e[0] & 0x0F;
        if result == 0xF {
            continue;
        }
        let code = e[0] >> 4;
        entries.push(SelfTestEntry {
            kind: test_code_text(code),
            status: result_text(result).to_string(),
            passed: match result {
                0 => Some(true),
                5..=7 => Some(false),
                _ => None,
            },
            remaining_pct: None,
            lifetime_hours: le64(e, 4),
            failing_lba: if e[2] & 0x02 != 0 && (5..=7).contains(&result) {
                Some(le64(e, 16))
            } else {
                None
            },
        });
    }
    (current, pct, entries)
}

pub fn self_test_code(kind: SelfTestKind) -> Option<u32> {
    match kind {
        SelfTestKind::Short => Some(0x1),
        SelfTestKind::Extended => Some(0x2),
        SelfTestKind::Abort => Some(0xF),
        SelfTestKind::Conveyance => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_health_log() {
        let mut b = vec![0u8; 512];
        b[0] = 0x04;
        b[1..3].copy_from_slice(&318u16.to_le_bytes());
        b[3] = 100;
        b[4] = 10;
        b[5] = 3;
        b[48..56].copy_from_slice(&123_456u64.to_le_bytes());
        b[128..136].copy_from_slice(&5_000u64.to_le_bytes());
        b[160..168].copy_from_slice(&2u64.to_le_bytes());
        b[200..202].copy_from_slice(&320u16.to_le_bytes());
        let h = parse_smart_log(&b);
        assert_eq!(h.temperature_c, Some(45));
        assert_eq!(h.percentage_used, 3);
        assert_eq!(h.data_units_written, 123_456);
        assert_eq!(h.power_on_hours, 5_000);
        assert_eq!(h.media_errors, 2);
        assert_eq!(h.sensor_temps_c, vec![47]);
        assert_eq!(nvme_critical_warning_bits(h.critical_warning).len(), 1);
    }

    #[test]
    fn parses_selftest_log() {
        let mut b = vec![0u8; SELF_TEST_LOG_LEN];
        b[0] = 0x2;
        b[1] = 40;
        for i in 0..20 {
            b[4 + i * 28] = 0x0F;
        }
        b[4] = 0x10; // short, ok
        b[8..16].copy_from_slice(&777u64.to_le_bytes());
        let (cur, pct, e) = parse_selftest_log(&b);
        assert_eq!(cur.as_deref(), Some("Extended"));
        assert_eq!(pct, Some(40));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].lifetime_hours, 777);
        assert_eq!(e[0].passed, Some(true));
    }
}
