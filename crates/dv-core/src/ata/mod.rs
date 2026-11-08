//! ATA command set (IDENTIFY DEVICE, SMART feature set, SET FEATURES) and decoders
//! for the 512-byte SMART structures. Transport-independent: platform backends only
//! have to implement [`AtaTransport`].

pub mod attributes;
pub mod sat;

use crate::model::*;
use crate::util::*;
use std::io;

pub const CMD_IDENTIFY: u8 = 0xEC;
pub const CMD_SMART: u8 = 0xB0;
pub const CMD_SET_FEATURES: u8 = 0xEF;
pub const SMART_READ_DATA: u8 = 0xD0;
pub const SMART_READ_THRESHOLDS: u8 = 0xD1;
pub const SMART_EXEC_OFFLINE: u8 = 0xD4;
pub const SMART_READ_LOG: u8 = 0xD5;
pub const SMART_RETURN_STATUS: u8 = 0xDA;

/// ATA task-file registers. For output registers `features` holds ERROR and
/// `command` holds STATUS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Regs {
    pub features: u8,
    pub count: u8,
    pub lba_low: u8,
    pub lba_mid: u8,
    pub lba_high: u8,
    pub device: u8,
    pub command: u8,
}

impl Regs {
    pub fn smart(sub: u8) -> Self {
        Regs {
            features: sub,
            lba_mid: 0x4F,
            lba_high: 0xC2,
            device: 0xA0,
            command: CMD_SMART,
            ..Default::default()
        }
    }
    pub fn error(&self) -> u8 {
        self.features
    }
    pub fn status(&self) -> u8 {
        self.command
    }
}

/// Something that can send an ATA command to a device.
pub trait AtaTransport {
    fn name(&self) -> &'static str;
    /// Executes a command. `data` = `Some` means a single 512-byte PIO data-in
    /// transfer. Returns the output registers when the transport can report them.
    fn exec(&mut self, regs: &Regs, data: Option<&mut [u8; 512]>) -> io::Result<Option<Regs>>;
}

pub(crate) fn io_err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

fn check(out: Option<Regs>) -> io::Result<Option<Regs>> {
    if let Some(o) = out {
        if o.status() & 0x01 != 0 {
            return Err(io_err(format!(
                "ATA command aborted (status 0x{:02X}, error 0x{:02X})",
                o.status(),
                o.error()
            )));
        }
    }
    Ok(out)
}

pub fn identify(t: &mut dyn AtaTransport) -> io::Result<[u8; 512]> {
    let mut buf = [0u8; 512];
    let regs = Regs {
        command: CMD_IDENTIFY,
        count: 1,
        device: 0xA0,
        ..Default::default()
    };
    check(t.exec(&regs, Some(&mut buf))?)?;
    if buf.iter().all(|&b| b == 0) || buf.iter().all(|&b| b == 0xFF) {
        return Err(io_err("IDENTIFY DEVICE returned no data"));
    }
    if buf[510] == 0xA5 {
        let sum = buf.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        if sum != 0 {
            return Err(io_err("IDENTIFY DEVICE checksum mismatch"));
        }
    }
    Ok(buf)
}

pub fn smart_read(t: &mut dyn AtaTransport, sub: u8, lba_low: u8) -> io::Result<[u8; 512]> {
    let mut buf = [0u8; 512];
    let mut r = Regs::smart(sub);
    r.count = 1;
    r.lba_low = lba_low;
    check(t.exec(&r, Some(&mut buf))?)?;
    Ok(buf)
}

/// `Some(true)` = passed, `Some(false)` = threshold exceeded, `None` = unknown.
pub fn smart_return_status(t: &mut dyn AtaTransport) -> io::Result<Option<bool>> {
    let out = check(t.exec(&Regs::smart(SMART_RETURN_STATUS), None)?)?;
    Ok(out.and_then(|o| match (o.lba_mid, o.lba_high) {
        (0x4F, 0xC2) => Some(true),
        (0xF4, 0x2C) => Some(false),
        _ => None,
    }))
}

pub fn smart_execute_offline(t: &mut dyn AtaTransport, test: u8) -> io::Result<()> {
    let mut r = Regs::smart(SMART_EXEC_OFFLINE);
    r.lba_low = test;
    check(t.exec(&r, None)?)?;
    Ok(())
}

