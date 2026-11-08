//! Windows native backend.
//!
//! * Enumeration and basic info: `IOCTL_STORAGE_QUERY_PROPERTY` (device descriptor,
//!   seek penalty, temperature) and `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`.
//! * SATA/ATA: `IOCTL_ATA_PASS_THROUGH`.
//! * USB / SCSI / SAS bridges: `IOCTL_SCSI_PASS_THROUGH` with SAT ATA PASS-THROUGH(16).
//! * NVMe: `IOCTL_STORAGE_QUERY_PROPERTY` with protocol-specific (NVMe) queries
//!   for Identify Controller and the SMART / self-test log pages (stornvme).
//!
//! The Win32 surface used is tiny, so it is declared here directly instead of
//! pulling in a bindings crate.

use super::{NativeBackend, RawDrive};
use crate::ata::{self, sat, AtaTransport, FeatureChange, Regs};
use crate::model::*;
use crate::nvme;
use crate::util::{ascii_trim, le16, le32, le64, put_u32};
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::mem::{offset_of, size_of, zeroed};
use std::ptr::null_mut;

type Handle = *mut c_void;
const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 1;
const FILE_SHARE_WRITE: u32 = 2;
const OPEN_EXISTING: u32 = 3;

const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
const IOCTL_ATA_PASS_THROUGH: u32 = 0x0004_D02C;
const IOCTL_SCSI_PASS_THROUGH: u32 = 0x0004_D004;

const PROP_DEVICE: u32 = 0;
const PROP_SEEK_PENALTY: u32 = 7;
const PROP_ADAPTER_PROTOCOL_SPECIFIC: u32 = 49;
const PROP_DEVICE_PROTOCOL_SPECIFIC: u32 = 50;
const PROP_DEVICE_TEMPERATURE: u32 = 52;

const PROTOCOL_TYPE_NVME: u32 = 3;
const NVME_DATA_TYPE_IDENTIFY: u32 = 1;
const NVME_DATA_TYPE_LOG_PAGE: u32 = 2;

const ATA_FLAGS_DRDY_REQUIRED: u16 = 0x01;
const ATA_FLAGS_DATA_IN: u16 = 0x02;

const ERROR_ACCESS_DENIED: i32 = 5;

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        sa: *mut c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn DeviceIoControl(
        h: Handle,
        code: u32,
        inbuf: *mut c_void,
        inlen: u32,
        outbuf: *mut c_void,
        outlen: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(h: Handle) -> i32;
    fn GetLogicalDrives() -> u32;
}

#[link(name = "shell32")]
extern "system" {
    fn IsUserAnAdmin() -> i32;
}

pub fn is_elevated() -> bool {
    // SAFETY: no arguments, no preconditions.
    unsafe { IsUserAnAdmin() != 0 }
}

struct Device(Handle);

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by CreateFileW and is closed exactly once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

impl Device {
    fn open(path: &str, access: u32) -> io::Result<Device> {
        let w: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `w` is a valid NUL-terminated wide string for the duration of the call.
        let h = unsafe {
            CreateFileW(
                w.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null_mut(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Device(h))
        }
    }

    /// # Safety
    /// `inp`/`out` must be valid for `in_len`/`out_len` bytes (they may alias).
    unsafe fn ioctl_raw(
        &self,
        code: u32,
        inp: *mut c_void,
        in_len: usize,
        out: *mut c_void,
        out_len: usize,
    ) -> io::Result<u32> {
        let mut ret = 0u32;
        let ok = DeviceIoControl(
            self.0,
            code,
            inp,
            in_len as u32,
            out,
            out_len as u32,
            &mut ret,
            null_mut(),
        );
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ret)
        }
    }

    /// In-place ioctl on a byte buffer.
    fn ioctl_buf(&self, code: u32, buf: &mut [u8], in_len: usize) -> io::Result<u32> {
        let p = buf.as_mut_ptr() as *mut c_void;
        // SAFETY: both pointers refer to `buf`, valid for its whole length.
        unsafe { self.ioctl_raw(code, p, in_len.min(buf.len()), p, buf.len()) }
    }

    fn query_property(&self, prop: u32, out_len: usize) -> io::Result<Vec<u8>> {
        let mut q = [0u8; 12];
        put_u32(&mut q, 0, prop);
        let mut out = vec![0u8; out_len];
        // SAFETY: `q` and `out` are valid for the given lengths.
        let n = unsafe {
            self.ioctl_raw(
                IOCTL_STORAGE_QUERY_PROPERTY,
                q.as_mut_ptr() as _,
                q.len(),
                out.as_mut_ptr() as _,
                out.len(),
            )?
        };
        out.truncate(n as usize);
        Ok(out)
    }
}

