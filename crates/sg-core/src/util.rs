//! Small byte-level and formatting helpers.

pub fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

pub fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub fn le64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// Reads a 128-bit little-endian counter, saturating to `u64::MAX` if it does not fit.
pub fn le128_sat(b: &[u8], o: usize) -> u64 {
    if le64(b, o + 8) != 0 {
        u64::MAX
    } else {
        le64(b, o)
    }
}

pub fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// 48-bit SMART raw value.
pub fn raw48(raw: &[u8; 6]) -> u64 {
    let mut v = [0u8; 8];
    v[..6].copy_from_slice(raw);
    u64::from_le_bytes(v)
}

pub fn raw48_bytes(v: u64) -> [u8; 6] {
    let b = v.to_le_bytes();
    [b[0], b[1], b[2], b[3], b[4], b[5]]
}

/// NUL-terminated, space-padded ASCII to a trimmed `String`.
pub fn ascii_trim(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end])
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// ATA IDENTIFY strings are stored with the two bytes of every word swapped.
pub fn ata_string(id: &[u8], start_word: usize, end_word: usize) -> String {
    let mut v = Vec::with_capacity((end_word - start_word) * 2);
    for w in start_word..end_word {
        v.push(id[w * 2 + 1]);
        v.push(id[w * 2]);
    }
    ascii_trim(&v)
}

/// Decimal (vendor-style) byte formatting: 1 TB = 10^12 bytes.
pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[i])
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

pub fn format_hours(h: u64) -> String {
    let y = h / 8760;
    let d = (h % 8760) / 24;
    let hr = h % 24;
    if y > 0 {
        format!("{h} h ({y}y {d}d {hr}h)")
    } else if d > 0 {
        format!("{h} h ({d}d {hr}h)")
    } else {
        format!("{h} h")
    }
}

pub fn format_temp(c: i32, fahrenheit: bool) -> String {
    if fahrenheit {
        format!("{} °F", c * 9 / 5 + 32)
    } else {
        format!("{c} °C")
    }
}

/// Turns any string into something safe to use as a file name / stable key.
pub fn sanitize_filename(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_us = false;
    for c in s.trim().chars() {
        let ok = c.is_ascii_alphanumeric() || c == '-' || c == '.';
        if ok {
            out.push(c);
            last_us = false;
        } else if !last_us {
            out.push('_');
            last_us = true;
        }
    }
    let t = out.trim_matches('_').to_string();
    if t.is_empty() {
        "unknown".into()
    } else {
        t
    }
}

/// Exponential "diminishing returns" penalty: 0 for n == 0, approaches `max` as n grows.
pub fn decay(n: u64, max: f64, scale: f64) -> f64 {
    if n == 0 {
        0.0
    } else {
        max * (1.0 - (-(n as f64) / scale).exp())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ata_string_swaps() {
        let mut id = [0u8; 512];
        // word 27..: "ABCD" stored as "BADC"
        id[54] = b'B';
        id[55] = b'A';
        id[56] = b'D';
        id[57] = b'C';
        assert_eq!(ata_string(&id, 27, 29), "ABCD");
    }

    #[test]
    fn bytes_format() {
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(2_000_398_934_016), "2.00 TB");
        assert_eq!(format_bytes(512_110_190_592), "512 GB");
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_filename("Samsung SSD 870 / S5Y"), "Samsung_SSD_870_S5Y");
    }
}