pub fn set_features(t: &mut dyn AtaTransport, feature: u8, count: u8) -> io::Result<()> {
    let r = Regs {
        features: feature,
        count,
        device: 0xA0,
        command: CMD_SET_FEATURES,
        ..Default::default()
    };
    check(t.exec(&r, None)?)?;
    Ok(())
}

pub fn self_test_code(kind: SelfTestKind) -> u8 {
    match kind {
        SelfTestKind::Short => 0x01,
        SelfTestKind::Extended => 0x02,
        SelfTestKind::Conveyance => 0x03,
        SelfTestKind::Abort => 0x7F,
    }
}

/// Change to an ATA power / acoustic setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureChange {
    /// `None` disables APM. Levels 1..=254 (1 = max power saving, 254 = max performance).
    Apm(Option<u8>),
    /// `None` disables AAM. Levels 128 (quiet) ..= 254 (fast).
    Aam(Option<u8>),
}

pub fn apply_feature(t: &mut dyn AtaTransport, change: FeatureChange) -> io::Result<()> {
    match change {
        FeatureChange::Apm(Some(l)) => set_features(t, 0x05, l.clamp(1, 254)),
        FeatureChange::Apm(None) => set_features(t, 0x85, 0),
        FeatureChange::Aam(Some(l)) => set_features(t, 0x42, l.clamp(128, 254)),
        FeatureChange::Aam(None) => set_features(t, 0xC2, 0),
    }
}

// ----------------------------------------------------------------------------------
// IDENTIFY DEVICE
// ----------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Identify {
    pub model: String,
    pub serial: String,
    pub firmware: String,
    pub capacity_bytes: u64,
    pub rpm: Option<u32>,
    pub non_rotating: bool,
    pub smart_supported: bool,
    pub smart_enabled: bool,
    pub features: AtaFeatures,
}

pub fn parse_identify(b: &[u8; 512]) -> Identify {
    let w = |i: usize| le16(b, i * 2);
    let valid = |x: u16| x != 0 && x != 0xFFFF;

    let lba48 = w(83) & (1 << 10) != 0;
    let mut sectors = if lba48 {
        (w(100) as u64) | (w(101) as u64) << 16 | (w(102) as u64) << 32 | (w(103) as u64) << 48
    } else {
        0
    };
    if sectors == 0 {
        sectors = w(60) as u64 | (w(61) as u64) << 16;
    }

    let w106 = w(106);
    let w106_valid = w106 & 0xC000 == 0x4000;
    let mut logical = 512u32;
    if w106_valid && w106 & (1 << 12) != 0 {
        let words = w(117) as u32 | (w(118) as u32) << 16;
        if words >= 256 {
            logical = words * 2;
        }
    }
    let physical = if w106_valid && w106 & (1 << 13) != 0 {
        logical << (w106 & 0xF)
    } else {
        logical
    };

    let w217 = w(217);
    let (rpm, non_rotating) = match w217 {
        1 => (None, true),
        0x0401..=0xFFFE => (Some(w217 as u32), false),
        _ => (None, false),
    };

    let w76 = w(76);
    let sata = valid(w76);
    let sata_max_gen = if sata {
        if w76 & 0x8 != 0 {
            Some(3)
        } else if w76 & 0x4 != 0 {
            Some(2)
        } else if w76 & 0x2 != 0 {
            Some(1)
        } else {
            None
        }
    } else {
        None
    };
    let w77 = w(77);
    let sata_cur_gen = if sata && valid(w77) {
        match (w77 >> 1) & 0x7 {
            g @ 1..=3 => Some(g as u8),
            _ => None,
        }
    } else {
        None
    };

    let w80 = w(80);
    let ata_version = if valid(w80) {
        (4..=15u32)
            .rev()
            .find(|bit| w80 & (1 << bit) != 0)
            .map(|bit| {
                match bit {
                    4 => "ATA/ATAPI-4",
                    5 => "ATA/ATAPI-5",
                    6 => "ATA/ATAPI-6",
                    7 => "ATA/ATAPI-7",
                    8 => "ATA8-ACS",
                    9 => "ACS-2",
                    10 => "ACS-3",
                    11 => "ACS-4",
                    12 => "ACS-5",
                    _ => "ACS-x",
                }
                .to_string()
            })
    } else {
        None
    };

    let w82 = w(82);
    let w83 = w(83);
    let w84 = w(84);
    let w85 = w(85);
    let w86 = w(86);
    let w84_valid = w84 & 0xC000 == 0x4000;
    let apm_enabled = w86 & (1 << 3) != 0;
    let aam_enabled = w86 & (1 << 9) != 0;

    let features = AtaFeatures {
        ata_version,
        sata_max_gen,
        sata_cur_gen,
        lba48,
        ncq: sata && w76 & (1 << 8) != 0,
        ncq_depth: if sata && w76 & (1 << 8) != 0 {
            Some(((w(75) & 0x1F) + 1) as u8)
        } else {
            None
        },
        trim: w(169) & 1 != 0 && valid(w(169)),
        write_cache_supported: w82 & (1 << 5) != 0,
        write_cache_enabled: w85 & (1 << 5) != 0,
        apm_supported: w83 & (1 << 3) != 0,
        apm_enabled,
        apm_level: if apm_enabled {
            Some((w(91) & 0xFF) as u8)
        } else {
            None
        },
        aam_supported: w83 & (1 << 9) != 0,
        aam_enabled,
        aam_level: if aam_enabled {
            Some((w(94) & 0xFF) as u8)
        } else {
            None
        },
        security_supported: w(128) & 1 != 0 && valid(w(128)),
        security_enabled: w(128) & 2 != 0 && valid(w(128)),
        self_test_supported: w84_valid && w84 & (1 << 1) != 0,
        logical_sector_size: logical,
        physical_sector_size: physical,
    };

    Identify {
        model: ata_string(b, 27, 47),
        serial: ata_string(b, 10, 20),
        firmware: ata_string(b, 23, 27),
        capacity_bytes: sectors * logical as u64,
        rpm,
        non_rotating,
        smart_supported: valid(w82) && w82 & 1 != 0,
        smart_enabled: valid(w85) && w85 & 1 != 0,
        features,
    }
}