// ----------------------------------------------------------------------------------
// ATA pass-through
// ----------------------------------------------------------------------------------

#[repr(C)]
struct AtaPassThroughEx {
    length: u16,
    ata_flags: u16,
    path_id: u8,
    target_id: u8,
    lun: u8,
    reserved_uchar: u8,
    data_transfer_length: u32,
    timeout_value: u32,
    reserved_ulong: u32,
    data_buffer_offset: usize,
    previous_task_file: [u8; 8],
    current_task_file: [u8; 8],
}

#[repr(C)]
struct AtaPtBuf {
    apt: AtaPassThroughEx,
    data: [u8; 512],
}

struct WinAta<'a>(&'a Device);

impl AtaTransport for WinAta<'_> {
    fn name(&self) -> &'static str {
        "ATA pass-through"
    }

    fn exec(&mut self, r: &Regs, data: Option<&mut [u8; 512]>) -> io::Result<Option<Regs>> {
        // SAFETY: plain-old-data struct, all-zero is a valid value.
        let mut b: AtaPtBuf = unsafe { zeroed() };
        b.apt.length = size_of::<AtaPassThroughEx>() as u16;
        b.apt.ata_flags =
            ATA_FLAGS_DRDY_REQUIRED | if data.is_some() { ATA_FLAGS_DATA_IN } else { 0 };
        b.apt.data_transfer_length = if data.is_some() { 512 } else { 0 };
        b.apt.timeout_value = 15;
        b.apt.data_buffer_offset = offset_of!(AtaPtBuf, data);
        b.apt.current_task_file = [
            r.features, r.count, r.lba_low, r.lba_mid, r.lba_high, r.device, r.command, 0,
        ];
        let size = size_of::<AtaPtBuf>();
        let p = &mut b as *mut AtaPtBuf as *mut c_void;
        // SAFETY: `b` is valid for `size` bytes; in and out buffers alias as allowed.
        unsafe { self.0.ioctl_raw(IOCTL_ATA_PASS_THROUGH, p, size, p, size)? };
        if let Some(d) = data {
            d.copy_from_slice(&b.data);
        }
        let tf = b.apt.current_task_file;
        Ok(Some(Regs {
            features: tf[0],
            count: tf[1],
            lba_low: tf[2],
            lba_mid: tf[3],
            lba_high: tf[4],
            device: tf[5],
            command: tf[6],
        }))
    }
}

// ----------------------------------------------------------------------------------
// SCSI pass-through (SAT)
// ----------------------------------------------------------------------------------

#[repr(C)]
struct ScsiPassThrough {
    length: u16,
    scsi_status: u8,
    path_id: u8,
    target_id: u8,
    lun: u8,
    cdb_length: u8,
    sense_info_length: u8,
    data_in: u8,
    data_transfer_length: u32,
    timeout_value: u32,
    data_buffer_offset: usize,
    sense_info_offset: u32,
    cdb: [u8; 16],
}

#[repr(C)]
struct SptBuf {
    spt: ScsiPassThrough,
    sense: [u8; sat::SENSE_LEN],
    data: [u8; 512],
}

struct WinSat<'a>(&'a Device);

