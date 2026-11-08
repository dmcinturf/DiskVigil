//! smartmontools (`smartctl --json`) backend. Used on macOS, for exotic USB
//! bridges / RAID controllers, and as an automatic fallback elsewhere.

use super::RawDrive;
use crate::ata::make_attribute;
use crate::model::*;
use crate::util::raw48_bytes;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Smartctl {
    pub exe: PathBuf,
}

/// smartctl device name for a natively enumerated drive.
pub fn device_name_for(info: &DriveInfo) -> String {
    if let Some(d) = &info.smartctl_device {
        return d.clone();
    }
    if cfg!(windows) {
        if let Some(i) = info.index {
            return format!("/dev/pd{i}");
        }
    }
    info.path.clone()
}

impl Smartctl {
    pub fn locate(override_path: Option<&str>) -> Option<Self> {
        let exe_name = if cfg!(windows) { "smartctl.exe" } else { "smartctl" };
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(p) = override_path.filter(|p| !p.trim().is_empty()) {
            candidates.push(PathBuf::from(p));
        }
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
            candidates.push(dir.join(exe_name));
            candidates.push(dir.join("smartmontools").join(exe_name));
            candidates.push(dir.join("smartmontools").join("bin").join(exe_name));
        }
        if let Some(path) = std::env::var_os("PATH") {
            for d in std::env::split_paths(&path) {
                candidates.push(d.join(exe_name));
            }
        }
        if cfg!(windows) {
            for base in ["ProgramFiles", "ProgramFiles(x86)"] {
                if let Some(pf) = std::env::var_os(base) {
                    candidates.push(PathBuf::from(pf).join("smartmontools").join("bin").join(exe_name));
                }
            }
        } else {
            for d in ["/usr/sbin", "/usr/local/sbin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin", "/opt/homebrew/sbin", "/opt/local/sbin"] {
                candidates.push(PathBuf::from(d).join(exe_name));
            }
        }
        candidates.into_iter().find(|p| p.is_file()).map(|exe| Smartctl { exe })
    }

    fn run(&self, args: &[&str]) -> Result<Value> {
        let mut cmd = Command::new(&self.exe);
        cmd.args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = cmd.output().with_context(|| format!("running {}", self.exe.display()))?;
        // smartctl's exit status is a bit mask of drive conditions; rely on the JSON.
        let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| {
            anyhow!(
                "could not parse smartctl output ({e}): {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )
        })?;
        // bit 0: command line error, bit 1: device open failed
        let code = out.status.code().unwrap_or(0);
        if code & 0b11 != 0 {
            let msg = v["smartctl"]["messages"]
                .as_array()
                .and_then(|m| m.iter().filter_map(|x| x["string"].as_str()).next_back())
                .unwrap_or("device could not be opened")
                .to_string();
            bail!(msg);
        }
        Ok(v)
    }

    pub fn scan(&self) -> Result<Vec<RawDrive>> {
        let v = self.run(&["--scan-open", "-j"])?;
        let mut out = Vec::new();
        for d in v["devices"].as_array().cloned().unwrap_or_default() {
            let Some(name) = d["name"].as_str() else { continue };
            let dtype = d["type"].as_str();
            match self.read(name, dtype) {
                Ok(r) => out.push(r),
                Err(e) => {
                    let mut r = RawDrive::new(DriveInfo {
                        path: name.to_string(),
                        model: name.to_string(),
                        backend: "smartctl".into(),
                        smartctl_device: Some(name.to_string()),
                        smartctl_type: dtype.map(str::to_string),
                        ..Default::default()
                    });
                    r.smart = SmartData::Unavailable { reason: format!("{e:#}") };
                    out.push(r);
                }
            }
        }
        Ok(out)
    }

    pub fn read(&self, name: &str, dtype: Option<&str>) -> Result<RawDrive> {
        let mut args = vec!["-a", "-j"];
        if let Some(t) = dtype {
            args.push("-d");
            args.push(t);
        }
        args.push(name);
        let v = self.run(&args)?;
        Ok(parse_device(&v, name, dtype))
    }

    pub fn self_test(&self, info: &DriveInfo, kind: SelfTestKind) -> Result<()> {
        let dev = device_name_for(info);
        let t = match kind {
            SelfTestKind::Short => "short",
            SelfTestKind::Extended => "long",
            SelfTestKind::Conveyance => "conveyance",
            SelfTestKind::Abort => "",
        };
        let mut args: Vec<&str> = if kind == SelfTestKind::Abort {
            vec!["-X", "-j"]
        } else {
            vec!["-t", t, "-j"]
        };
        if let Some(dt) = info.smartctl_type.as_deref() {
            args.push("-d");
            args.push(dt);
        }
        args.push(&dev);
        let v = self.run(&args)?;
        if let Some(msgs) = v["smartctl"]["messages"].as_array() {
            if let Some(err) = msgs.iter().find(|m| m["severity"].as_str() == Some("error")) {
                bail!("{}", err["string"].as_str().unwrap_or("self-test failed to start"));
            }
        }
        Ok(())
    }
}