pub fn apply_identify(info: &mut DriveInfo, id: &Identify) {
    if !id.model.is_empty() {
        info.model = id.model.clone();
    }
    if !id.serial.is_empty() {
        info.serial = id.serial.clone();
    }
    if !id.firmware.is_empty() {
        info.firmware = id.firmware.clone();
    }
    if id.capacity_bytes > 0 && info.capacity_bytes == 0 {
        info.capacity_bytes = id.capacity_bytes;
    }
    if id.non_rotating {
        info.media = MediaType::Ssd;
        info.rotation_rpm = Some(0);
    } else if let Some(r) = id.rpm {
        info.media = MediaType::Hdd;
        info.rotation_rpm = Some(r);
    }
    info.smart_supported = Some(id.smart_supported);
    info.smart_enabled = Some(id.smart_enabled);
    info.ata = Some(id.features.clone());
}

// ----------------------------------------------------------------------------------
// SMART data
// ----------------------------------------------------------------------------------

pub fn counter_value(id: u8, raw: u64) -> u64 {
    match id {
        188 => raw & 0xFFFF,
        5 | 9 | 10 | 12 | 184 | 187 | 196 | 197 | 198 | 199 => raw & 0xFFFF_FFFF,
        _ => raw,
    }
}

fn format_raw(id: u8, raw: &[u8; 6], raw_value: u64) -> String {
    match id {
        190 | 194 => {
            let t = raw[0];
            let (lo, hi) = (raw[2], raw[4]);
            if lo > 0 && hi >= t && lo <= t && hi < 150 {
                format!("{t} °C (min {lo} / max {hi})")
            } else {
                format!("{t} °C")
            }
        }
        9 => format!("{} h", raw_value & 0xFFFF_FFFF),
        188 if raw_value > 0xFFFF => format!("{} ({raw_value:#x})", raw_value & 0xFFFF),
        _ => raw_value.to_string(),
    }
}