impl AtaTransport for WinSat<'_> {
    fn name(&self) -> &'static str {
        "SAT (SCSI ATA PASS-THROUGH 16)"
    }

    fn exec(&mut self, r: &Regs, data: Option<&mut [u8; 512]>) -> io::Result<Option<Regs>> {
        // SAFETY: plain-old-data struct, all-zero is a valid value.
        let mut b: SptBuf = unsafe { zeroed() };
        let data_in = data.is_some();
        b.spt.length = size_of::<ScsiPassThrough>() as u16;
        b.spt.cdb_length = 16;
        b.spt.sense_info_length = sat::SENSE_LEN as u8;
        b.spt.data_in = if data_in { 1 } else { 2 };
        b.spt.data_transfer_length = if data_in { 512 } else { 0 };
        b.spt.timeout_value = 15;
        b.spt.data_buffer_offset = offset_of!(SptBuf, data);
        b.spt.sense_info_offset = offset_of!(SptBuf, sense) as u32;
        b.spt.cdb = sat::build_cdb(r, data_in, !data_in);
        let size = size_of::<SptBuf>();
        let p = &mut b as *mut SptBuf as *mut c_void;
        // SAFETY: `b` is valid for `size` bytes; in and out buffers alias as allowed.
        unsafe {
            self.0
                .ioctl_raw(IOCTL_SCSI_PASS_THROUGH, p, size, p, size)?
        };
        let regs = sat::interpret(b.spt.scsi_status, &b.sense)?;
        if let Some(d) = data {
            d.copy_from_slice(&b.data);
        }
        Ok(regs)
    }
}

// ----------------------------------------------------------------------------------
// NVMe (stornvme protocol-specific property queries)
// ----------------------------------------------------------------------------------

fn nvme_query(
    dev: &Device,
    prop: u32,
    data_type: u32,
    value: u32,
    sub: u32,
    len: usize,
) -> io::Result<Vec<u8>> {
    const PSD: usize = 40; // sizeof(STORAGE_PROTOCOL_SPECIFIC_DATA)
    let mut buf = vec![0u8; 8 + PSD + len];
    put_u32(&mut buf, 0, prop);
    put_u32(&mut buf, 4, 0); // PropertyStandardQuery
    put_u32(&mut buf, 8, PROTOCOL_TYPE_NVME);
    put_u32(&mut buf, 12, data_type);
    put_u32(&mut buf, 16, value);
    put_u32(&mut buf, 20, sub);
    put_u32(&mut buf, 24, PSD as u32);
    put_u32(&mut buf, 28, len as u32);
    let n = buf.len();
    dev.ioctl_buf(IOCTL_STORAGE_QUERY_PROPERTY, &mut buf, n)?;
    // Output: STORAGE_PROTOCOL_DATA_DESCRIPTOR { Version, Size, ProtocolSpecificData }
    let off = le32(&buf, 8 + 16) as usize;
    let dlen = le32(&buf, 8 + 20) as usize;
    let start = 8 + off;
    if dlen == 0 || start >= buf.len() {
        return Err(io::Error::other("NVMe query returned no data"));
    }
    let end = (start + dlen.min(len)).min(buf.len());
    let mut v = buf[start..end].to_vec();
    v.resize(len, 0);
    Ok(v)
}

fn nvme_query_any(dev: &Device, data_type: u32, value: u32, len: usize) -> io::Result<Vec<u8>> {
    nvme_query(
        dev,
        PROP_ADAPTER_PROTOCOL_SPECIFIC,
        data_type,
        value,
        0,
        len,
    )
    .or_else(|_| nvme_query(dev, PROP_DEVICE_PROTOCOL_SPECIFIC, data_type, value, 0, len))
}

// ----------------------------------------------------------------------------------
// Backend
// ----------------------------------------------------------------------------------

fn bus_from(code: u32) -> BusType {
    match code {
        0x01 => BusType::Scsi,
        0x02 | 0x03 => BusType::Ata,
        0x07 => BusType::Usb,
        0x08 => BusType::Raid,
        0x0A => BusType::Sas,
        0x0B => BusType::Sata,
        0x0C => BusType::Sd,
        0x0D => BusType::Mmc,
        0x0E..=0x10 => BusType::Virtual,
        0x11 => BusType::Nvme,
        0x00 => BusType::Unknown,
        _ => BusType::Other,
    }
}

