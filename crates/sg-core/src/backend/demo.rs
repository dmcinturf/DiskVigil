//! Simulated drives for trying the UI, screenshots and tests without hardware
//! access. Values drift over time so charts and alerts have something to show.

use super::RawDrive;
use crate::ata::{make_attribute, FeatureChange};
use crate::model::*;
use crate::util::raw48_bytes;
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::time::Instant;

pub struct DemoBackend {
    start: Instant,
    tests: HashMap<String, (SelfTestKind, Instant)>,
    apm: HashMap<String, Option<u8>>,
}

impl Default for DemoBackend {
    fn default() -> Self {
        Self::new()
    }
}

struct DemoAttr(u8, u16, u8, u8, u8, u64);

impl DemoBackend {
    pub fn new() -> Self {
        DemoBackend {
            start: Instant::now(),
            tests: HashMap::new(),
            apm: HashMap::new(),
        }
    }

    fn minutes(&self) -> f64 {
        self.start.elapsed().as_secs_f64() / 60.0
    }

    fn wave(&self, period_min: f64, amp: f64) -> f64 {
        (self.minutes() / period_min * std::f64::consts::TAU).sin() * amp
    }

    fn test_status(&mut self, path: &str, minutes: u16) -> (u8, Vec<SelfTestEntry>, u64) {
        let mut extra = Vec::new();
        let mut status = 0x00;
        if let Some((kind, started)) = self.tests.get(path).copied() {
            let dur = minutes.max(1) as f64 * 60.0 / 20.0; // demo tests run 20x faster
            let frac = started.elapsed().as_secs_f64() / dur;
            if frac >= 1.0 {
                self.tests.remove(path);
                extra.push(SelfTestEntry {
                    kind: format!("{} offline", kind.label()),
                    status: "Completed without error".into(),
                    passed: Some(true),
                    remaining_pct: None,
                    lifetime_hours: 0,
                    failing_lba: None,
                });
            } else {
                let rem = (((1.0 - frac) * 10.0).ceil() as u8).min(9);
                status = 0xF0 | rem;
            }
        }
        (status, extra, 0)
    }

    #[allow(clippy::too_many_arguments)]
    fn ata(&mut self, path: &str, model: &str, serial: &str, media: MediaType, capacity: u64, rpm: u32, attrs: Vec<DemoAttr>, tests: Vec<SelfTestEntry>) -> RawDrive {
        let ssd = media.is_solid_state();
        let (status, mut extra, _) = self.test_status(path, if ssd { 2 } else { 20 });
        let apm = self.apm.get(path).copied().unwrap_or(Some(128));
        let attributes = attrs
            .into_iter()
            .map(|DemoAttr(id, flags, v, w, t, raw)| make_attribute(id, flags, v, w, Some(t), raw48_bytes(raw), ssd, None))
            .collect();
        for e in &mut extra {
            e.lifetime_hours = 31_000;
        }
        extra.extend(tests);
        let info = DriveInfo {
            path: path.to_string(),
            index: path.chars().last().and_then(|c| c.to_digit(10)),
            model: model.into(),
            serial: serial.into(),
            firmware: "DEMO01".into(),
            capacity_bytes: capacity,
            media,
            bus: BusType::Sata,
            rotation_rpm: Some(rpm),
            smart_supported: Some(true),
            smart_enabled: Some(true),
            volumes: vec![],
            ata: Some(AtaFeatures {
                ata_version: Some("ACS-3".into()),
                sata_max_gen: Some(3),
                sata_cur_gen: Some(3),
                lba48: true,
                ncq: true,
                ncq_depth: Some(32),
                trim: ssd,
                write_cache_supported: true,
                write_cache_enabled: true,
                apm_supported: !ssd,
                apm_enabled: apm.is_some() && !ssd,
                apm_level: if ssd { None } else { apm },
                aam_supported: false,
                self_test_supported: true,
                logical_sector_size: 512,
                physical_sector_size: 4096,
                ..Default::default()
            }),
            backend: "demo".into(),
            transport: Some("simulated".into()),
            ..Default::default()
        };
        let mut r = RawDrive::new(info);
        r.smart = SmartData::Ata(AtaSmart {
            attributes,
            overall_passed: Some(true),
            offline_status: 0x82,
            self_test_status: status,
            short_test_minutes: Some(2),
            extended_test_minutes: Some(if ssd { 85 } else { 498 }),
            conveyance_test_minutes: if ssd { None } else { Some(5) },
            self_test_supported: true,
            conveyance_supported: !ssd,
            error_logging: true,
            error_count: Some(0),
            self_test_log: extra,
            checksum_ok: true,
        });
        r
    }

