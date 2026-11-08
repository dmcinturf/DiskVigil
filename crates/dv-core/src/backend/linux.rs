//! Linux native backend.
//!
//! * Enumeration: `/sys/block` (model, vendor, size, rotational, transport).
//! * SATA / USB / SAS: `SG_IO` with SAT ATA PASS-THROUGH(16) on `/dev/sdX`.
//! * NVMe: `NVME_IOCTL_ADMIN_CMD` on the controller character device `/dev/nvmeN`.
//! * Temperature fallback: hwmon (`nvme` and `drivetemp` drivers), works without root.

use super::{NativeBackend, RawDrive};
use crate::ata::{self, sat, AtaTransport, FeatureChange, Regs};
use crate::model::*;
use crate::nvme;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SG_IO: u32 = 0x2285;
const SG_DXFER_NONE: i32 = -1;
const SG_DXFER_FROM_DEV: i32 = -3;
const NVME_IOCTL_ADMIN_CMD: u32 = 0xC048_4E41;

#[repr(C)]
struct SgIoHdr {
    interface_id: i32,
    dxfer_direction: i32,
    cmd_len: u8,
    mx_sb_len: u8,
    iovec_count: u16,
    dxfer_len: u32,
    dxferp: *mut c_void,
    cmdp: *mut u8,
    sbp: *mut u8,
    timeout: u32,
    flags: u32,
    pack_id: i32,
    usr_ptr: *mut c_void,
    status: u8,
    masked_status: u8,
    msg_status: u8,
    sb_len_wr: u8,
    host_status: u16,
    driver_status: u16,
    resid: i32,
    duration: u32,
    info: u32,
}

#[repr(C)]
#[derive(Default)]
struct NvmeAdminCmd {
    opcode: u8,
    flags: u8,
    rsvd1: u16,
    nsid: u32,
    cdw2: u32,
    cdw3: u32,
    metadata: u64,
    addr: u64,
    metadata_len: u32,
    data_len: u32,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
    timeout_ms: u32,
    result: u32,
}

struct Fd(i32);

impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: fd was opened by us and is closed once.
        unsafe {
            libc::close(self.0);
        }
    }
}

fn open_dev(path: &str, flags: i32) -> io::Result<Fd> {
    let c =
        CString::new(path).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad path"))?;
    // SAFETY: valid NUL-terminated path.
    let fd = unsafe { libc::open(c.as_ptr(), flags | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(Fd(fd))
    }
}

struct SgSat<'a>(&'a Fd);

impl AtaTransport for SgSat<'_> {
    fn name(&self) -> &'static str {
        "SAT (SG_IO ATA PASS-THROUGH 16)"
    }

    fn exec(&mut self, r: &Regs, data: Option<&mut [u8; 512]>) -> io::Result<Option<Regs>> {
        let data_in = data.is_some();
        let mut cdb = sat::build_cdb(r, data_in, !data_in);
        let mut sense = [0u8; sat::SENSE_LEN];
        let mut buf = [0u8; 512];
        let mut hdr = SgIoHdr {
            interface_id: b'S' as i32,
            dxfer_direction: if data_in {
                SG_DXFER_FROM_DEV
            } else {
                SG_DXFER_NONE
            },
            cmd_len: 16,
            mx_sb_len: sense.len() as u8,
            iovec_count: 0,
            dxfer_len: if data_in { 512 } else { 0 },
            dxferp: if data_in {
                buf.as_mut_ptr() as *mut c_void
            } else {
                std::ptr::null_mut()
            },
            cmdp: cdb.as_mut_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: 15_000,
            flags: 0,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        };
        // SAFETY: hdr points at buffers that outlive the call.
        let rc = unsafe { libc::ioctl(self.0 .0, SG_IO as _, &mut hdr as *mut SgIoHdr) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        if hdr.host_status != 0 {
            return Err(io::Error::other(format!(
                "SG_IO host status 0x{:X}",
                hdr.host_status
            )));
        }
        let regs = sat::interpret(
            hdr.status,
            &sense[..(hdr.sb_len_wr as usize).max(8).min(sense.len())],
        )?;
        if let Some(d) = data {
            d.copy_from_slice(&buf);
        }
        Ok(regs)
    }
}

fn nvme_admin(
    fd: &Fd,
    opcode: u8,
    nsid: u32,
    cdw10: u32,
    buf: Option<&mut [u8]>,
) -> io::Result<()> {
    let mut cmd = NvmeAdminCmd {
        opcode,
        nsid,
        cdw10,
        timeout_ms: 15_000,
        ..Default::default()
    };
    if let Some(b) = buf {
        cmd.addr = b.as_mut_ptr() as u64;
        cmd.data_len = b.len() as u32;
        // SAFETY: cmd points at a buffer valid for data_len bytes.
        let rc = unsafe {
            libc::ioctl(
                fd.0,
                NVME_IOCTL_ADMIN_CMD as _,
                &mut cmd as *mut NvmeAdminCmd,
            )
        };
        return check_nvme_rc(rc);
    }
    // SAFETY: no data buffer.
    let rc = unsafe {
        libc::ioctl(
            fd.0,
            NVME_IOCTL_ADMIN_CMD as _,
            &mut cmd as *mut NvmeAdminCmd,
        )
    };
    check_nvme_rc(rc)
}