/// Builds a fully classified attribute.
#[allow(clippy::too_many_arguments)]
pub fn make_attribute(
    id: u8,
    flags: u16,
    value: u8,
    worst: u8,
    threshold: Option<u8>,
    raw: [u8; 6],
    ssd: bool,
    raw_display: Option<String>,
) -> AtaAttribute {
    let meta = attributes::meta(id, ssd);
    let raw_value = raw48(&raw);
    let valid_val = |v: u8| v != 0 && v < 0xFE;
    let thr = threshold.filter(|&t| t > 0 && t < 0xFE);
    let counter = counter_value(id, raw_value);
    let counter_status = if (meta.critical || id == 199) && counter > 0 {
        AttrStatus::Warning
    } else {
        AttrStatus::Ok
    };
    let status = match thr {
        Some(t) if valid_val(value) && value <= t => AttrStatus::FailingNow,
        Some(t) if valid_val(worst) && worst <= t => AttrStatus::FailedInPast,
        _ => counter_status,
    };
    AtaAttribute {
        id,
        name: meta.name.to_string(),
        flags,
        value,
        worst,
        threshold,
        raw,
        raw_value,
        raw_display: raw_display.unwrap_or_else(|| format_raw(id, &raw, raw_value)),
        prefailure: flags & 1 != 0,
        status,
        critical: meta.critical,
    }
}

pub fn parse_smart(data: &[u8; 512], thresholds: Option<&[u8; 512]>, ssd: bool) -> AtaSmart {
    let mut attributes = Vec::new();
    for i in 0..30 {
        let o = 2 + i * 12;
        let id = data[o];
        if id == 0 {
            continue;
        }
        let flags = le16(data, o + 1);
        let mut raw = [0u8; 6];
        raw.copy_from_slice(&data[o + 5..o + 11]);
        let threshold = thresholds.and_then(|t| {
            (0..30)
                .map(|j| 2 + j * 12)
                .find(|&p| t[p] == id)
                .map(|p| t[p + 1])
        });
        attributes.push(make_attribute(
            id,
            flags,
            data[o + 3],
            data[o + 4],
            threshold,
            raw,
            ssd,
            None,
        ));
    }
    let offline_cap = data[367];
    let short = data[372] as u16;
    let ext = if data[373] == 0xFF {
        le16(data, 375)
    } else {
        data[373] as u16
    };
    let conv = data[374] as u16;
    AtaSmart {
        attributes,
        overall_passed: None,
        offline_status: data[362],
        self_test_status: data[363],
        short_test_minutes: (short > 0).then_some(short),
        extended_test_minutes: (ext > 0).then_some(ext),
        conveyance_test_minutes: (conv > 0).then_some(conv),
        self_test_supported: offline_cap & 0x10 != 0,
        conveyance_supported: offline_cap & 0x20 != 0,
        error_logging: data[370] & 1 != 0,
        error_count: None,
        self_test_log: Vec::new(),
        checksum_ok: data.iter().fold(0u8, |a, &b| a.wrapping_add(b)) == 0,
    }
}

pub fn self_test_status_text(code: u8) -> &'static str {
    match code {
        0x0 => "Completed without error",
        0x1 => "Aborted by host",
        0x2 => "Interrupted by host reset",
        0x3 => "Fatal error",
        0x4 => "Failed (unknown test element)",
        0x5 => "Failed (electrical element)",
        0x6 => "Failed (servo/seek element)",
        0x7 => "Failed (read element)",
        0x8 => "Failed (handling damage)",
        0xF => "In progress",
        _ => "Reserved",
    }
}

fn self_test_type_text(t: u8) -> String {
    match t {
        0x00 => "Offline data collection".into(),
        0x01 => "Short offline".into(),
        0x02 => "Extended offline".into(),
        0x03 => "Conveyance offline".into(),
        0x04 => "Selective offline".into(),
        0x81 => "Short captive".into(),
        0x82 => "Extended captive".into(),
        0x83 => "Conveyance captive".into(),
        0x84 => "Selective captive".into(),
        x => format!("Vendor (0x{x:02X})"),
    }
}

