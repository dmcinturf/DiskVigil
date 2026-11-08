//! SCSI / ATA Translation (SAT): ATA PASS-THROUGH (16) CDB construction and sense
//! decoding. Used for USB enclosures, SAS HBAs and Linux libata devices.

use super::{io_err, Regs};
use std::io;

pub const SENSE_LEN: usize = 32;

/// Builds an ATA PASS-THROUGH (16) CDB for a 28-bit command.
pub fn build_cdb(r: &Regs, data_in: bool, ck_cond: bool) -> [u8; 16] {
    let mut c = [0u8; 16];
    c[0] = 0x85;
    // protocol: 4 = PIO data-in, 3 = non-data
    let protocol: u8 = if data_in { 4 } else { 3 };
    c[1] = protocol << 1;
    // T_DIR=1 (from device), BYT_BLOK=1 (blocks), T_LENGTH=2 (length in COUNT)
    c[2] = if data_in { 0x0E } else { 0x00 } | if ck_cond { 0x20 } else { 0x00 };
    c[4] = r.features;
    c[6] = r.count;
    c[8] = r.lba_low;
    c[10] = r.lba_mid;
    c[12] = r.lba_high;
    c[13] = r.device & 0xF0;
    c[14] = r.command;
    c
}

/// Returns (sense key, ASC, ASCQ, ATA output registers if present).
pub fn parse_sense(sense: &[u8]) -> (u8, u8, u8, Option<Regs>) {
    if sense.len() < 8 {
        return (0, 0, 0, None);
    }
    match sense[0] & 0x7F {
        0x72 | 0x73 => {
            let (key, asc, ascq) = (sense[1] & 0x0F, sense[2], sense[3]);
            let end = (8 + sense[7] as usize).min(sense.len());
            let mut i = 8;
            while i + 1 < end {
                let (dt, dl) = (sense[i], sense[i + 1] as usize);
                if dt == 0x09 && i + 14 <= sense.len() {
                    let d = &sense[i..];
                    let regs = Regs {
                        features: d[3],
                        count: d[5],
                        lba_low: d[7],
                        lba_mid: d[9],
                        lba_high: d[11],
                        device: d[12],
                        command: d[13],
                    };
                    return (key, asc, ascq, Some(regs));
                }
                i += 2 + dl;
            }
            (key, asc, ascq, None)
        }
        0x70 | 0x71 if sense.len() >= 14 => {
            let (key, asc, ascq) = (sense[2] & 0x0F, sense[12], sense[13]);
            let regs = if asc == 0x00 && ascq == 0x1D {
                Some(Regs {
                    features: sense[3],
                    command: sense[4],
                    device: sense[5],
                    count: sense[6],
                    lba_low: sense[9],
                    lba_mid: sense[10],
                    lba_high: sense[11],
                })
            } else {
                None
            };
            (key, asc, ascq, regs)
        }
        _ => (0, 0, 0, None),
    }
}

/// Interprets the SCSI status + sense of an ATA PASS-THROUGH command.
pub fn interpret(scsi_status: u8, sense: &[u8]) -> io::Result<Option<Regs>> {
    if scsi_status == 0 {
        return Ok(None);
    }
    if scsi_status != 2 {
        return Err(io_err(format!("SCSI status 0x{scsi_status:02X}")));
    }
    let (key, asc, ascq, regs) = parse_sense(sense);
    if let Some(r) = regs {
        if r.status() & 0x01 != 0 {
            return Err(io_err(format!(
                "ATA command aborted (status 0x{:02X}, error 0x{:02X})",
                r.status(),
                r.error()
            )));
        }
        return Ok(Some(r));
    }
    match key {
        0 | 1 => Ok(None),
        5 => Err(io_err(format!(
            "device or bridge rejected ATA PASS-THROUGH (ASC {asc:02X}/{ascq:02X})"
        ))),
        _ => Err(io_err(format!(
            "SCSI sense key 0x{key:X}, ASC {asc:02X}/{ascq:02X}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdb_for_smart_read() {
        let mut r = Regs::smart(0xD0);
        r.count = 1;
        let c = build_cdb(&r, true, false);
        assert_eq!(c[0], 0x85);
        assert_eq!(c[1], 0x08);
        assert_eq!(c[2], 0x0E);
        assert_eq!(
            (c[4], c[6], c[10], c[12], c[14]),
            (0xD0, 1, 0x4F, 0xC2, 0xB0)
        );
    }

    #[test]
    fn descriptor_sense_returns_regs() {
        let mut s = [0u8; 32];
        s[0] = 0x72;
        s[1] = 0x01;
        s[2] = 0x00;
        s[3] = 0x1D;
        s[7] = 14;
        s[8] = 0x09;
        s[9] = 0x0C;
        s[17] = 0xF4; // lba mid
        s[19] = 0x2C; // lba high
        s[21] = 0x50; // status
        let r = interpret(2, &s).unwrap().unwrap();
        assert_eq!((r.lba_mid, r.lba_high), (0xF4, 0x2C));
    }
}