fn check_nvme_rc(rc: i32) -> io::Result<()> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else if rc > 0 {
        Err(io::Error::other(format!("NVMe status 0x{rc:X}")))
    } else {
        Ok(())
    }
}

fn nvme_get_log(fd: &Fd, log: u8, len: usize) -> io::Result<Vec<u8>> {
    let mut b = vec![0u8; len];
    let numd = (len / 4 - 1) as u32;
    nvme_admin(
        fd,
        0x02,
        0xFFFF_FFFF,
        (numd << 16) | log as u32,
        Some(&mut b),
    )?;
    Ok(b)
}

fn read_sys(p: &Path) -> Option<String> {
    fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn hwmon_temp(dev_dir: &Path) -> Option<i32> {
    for base in [dev_dir.to_path_buf(), dev_dir.join("hwmon")] {
        let Ok(rd) = fs::read_dir(&base) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name();
            if !name.to_string_lossy().starts_with("hwmon") {
                continue;
            }
            if let Some(v) =
                read_sys(&e.path().join("temp1_input")).and_then(|s| s.parse::<i32>().ok())
            {
                let c = v / 1000;
                if (1..=125).contains(&c) {
                    return Some(c);
                }
            }
        }
    }
    None
}

fn mount_map(names: &[String]) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    let Ok(text) = fs::read_to_string("/proc/mounts") else {
        return map;
    };
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(dev), Some(mnt)) = (it.next(), it.next()) else {
            continue;
        };
        let Some(base) = dev.strip_prefix("/dev/") else {
            continue;
        };
        for n in names {
            if let Some(rest) = base.strip_prefix(n.as_str()) {
                let rest = rest.strip_prefix('p').unwrap_or(rest);
                if rest.chars().all(|c| c.is_ascii_digit()) {
                    let m = mnt.replace("\\040", " ");
                    let v = map.entry(n.clone()).or_default();
                    if !v.contains(&m) {
                        v.push(m);
                    }
                }
            }
        }
    }
    map
}

fn nvme_controller(name: &str) -> String {
    // nvme0n1 -> nvme0
    match name[4..].find('n') {
        Some(i) => name[..4 + i].to_string(),
        None => name.to_string(),
    }
}

pub struct LinuxBackend;

impl LinuxBackend {
    pub fn new() -> Self {
        LinuxBackend
    }

    fn block_devices() -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir("/sys/block")
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        v.retain(|n| {
            let skip = [
                "loop", "ram", "zram", "dm-", "md", "sr", "fd", "nbd", "zd", "rbd",
            ];
            !skip.iter().any(|p| n.starts_with(p)) && !(n.starts_with("nvme") && n.contains('c'))
        });
        v.sort();
        v
    }

    fn read_nvme(raw: &mut RawDrive, name: &str) {
        let ctrl = format!("/dev/{}", nvme_controller(name));
        let fd = match open_dev(&ctrl, libc::O_RDONLY) {
            Ok(f) => f,
            Err(e) => {
                raw.smart = SmartData::Unavailable {
                    reason: permission_reason(&ctrl, &e),
                };
                return;
            }
        };
        let mut id = vec![0u8; 4096];
        match nvme_admin(&fd, 0x06, 0, 1, Some(&mut id)) {
            Ok(()) => nvme::apply_identify(&mut raw.info, &nvme::parse_identify(&id)),
            Err(e) => raw.warnings.push(format!("NVMe Identify failed: {e}")),
        }
        match nvme_get_log(&fd, nvme::LOG_SMART, 512) {
            Ok(log) => {
                let mut h = nvme::parse_smart_log(&log);
                if raw
                    .info
                    .nvme
                    .as_ref()
                    .is_some_and(|n| n.self_test_supported)
                {
                    if let Ok(st) = nvme_get_log(&fd, nvme::LOG_SELF_TEST, nvme::SELF_TEST_LOG_LEN)
                    {
                        let (cur, pct, entries) = nvme::parse_selftest_log(&st);
                        h.current_test = cur;
                        h.current_test_pct = pct;
                        h.self_test_log = entries;
                    }
                }
                raw.smart = SmartData::Nvme(h);
                raw.info.transport = Some("NVMe admin ioctl".into());
            }
            Err(e) => {
                raw.smart = SmartData::Unavailable {
                    reason: permission_reason(&ctrl, &e),
                }
            }
        }
    }

    fn read_ata(raw: &mut RawDrive) {
        let fd = match open_dev(&raw.info.path, libc::O_RDWR)
            .or_else(|_| open_dev(&raw.info.path, libc::O_RDONLY))
        {
            Ok(f) => f,
            Err(e) => {
                raw.smart = SmartData::Unavailable {
                    reason: permission_reason(&raw.info.path, &e),
                };
                return;
            }
        };
        let mut t = SgSat(&fd);
        match ata::read_all(&mut t, &mut raw.info) {
            Ok((smart, w)) => {
                raw.smart = smart;
                raw.warnings.extend(w);
                raw.info.transport = Some(t.name().into());
            }
            Err(e) => {
                raw.smart = SmartData::Unavailable {
                    reason: if e.raw_os_error() == Some(libc::EPERM)
                        || e.raw_os_error() == Some(libc::EACCES)
                    {
                        permission_reason(&raw.info.path, &e)
                    } else {
                        format!("SMART not accessible via SAT: {e}")
                    },
                };
            }
        }
    }
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn permission_reason(path: &str, e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::EACCES) | Some(libc::EPERM) => {
            format!("root privileges are required to read SMART data from {path} (run with sudo)")
        }
        _ => format!("cannot access {path}: {e}"),
    }
}