/// Physical drive number -> drive letters.
fn volume_map() -> HashMap<u32, Vec<String>> {
    let mut map: HashMap<u32, Vec<String>> = HashMap::new();
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let Ok(dev) = Device::open(&format!(r"\\.\{letter}:"), 0) else {
            continue;
        };
        let mut out = [0u8; 12];
        // SAFETY: `out` is valid for 12 bytes, no input buffer.
        if unsafe {
            dev.ioctl_raw(
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                null_mut(),
                0,
                out.as_mut_ptr() as _,
                out.len(),
            )
        }
        .is_ok()
        {
            let dev_type = le32(&out, 0);
            if dev_type == 7 {
                map.entry(le32(&out, 4))
                    .or_default()
                    .push(format!("{letter}:"));
            }
        }
    }
    map
}

fn device_temperature(dev: &Device) -> Option<i32> {
    let b = dev.query_property(PROP_DEVICE_TEMPERATURE, 512).ok()?;
    if b.len() < 28 || le16(&b, 12) == 0 {
        return None;
    }
    let t = le16(&b, 26) as i16 as i32;
    (1..=125).contains(&t).then_some(t)
}

pub struct WindowsBackend;

impl WindowsBackend {
    pub fn new() -> Self {
        WindowsBackend
    }

    fn read_ata(dev: &Device, raw: &mut RawDrive) {
        let sat_first = matches!(
            raw.info.bus,
            BusType::Usb | BusType::Scsi | BusType::Sas | BusType::Raid
        );
        let mut errors = Vec::new();
        for use_sat in [sat_first, !sat_first] {
            let mut ata_t = WinAta(dev);
            let mut sat_t = WinSat(dev);
            let t: &mut dyn AtaTransport = if use_sat { &mut sat_t } else { &mut ata_t };
            let name = t.name();
            match ata::read_all(t, &mut raw.info) {
                Ok((smart, warnings)) => {
                    raw.smart = smart;
                    raw.warnings.extend(warnings);
                    raw.info.transport = Some(name.to_string());
                    return;
                }
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        raw.smart = SmartData::Unavailable {
            reason: format!("SMART data not accessible – {}", errors.join("; ")),
        };
    }

    fn read_nvme(dev: &Device, raw: &mut RawDrive) {
        match nvme_query_any(dev, NVME_DATA_TYPE_IDENTIFY, 1, 4096) {
            Ok(id) => nvme::apply_identify(&mut raw.info, &nvme::parse_identify(&id)),
            Err(e) => raw.warnings.push(format!("NVMe Identify failed: {e}")),
        }
        match nvme_query_any(dev, NVME_DATA_TYPE_LOG_PAGE, nvme::LOG_SMART as u32, 512) {
            Ok(log) => {
                let mut h = nvme::parse_smart_log(&log);
                if let Ok(st) = nvme_query_any(
                    dev,
                    NVME_DATA_TYPE_LOG_PAGE,
                    nvme::LOG_SELF_TEST as u32,
                    nvme::SELF_TEST_LOG_LEN,
                ) {
                    let (cur, pct, entries) = nvme::parse_selftest_log(&st);
                    h.current_test = cur;
                    h.current_test_pct = pct;
                    h.self_test_log = entries;
                }
                raw.smart = SmartData::Nvme(h);
                raw.info.transport = Some("NVMe (stornvme protocol query)".into());
                raw.info.media = MediaType::Nvme;
            }
            Err(e) => {
                raw.smart = SmartData::Unavailable {
                    reason: format!("NVMe health log not accessible through this driver: {e}"),
                };
            }
        }
    }

    fn open_rw(path: &str) -> Result<Device> {
        Device::open(path, GENERIC_READ | GENERIC_WRITE).map_err(|e| {
            if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
                anyhow!("administrator rights are required")
            } else {
                anyhow!("cannot open {path}: {e}")
            }
        })
    }

    fn ata_transport<'a>(info: &DriveInfo, dev: &'a Device) -> Box<dyn AtaTransport + 'a> {
        if info
            .transport
            .as_deref()
            .is_some_and(|t| t.starts_with("SAT"))
        {
            Box::new(WinSat(dev))
        } else {
            Box::new(WinAta(dev))
        }
    }
}