pub fn progress_from_status(status_byte: u8) -> SelfTestProgress {
    let code = status_byte >> 4;
    if code == 0xF {
        let rem = (status_byte & 0x0F) * 10;
        SelfTestProgress {
            running: true,
            remaining_pct: Some(rem),
            text: format!("Self-test in progress, {rem}% remaining"),
        }
    } else {
        SelfTestProgress {
            running: false,
            remaining_pct: None,
            text: format!("Last self-test: {}", self_test_status_text(code)),
        }
    }
}

pub fn parse_selftest_log(b: &[u8; 512]) -> Vec<SelfTestEntry> {
    let idx = b[508] as usize;
    let mut out = Vec::new();
    if idx == 0 || idx > 21 {
        return out;
    }
    for n in 0..21 {
        let i = (idx - 1 + 21 - n) % 21;
        let o = 2 + i * 24;
        let e = &b[o..o + 24];
        if e.iter().all(|&x| x == 0) {
            continue;
        }
        let code = e[1] >> 4;
        let lba = le32(e, 5) as u64;
        let failed = (3..=8).contains(&code);
        out.push(SelfTestEntry {
            kind: self_test_type_text(e[0]),
            status: self_test_status_text(code).to_string(),
            passed: match code {
                0 => Some(true),
                3..=8 => Some(false),
                _ => None,
            },
            remaining_pct: if code == 0xF {
                Some((e[1] & 0xF) * 10)
            } else {
                None
            },
            lifetime_hours: le16(e, 2) as u64,
            failing_lba: if failed && lba != 0xFFFF_FFFF && lba != 0 {
                Some(lba)
            } else {
                None
            },
        });
    }
    out
}