fn u(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

fn bus_from_type(dtype: &str, protocol: &str) -> BusType {
    let t = dtype.to_ascii_lowercase();
    if t.starts_with("nvme") || protocol == "NVMe" && !t.starts_with("snt") {
        BusType::Nvme
    } else if t.starts_with("usb") || t.starts_with("snt") {
        BusType::Usb
    } else if t.starts_with("megaraid") || t.starts_with("areca") || t.starts_with("cciss") || t.starts_with("3ware") || t.starts_with("csmi") || t.starts_with("aacraid") {
        BusType::Raid
    } else if t == "ata" || t == "sat" || protocol == "ATA" {
        BusType::Sata
    } else if t == "scsi" {
        BusType::Scsi
    } else {
        BusType::Unknown
    }
}

pub fn parse_device(v: &Value, name: &str, dtype: Option<&str>) -> RawDrive {
    let dev = &v["device"];
    let protocol = dev["protocol"].as_str().unwrap_or("");
    let actual_type = dev["type"].as_str().or(dtype).unwrap_or("").to_string();
    let rotation = v["rotation_rate"].as_u64();
    let mut info = DriveInfo {
        path: name.to_string(),
        model: v["model_name"].as_str().or(v["scsi_model_name"].as_str()).unwrap_or("").trim().to_string(),
        serial: v["serial_number"].as_str().unwrap_or("").trim().to_string(),
        firmware: v["firmware_version"].as_str().or(v["scsi_revision"].as_str()).unwrap_or("").trim().to_string(),
        vendor: v["scsi_vendor"].as_str().map(str::to_string),
        capacity_bytes: u(&v["user_capacity"]["bytes"]),
        bus: bus_from_type(&actual_type, protocol),
        rotation_rpm: rotation.map(|r| r as u32),
        smart_supported: v["smart_support"]["available"].as_bool(),
        smart_enabled: v["smart_support"]["enabled"].as_bool(),
        backend: "smartctl".into(),
        transport: Some(format!("smartctl ({})", dev["info_name"].as_str().unwrap_or(name))),
        smartctl_device: Some(name.to_string()),
        smartctl_type: (!actual_type.is_empty()).then(|| actual_type.clone()),
        ..Default::default()
    };
    info.media = if protocol == "NVMe" {
        MediaType::Nvme
    } else {
        match rotation {
            Some(0) => MediaType::Ssd,
            Some(_) => MediaType::Hdd,
            None => MediaType::Unknown,
        }
    };
    let mut raw = RawDrive::new(info);
    raw.fallback_temp = v["temperature"]["current"].as_i64().map(|t| t as i32);

    if protocol == "NVMe" {
        let l = &v["nvme_smart_health_information_log"];
        if l.is_object() {
            let st = &v["nvme_self_test_log"];
            let n = NvmeHealth {
                critical_warning: u(&l["critical_warning"]) as u8,
                temperature_c: l["temperature"].as_i64().map(|t| t as i32),
                available_spare: u(&l["available_spare"]) as u8,
                spare_threshold: u(&l["available_spare_threshold"]) as u8,
                percentage_used: u(&l["percentage_used"]).min(255) as u8,
                data_units_read: u(&l["data_units_read"]),
                data_units_written: u(&l["data_units_written"]),
                host_reads: u(&l["host_reads"]),
                host_writes: u(&l["host_writes"]),
                controller_busy_minutes: u(&l["controller_busy_time"]),
                power_cycles: u(&l["power_cycles"]),
                power_on_hours: u(&l["power_on_hours"]),
                unsafe_shutdowns: u(&l["unsafe_shutdowns"]),
                media_errors: u(&l["media_errors"]),
                error_log_entries: u(&l["num_err_log_entries"]),
                warning_temp_minutes: u(&l["warning_temp_time"]) as u32,
                critical_temp_minutes: u(&l["critical_comp_time"]) as u32,
                sensor_temps_c: l["temperature_sensors"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_i64().map(|t| t as i32)).collect())
                    .unwrap_or_default(),
                thermal_throttle_events: 0,
                thermal_throttle_seconds: 0,
                current_test: st["current_self_test_operation"]["value"]
                    .as_u64()
                    .filter(|&c| c != 0)
                    .map(|_| st["current_self_test_operation"]["string"].as_str().unwrap_or("Self-test").to_string()),
                current_test_pct: st["current_self_test_completion_percent"].as_u64().map(|p| p as u8),
                self_test_log: st["table"]
                    .as_array()
                    .map(|t| {
                        t.iter()
                            .map(|e| {
                                let res = u(&e["self_test_result"]["value"]);
                                SelfTestEntry {
                                    kind: e["self_test_code"]["string"].as_str().unwrap_or("?").to_string(),
                                    status: e["self_test_result"]["string"].as_str().unwrap_or("?").to_string(),
                                    passed: match res {
                                        0 => Some(true),
                                        5..=7 => Some(false),
                                        _ => None,
                                    },
                                    remaining_pct: None,
                                    lifetime_hours: u(&e["power_on_hours"]),
                                    failing_lba: e["lba"].as_u64(),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            raw.info.nvme = Some(NvmeInfo {
                version: v["nvme_version"]["string"].as_str().map(str::to_string),
                pci_vendor_id: u(&v["nvme_pci_vendor"]["id"]) as u16,
                namespaces: u(&v["nvme_number_of_namespaces"]) as u32,
                warning_temp_c: v["nvme_warning_composite_temperature"].as_i64().map(|t| t as i32),
                critical_temp_c: v["nvme_critical_composite_temperature"].as_i64().map(|t| t as i32),
                self_test_supported: v["nvme_optional_admin_commands"]["self_test"].as_bool().unwrap_or(false),
                extended_test_minutes: None,
            });
            raw.info.smart_supported = Some(true);
            raw.info.smart_enabled = Some(true);
            raw.smart = SmartData::Nvme(n);
        } else {
            raw.smart = SmartData::Unavailable {
                reason: "smartctl returned no NVMe health log".into(),
            };
        }
        return raw;
    }

    let table = v["ata_smart_attributes"]["table"].as_array();
    let Some(table) = table else {
        raw.smart = SmartData::Unavailable {
            reason: if protocol == "SCSI" {
                "SCSI/SAS drive: SMART attributes are not available (health status only)".into()
            } else {
                "smartctl returned no SMART attributes".into()
            },
        };
        return raw;
    };
    let ssd = raw.info.media.is_solid_state();
    let attributes = table
        .iter()
        .map(|a| {
            let thr = a["thresh"].as_u64().map(|t| t as u8);
            make_attribute(
                u(&a["id"]) as u8,
                u(&a["flags"]["value"]) as u16,
                u(&a["value"]) as u8,
                u(&a["worst"]) as u8,
                thr,
                raw48_bytes(u(&a["raw"]["value"])),
                ssd,
                a["raw"]["string"].as_str().map(str::to_string),
            )
        })
        .collect();
    let d = &v["ata_smart_data"];
    let polling = &d["self_test"]["polling_minutes"];
    let nz = |x: &Value| x.as_u64().filter(|&m| m > 0).map(|m| m as u16);
    let st_value = u(&d["self_test"]["status"]["value"]) as u8;
    let st_rem = d["self_test"]["status"]["remaining_percent"].as_u64().unwrap_or(0) as u8;
    let smart = AtaSmart {
        attributes,
        overall_passed: v["smart_status"]["passed"].as_bool(),
        offline_status: u(&d["offline_data_collection"]["status"]["value"]) as u8,
        self_test_status: if st_value >> 4 == 0xF { 0xF0 | (st_rem / 10) } else { st_value },
        short_test_minutes: nz(&polling["short"]),
        extended_test_minutes: nz(&polling["extended"]),
        conveyance_test_minutes: nz(&polling["conveyance"]),
        self_test_supported: d["capabilities"]["self_tests_supported"].as_bool().unwrap_or(false),
        conveyance_supported: d["capabilities"]["conveyance_self_test_supported"].as_bool().unwrap_or(false),
        error_logging: d["capabilities"]["error_logging_supported"].as_bool().unwrap_or(false),
        error_count: v["ata_smart_error_log"]["summary"]["count"].as_u64().map(|c| c as u32),
        self_test_log: v["ata_smart_self_test_log"]["standard"]["table"]
            .as_array()
            .map(|t| {
                t.iter()
                    .map(|e| SelfTestEntry {
                        kind: e["type"]["string"].as_str().unwrap_or("?").to_string(),
                        status: e["status"]["string"].as_str().unwrap_or("?").to_string(),
                        passed: e["status"]["passed"].as_bool(),
                        remaining_pct: e["status"]["remaining_percent"].as_u64().map(|p| p as u8),
                        lifetime_hours: u(&e["lifetime_hours"]),
                        failing_lba: e["lba"].as_u64(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        checksum_ok: true,
    };
    let speed = |s: &Value| -> Option<u8> {
        match s["sata_value"].as_u64() {
            Some(g @ 1..=3) => Some(g as u8),
            _ => None,
        }
    };
    raw.info.ata = Some(AtaFeatures {
        ata_version: v["ata_version"]["string"].as_str().map(str::to_string),
        sata_max_gen: speed(&v["interface_speed"]["max"]),
        sata_cur_gen: speed(&v["interface_speed"]["current"]),
        trim: v["trim"]["supported"].as_bool().unwrap_or(false),
        self_test_supported: smart.self_test_supported,
        logical_sector_size: u(&v["logical_block_size"]) as u32,
        physical_sector_size: u(&v["physical_block_size"]) as u32,
        ..Default::default()
    });
    raw.smart = SmartData::Ata(smart);
    raw
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ata_json() {
        let j = serde_json::json!({
            "device": {"name": "/dev/sda", "type": "sat", "protocol": "ATA", "info_name": "/dev/sda [SAT]"},
            "model_name": "Samsung SSD 870 EVO 1TB", "serial_number": "S6P", "firmware_version": "SVT01B6Q",
            "user_capacity": {"bytes": 1000204886016u64}, "rotation_rate": 0,
            "smart_support": {"available": true, "enabled": true}, "smart_status": {"passed": true},
            "temperature": {"current": 33},
            "ata_smart_attributes": {"table": [
                {"id": 5, "value": 100, "worst": 100, "thresh": 10, "flags": {"value": 51}, "raw": {"value": 0, "string": "0"}},
                {"id": 177, "value": 97, "worst": 97, "thresh": 0, "flags": {"value": 19}, "raw": {"value": 31, "string": "31"}}
            ]},
            "ata_smart_data": {"self_test": {"status": {"value": 0}, "polling_minutes": {"short": 2, "extended": 85}}, "capabilities": {"self_tests_supported": true}}
        });
        let r = parse_device(&j, "/dev/sda", None);
        assert_eq!(r.info.media, MediaType::Ssd);
        let SmartData::Ata(a) = &r.smart else { panic!() };
        assert_eq!(a.attributes.len(), 2);
        assert_eq!(a.extended_test_minutes, Some(85));
        assert_eq!(r.fallback_temp, Some(33));
    }
}