impl Default for WindowsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeBackend for WindowsBackend {
    fn scan(&mut self) -> Result<Vec<RawDrive>> {
        let volumes = volume_map();
        let mut out = Vec::new();
        for i in 0..64u32 {
            let path = format!(r"\\.\PhysicalDrive{i}");
            let Ok(dev) = Device::open(&path, 0) else {
                continue;
            };
            let mut info = DriveInfo {
                path: path.clone(),
                index: Some(i),
                backend: "native".into(),
                ..Default::default()
            };
            if let Ok(d) = dev.query_property(PROP_DEVICE, 1024) {
                if d.len() >= 36 {
                    let s = |field: usize| {
                        let off = le32(&d, field) as usize;
                        if off == 0 || off >= d.len() {
                            String::new()
                        } else {
                            ascii_trim(&d[off..])
                        }
                    };
                    let vendor = s(12);
                    let product = s(16);
                    info.firmware = s(20);
                    info.serial = s(24);
                    info.bus = bus_from(le32(&d, 28));
                    info.removable = d[10] != 0;
                    info.model = if vendor.is_empty() || product.starts_with(&vendor) {
                        product
                    } else {
                        format!("{vendor} {product}")
                    };
                    info.vendor = (!vendor.is_empty()).then_some(vendor);
                }
            }
            if info.bus == BusType::Virtual {
                continue;
            }
            let mut geo = [0u8; 256];
            // SAFETY: `geo` valid for 256 bytes; no input buffer.
            if unsafe {
                dev.ioctl_raw(
                    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
                    null_mut(),
                    0,
                    geo.as_mut_ptr() as _,
                    geo.len(),
                )
            }
            .is_ok()
            {
                info.capacity_bytes = le64(&geo, 24);
            }
            info.media = if info.bus == BusType::Nvme {
                MediaType::Nvme
            } else {
                match dev.query_property(PROP_SEEK_PENALTY, 12) {
                    Ok(b) if b.len() >= 9 => {
                        if b[8] != 0 {
                            MediaType::Hdd
                        } else {
                            MediaType::Ssd
                        }
                    }
                    _ => MediaType::Unknown,
                }
            };
            info.volumes = volumes.get(&i).cloned().unwrap_or_default();
            let mut raw = RawDrive::new(info);
            raw.fallback_temp = device_temperature(&dev);
            drop(dev);

            match Self::open_rw(&path) {
                Ok(rw) => {
                    if raw.info.bus == BusType::Nvme {
                        Self::read_nvme(&rw, &mut raw);
                    } else {
                        Self::read_ata(&rw, &mut raw);
                    }
                }
                Err(e) => {
                    raw.smart = SmartData::Unavailable {
                        reason: format!("SMART not readable: {e}"),
                    };
                }
            }
            out.push(raw);
        }
        Ok(out)
    }

    fn self_test(&mut self, info: &DriveInfo, kind: SelfTestKind) -> Result<()> {
        if info.bus == BusType::Nvme || info.media == MediaType::Nvme {
            bail!("The Windows NVMe driver does not allow starting device self-tests. Install smartmontools and use the smartctl backend, or use the drive vendor's tool.");
        }
        let dev = Self::open_rw(&info.path)?;
        let mut t = Self::ata_transport(info, &dev);
        ata::smart_execute_offline(t.as_mut(), ata::self_test_code(kind))?;
        Ok(())
    }

    fn set_feature(&mut self, info: &DriveInfo, change: FeatureChange) -> Result<()> {
        if info.bus == BusType::Nvme {
            bail!("APM/AAM are ATA features and do not apply to NVMe drives");
        }
        let dev = Self::open_rw(&info.path)?;
        let mut t = Self::ata_transport(info, &dev);
        ata::apply_feature(t.as_mut(), change)?;
        Ok(())
    }
}