/// Reads everything we know how to read from an ATA device. Fails only when the
/// transport itself cannot talk to the drive (IDENTIFY fails), so callers can try
/// another transport.
pub fn read_all(
    t: &mut dyn AtaTransport,
    info: &mut DriveInfo,
) -> io::Result<(SmartData, Vec<String>)> {
    let id_bytes = identify(t)?;
    let id = parse_identify(&id_bytes);
    apply_identify(info, &id);
    let mut warnings = Vec::new();
    if !id.smart_supported {
        return Ok((
            SmartData::Unavailable {
                reason: "The drive does not support SMART".into(),
            },
            warnings,
        ));
    }
    if !id.smart_enabled {
        return Ok((
            SmartData::Unavailable {
                reason: "SMART is disabled on this drive".into(),
            },
            warnings,
        ));
    }
    let data = match smart_read(t, SMART_READ_DATA, 0) {
        Ok(d) => d,
        Err(e) => {
            return Ok((
                SmartData::Unavailable {
                    reason: format!("SMART READ DATA failed: {e}"),
                },
                warnings,
            ))
        }
    };
    let thr = match smart_read(t, SMART_READ_THRESHOLDS, 0) {
        Ok(d) => Some(d),
        Err(e) => {
            warnings.push(format!("Could not read SMART thresholds: {e}"));
            None
        }
    };
    let mut smart = parse_smart(&data, thr.as_ref(), info.media.is_solid_state());
    if !smart.checksum_ok {
        warnings.push("SMART data checksum mismatch (common vendor quirk)".into());
    }
    match smart_return_status(t) {
        Ok(s) => smart.overall_passed = s,
        Err(e) => warnings.push(format!("SMART RETURN STATUS failed: {e}")),
    }
    if smart.overall_passed.is_none() && thr.is_some() {
        smart.overall_passed = Some(
            !smart
                .attributes
                .iter()
                .any(|a| a.prefailure && a.status == AttrStatus::FailingNow),
        );
    }
    if smart.self_test_supported || id.features.self_test_supported {
        smart.self_test_supported = true;
        match smart_read(t, SMART_READ_LOG, 0x06) {
            Ok(log) => smart.self_test_log = parse_selftest_log(&log),
            Err(e) => warnings.push(format!("Could not read self-test log: {e}")),
        }
    }
    if smart.error_logging {
        if let Ok(log) = smart_read(t, SMART_READ_LOG, 0x01) {
            smart.error_count = Some(le16(&log, 452) as u32);
        }
    }
    Ok((SmartData::Ata(smart), warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_attr(
        buf: &mut [u8; 512],
        slot: usize,
        id: u8,
        flags: u16,
        val: u8,
        worst: u8,
        raw: u64,
    ) {
        let o = 2 + slot * 12;
        buf[o] = id;
        buf[o + 1..o + 3].copy_from_slice(&flags.to_le_bytes());
        buf[o + 3] = val;
        buf[o + 4] = worst;
        buf[o + 5..o + 11].copy_from_slice(&raw48_bytes(raw));
    }

    #[test]
    fn parses_smart_attributes_and_thresholds() {
        let mut d = [0u8; 512];
        let mut t = [0u8; 512];
        put_attr(&mut d, 0, 5, 0x33, 100, 100, 8);
        put_attr(&mut d, 1, 194, 0x22, 64, 50, 36 | (20 << 16) | (45 << 32));
        put_attr(&mut d, 2, 1, 0x2F, 30, 30, 0);
        t[2] = 5;
        t[3] = 36;
        t[14] = 1;
        t[15] = 51;
        d[372] = 2;
        d[373] = 0xFF;
        d[375] = 0x2C;
        d[376] = 0x01;
        let s = parse_smart(&d, Some(&t), false);
        assert_eq!(s.attributes.len(), 3);
        let a5 = s.attr(5).unwrap();
        assert_eq!(a5.threshold, Some(36));
        assert_eq!(a5.status, AttrStatus::Warning);
        assert!(a5.prefailure);
        assert_eq!(s.attr(194).unwrap().raw_display, "36 °C (min 20 / max 45)");
        assert_eq!(s.attr(1).unwrap().status, AttrStatus::FailingNow);
        assert_eq!(s.short_test_minutes, Some(2));
        assert_eq!(s.extended_test_minutes, Some(300));
    }

    #[test]
    fn parses_identify() {
        let mut b = [0u8; 512];
        let set = |b: &mut [u8; 512], w: usize, v: u16| {
            b[w * 2..w * 2 + 2].copy_from_slice(&v.to_le_bytes())
        };
        let model = b"WDC WD40EFRX-68N32N0                    ";
        for (i, pair) in model.chunks(2).enumerate() {
            b[54 + i * 2] = pair[1];
            b[54 + i * 2 + 1] = pair[0];
        }
        set(&mut b, 82, 0x7469);
        set(&mut b, 83, 0x7C01 | (1 << 10) | (1 << 3));
        set(&mut b, 85, 0x7469);
        set(&mut b, 86, 0xBC01 | (1 << 3));
        set(&mut b, 91, 0x80FE);
        set(&mut b, 100, 0x6DB0);
        set(&mut b, 101, 0xD1C0);
        set(&mut b, 102, 0x0001);
        set(&mut b, 217, 5400);
        set(&mut b, 76, 0x8 | 0x4 | 0x2 | (1 << 8));
        set(&mut b, 77, 3 << 1);
        let id = parse_identify(&b);
        assert_eq!(id.model, "WDC WD40EFRX-68N32N0");
        assert_eq!(id.rpm, Some(5400));
        assert!(id.smart_supported && id.smart_enabled);
        assert_eq!(id.capacity_bytes, 0x1_D1C0_6DB0u64 * 512);
        assert_eq!(id.features.sata_max_gen, Some(3));
        assert_eq!(id.features.sata_cur_gen, Some(3));
        assert_eq!(id.features.apm_level, Some(0xFE));
    }

    #[test]
    fn parses_selftest_log_newest_first() {
        let mut b = [0u8; 512];
        // entry 1: short, completed ok at 100h ; entry 2: extended, read failure at 200h
        b[2] = 0x01;
        b[3] = 0x00;
        b[4..6].copy_from_slice(&100u16.to_le_bytes());
        b[26] = 0x02;
        b[27] = 0x70;
        b[28..30].copy_from_slice(&200u16.to_le_bytes());
        b[31..35].copy_from_slice(&123456u32.to_le_bytes());
        b[508] = 2;
        let log = parse_selftest_log(&b);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].lifetime_hours, 200);
        assert_eq!(log[0].passed, Some(false));
        assert_eq!(log[0].failing_lba, Some(123456));
        assert_eq!(log[1].passed, Some(true));
    }
}