impl NativeBackend for LinuxBackend {
    fn scan(&mut self) -> Result<Vec<RawDrive>> {
        let names = Self::block_devices();
        let mounts = mount_map(&names);
        let mut out = Vec::new();
        for (idx, name) in names.iter().enumerate() {
            let sys = PathBuf::from("/sys/block").join(name);
            let sectors: u64 = read_sys(&sys.join("size"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if sectors == 0 {
                continue;
            }
            let dev_dir = sys.join("device");
            let real = fs::canonicalize(&sys)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let is_nvme = name.starts_with("nvme");
            let bus = if is_nvme {
                BusType::Nvme
            } else if real.contains("/usb") {
                BusType::Usb
            } else if real.contains("/ata") {
                BusType::Sata
            } else if real.contains("virtio") || name.starts_with("vd") || name.starts_with("xvd") {
                BusType::Virtual
            } else if name.starts_with("mmcblk") {
                BusType::Mmc
            } else {
                BusType::Scsi
            };
            let rotational = read_sys(&sys.join("queue/rotational")).as_deref() == Some("1");
            let info = DriveInfo {
                path: format!("/dev/{name}"),
                index: Some(idx as u32),
                model: read_sys(&dev_dir.join("model")).unwrap_or_default(),
                serial: read_sys(&dev_dir.join("serial")).unwrap_or_default(),
                firmware: read_sys(&dev_dir.join("firmware_rev"))
                    .or_else(|| read_sys(&dev_dir.join("rev")))
                    .unwrap_or_default(),
                vendor: read_sys(&dev_dir.join("vendor")).filter(|v| v != "ATA"),
                capacity_bytes: sectors * 512,
                media: if is_nvme {
                    MediaType::Nvme
                } else if rotational {
                    MediaType::Hdd
                } else {
                    MediaType::Ssd
                },
                bus,
                removable: read_sys(&sys.join("removable")).as_deref() == Some("1"),
                volumes: mounts.get(name).cloned().unwrap_or_default(),
                backend: "native".into(),
                ..Default::default()
            };
            let mut raw = RawDrive::new(info);
            raw.fallback_temp = hwmon_temp(&dev_dir);
            match bus {
                BusType::Nvme => Self::read_nvme(&mut raw, name),
                BusType::Virtual | BusType::Mmc => {
                    raw.smart = SmartData::Unavailable {
                        reason: "This device type does not provide SMART data".into(),
                    }
                }
                _ => Self::read_ata(&mut raw),
            }
            out.push(raw);
        }
        Ok(out)
    }

    fn self_test(&mut self, info: &DriveInfo, kind: SelfTestKind) -> Result<()> {
        if info.bus == BusType::Nvme {
            let name = info.path.trim_start_matches("/dev/");
            let ctrl = format!("/dev/{}", nvme_controller(name));
            let fd = open_dev(&ctrl, libc::O_RDONLY)
                .map_err(|e| anyhow!(permission_reason(&ctrl, &e)))?;
            let code = nvme::self_test_code(kind)
                .ok_or_else(|| anyhow!("NVMe drives have no conveyance self-test"))?;
            nvme_admin(&fd, 0x14, 0xFFFF_FFFF, code, None)?;
            return Ok(());
        }
        let fd = open_dev(&info.path, libc::O_RDWR)
            .map_err(|e| anyhow!(permission_reason(&info.path, &e)))?;
        let mut t = SgSat(&fd);
        ata::smart_execute_offline(&mut t, ata::self_test_code(kind))?;
        Ok(())
    }

    fn set_feature(&mut self, info: &DriveInfo, change: FeatureChange) -> Result<()> {
        if info.bus == BusType::Nvme {
            bail!("APM/AAM are ATA features and do not apply to NVMe drives");
        }
        let fd = open_dev(&info.path, libc::O_RDWR)
            .map_err(|e| anyhow!(permission_reason(&info.path, &e)))?;
        let mut t = SgSat(&fd);
        ata::apply_feature(&mut t, change)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn controller_name() {
        assert_eq!(super::nvme_controller("nvme0n1"), "nvme0");
        assert_eq!(super::nvme_controller("nvme12n3"), "nvme12");
    }
}