    pub fn scan(&mut self) -> Vec<RawDrive> {
        let m = self.minutes();
        let hours = (m / 60.0) as u64;
        let t_hdd = (39.0 + self.wave(30.0, 3.0)) as u64;
        let t_ssd = (33.0 + self.wave(20.0, 2.0)) as u64;
        let t_bad = (47.0 + self.wave(15.0, 6.0)) as u64;
        let mut v = Vec::new();

        let mut d0 = self.ata(
            "demo0",
            "WDC WD40EFRX-68N32N0",
            "WD-WCC7K0DEMO01",
            MediaType::Hdd,
            4_000_787_030_016,
            5400,
            vec![
                DemoAttr(1, 0x2F, 200, 200, 51, 0),
                DemoAttr(3, 0x27, 186, 180, 21, 7683),
                DemoAttr(4, 0x32, 100, 100, 0, 412),
                DemoAttr(5, 0x33, 200, 200, 140, 8),
                DemoAttr(7, 0x2E, 200, 200, 0, 0),
                DemoAttr(9, 0x32, 58, 58, 0, 31_244 + hours),
                DemoAttr(10, 0x32, 100, 253, 0, 0),
                DemoAttr(12, 0x32, 100, 100, 0, 409),
                DemoAttr(192, 0x32, 200, 200, 0, 61),
                DemoAttr(193, 0x32, 200, 200, 0, 1_204),
                DemoAttr(194, 0x22, 113, 99, 0, t_hdd | (21 << 16) | (49 << 32)),
                DemoAttr(196, 0x32, 200, 200, 0, 8),
                DemoAttr(197, 0x32, 200, 200, 0, 0),
                DemoAttr(198, 0x30, 100, 253, 0, 0),
                DemoAttr(199, 0x32, 200, 200, 0, 3),
                DemoAttr(200, 0x08, 100, 253, 0, 0),
            ],
            vec![SelfTestEntry {
                kind: "Short offline".into(),
                status: "Completed without error".into(),
                passed: Some(true),
                remaining_pct: None,
                lifetime_hours: 30_910,
                failing_lba: None,
            }],
        );
        d0.info.volumes = vec!["D:".into()];
        v.push(d0);

        let written_lba = 102_400_000_000u64 + (m * 2_000_000.0) as u64;
        let mut d1 = self.ata(
            "demo1",
            "Samsung SSD 870 EVO 1TB",
            "S6PTNZ0DEMO02",
            MediaType::Ssd,
            1_000_204_886_016,
            0,
            vec![
                DemoAttr(5, 0x33, 100, 100, 10, 0),
                DemoAttr(9, 0x32, 97, 97, 0, 11_820 + hours),
                DemoAttr(12, 0x32, 99, 99, 0, 1_043),
                DemoAttr(177, 0x13, 92, 92, 0, 61),
                DemoAttr(179, 0x13, 100, 100, 10, 0),
                DemoAttr(181, 0x32, 100, 100, 10, 0),
                DemoAttr(182, 0x32, 100, 100, 10, 0),
                DemoAttr(183, 0x13, 100, 100, 10, 0),
                DemoAttr(187, 0x32, 100, 100, 0, 0),
                DemoAttr(190, 0x32, 67, 52, 0, t_ssd),
                DemoAttr(195, 0x1A, 200, 200, 0, 0),
                DemoAttr(199, 0x3E, 100, 100, 0, 0),
                DemoAttr(235, 0x12, 99, 99, 0, 37),
                DemoAttr(241, 0x32, 99, 99, 0, written_lba),
            ],
            vec![],
        );
        d1.info.volumes = vec!["C:".into()];
        v.push(d1);

        let pending = 24 + (m / 10.0) as u64;
        let mut d3 = self.ata(
            "demo3",
            "ST2000DM001-1ER164",
            "Z4Z0DEMO04",
            MediaType::Hdd,
            2_000_398_934_016,
            7200,
            vec![
                DemoAttr(1, 0x0F, 104, 99, 6, 63_204_221),
                DemoAttr(3, 0x03, 97, 96, 0, 0),
                DemoAttr(5, 0x33, 92, 92, 10, 1_224),
                DemoAttr(7, 0x0F, 85, 60, 30, 371_299_212),
                DemoAttr(9, 0x32, 47, 47, 0, 46_780 + hours),
                DemoAttr(10, 0x13, 100, 100, 97, 0),
                DemoAttr(12, 0x32, 99, 99, 20, 1_480),
                DemoAttr(187, 0x32, 1, 1, 0, 213),
                DemoAttr(188, 0x32, 100, 99, 0, 4),
                DemoAttr(194, 0x22, 53, 60, 0, t_bad),
                DemoAttr(197, 0x12, 100, 100, 0, pending),
                DemoAttr(198, 0x10, 100, 100, 0, pending),
                DemoAttr(199, 0x3E, 200, 200, 0, 0),
            ],
            vec![SelfTestEntry {
                kind: "Extended offline".into(),
                status: "Failed (read element)".into(),
                passed: Some(false),
                remaining_pct: Some(90),
                lifetime_hours: 46_501,
                failing_lba: Some(1_843_005_412),
            }],
        );
        if let SmartData::Ata(a) = &mut d3.smart {
            if a.self_test_status >> 4 != 0xF {
                a.self_test_status = 0x79; // last test: read failure, 90% remaining
            }
        }
        d3.info.volumes = vec!["E:".into()];
        d3.info.bus = BusType::Usb;
        d3.info.transport = Some("simulated SAT (USB)".into());
        v.push(d3);

        // NVMe
        let path = "demo2";
        let (cur, pct) = match self.tests.get(path).copied() {
            Some((kind, started)) => {
                let frac = started.elapsed().as_secs_f64() / 6.0;
                if frac >= 1.0 {
                    self.tests.remove(path);
                    (None, None)
                } else {
                    (Some(kind.label().to_string()), Some((frac * 100.0) as u8))
                }
            }
            None => (None, None),
        };
        let info = DriveInfo {
            path: path.into(),
            index: Some(2),
            model: "Samsung SSD 990 PRO 2TB".into(),
            serial: "S73WNJ0DEMO03".into(),
            firmware: "4B2QJXD7".into(),
            capacity_bytes: 2_000_398_934_016,
            media: MediaType::Nvme,
            bus: BusType::Nvme,
            rotation_rpm: Some(0),
            smart_supported: Some(true),
            smart_enabled: Some(true),
            volumes: vec!["F:".into()],
            nvme: Some(NvmeInfo {
                version: Some("2.0".into()),
                pci_vendor_id: 0x144D,
                namespaces: 1,
                warning_temp_c: Some(82),
                critical_temp_c: Some(85),
                self_test_supported: true,
                extended_test_minutes: Some(20),
            }),
            backend: "demo".into(),
            transport: Some("simulated NVMe".into()),
            ..Default::default()
        };
        let mut r = RawDrive::new(info);
        r.smart = SmartData::Nvme(NvmeHealth {
            critical_warning: 0,
            temperature_c: Some((44.0 + self.wave(10.0, 5.0)) as i32),
            available_spare: 100,
            spare_threshold: 10,
            percentage_used: 3,
            data_units_read: 61_204_331,
            data_units_written: 48_772_120 + (m * 1000.0) as u64,
            host_reads: 712_004_221,
            host_writes: 601_220_985,
            controller_busy_minutes: 1_402,
            power_cycles: 312,
            power_on_hours: 2_904 + hours,
            unsafe_shutdowns: 19,
            media_errors: 0,
            error_log_entries: 4,
            warning_temp_minutes: 0,
            critical_temp_minutes: 0,
            sensor_temps_c: vec![(44.0 + self.wave(10.0, 5.0)) as i32, (52.0 + self.wave(10.0, 4.0)) as i32],
            thermal_throttle_events: 0,
            thermal_throttle_seconds: 0,
            current_test: cur,
            current_test_pct: pct,
            self_test_log: vec![SelfTestEntry {
                kind: "Short".into(),
                status: "Completed without error".into(),
                passed: Some(true),
                remaining_pct: None,
                lifetime_hours: 2_870,
                failing_lba: None,
            }],
        });
        v.insert(2, r);
        v
    }

    pub fn self_test(&mut self, info: &DriveInfo, kind: SelfTestKind) -> Result<()> {
        if kind == SelfTestKind::Abort {
            self.tests.remove(&info.path);
            return Ok(());
        }
        if kind == SelfTestKind::Conveyance && info.media.is_solid_state() {
            bail!("conveyance self-test is not supported by this drive");
        }
        self.tests.insert(info.path.clone(), (kind, Instant::now()));
        Ok(())
    }

    pub fn set_feature(&mut self, info: &DriveInfo, change: FeatureChange) -> Result<()> {
        match change {
            FeatureChange::Apm(l) => {
                self.apm.insert(info.path.clone(), l);
                Ok(())
            }
            FeatureChange::Aam(_) => bail!("this drive does not support AAM"),
        }
    }
}
