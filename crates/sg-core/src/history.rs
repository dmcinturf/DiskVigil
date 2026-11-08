//! Per-drive history (JSON Lines files) and trend analysis.

use crate::ata::counter_value;
use crate::model::*;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::PathBuf;

/// Pseudo attribute ids used to track NVMe counters in the same map as ATA ones.
pub const NVME_MEDIA_ERRORS: u16 = 1001;
pub const NVME_UNSAFE_SHUTDOWNS: u16 = 1002;
pub const NVME_ERROR_LOG: u16 = 1003;
pub const NVME_SPARE: u16 = 1004;
pub const NVME_PCT_USED: u16 = 1005;
pub const NVME_CRIT_WARNING: u16 = 1006;

const TRACKED_ATA: &[u8] = &[
    1, 5, 7, 9, 10, 12, 171, 172, 177, 183, 184, 187, 188, 193, 194, 196, 197, 198, 199, 231, 233, 241, 242,
];

/// Attributes whose increase is a reliability event worth a history point / alert.
pub const CRITICAL_COUNTERS: &[u16] = &[5, 10, 184, 187, 196, 197, 198, NVME_MEDIA_ERRORS, NVME_CRIT_WARNING];

pub fn tracked_name(id: u16) -> String {
    match id {
        NVME_MEDIA_ERRORS => "Media & data integrity errors".into(),
        NVME_UNSAFE_SHUTDOWNS => "Unsafe shutdowns".into(),
        NVME_ERROR_LOG => "Error log entries".into(),
        NVME_SPARE => "Available spare (%)".into(),
        NVME_PCT_USED => "Percentage used".into(),
        NVME_CRIT_WARNING => "Critical warning".into(),
        id if id < 256 => crate::ata::attributes::meta(id as u8, false).name.to_string(),
        id => format!("#{id}"),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryPoint {
    pub t: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perf: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poh: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub life: Option<u8>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: BTreeMap<u16, u64>,
}

impl HistoryPoint {
    pub fn from_snapshot(s: &DriveSnapshot) -> Self {
        let mut attrs = BTreeMap::new();
        match &s.smart {
            SmartData::Ata(a) => {
                for at in &a.attributes {
                    if TRACKED_ATA.contains(&at.id) || at.status != AttrStatus::Ok {
                        attrs.insert(at.id as u16, counter_value(at.id, at.raw_value));
                    }
                }
            }
            SmartData::Nvme(n) => {
                attrs.insert(NVME_MEDIA_ERRORS, n.media_errors);
                attrs.insert(NVME_UNSAFE_SHUTDOWNS, n.unsafe_shutdowns);
                attrs.insert(NVME_ERROR_LOG, n.error_log_entries);
                attrs.insert(NVME_SPARE, n.available_spare as u64);
                attrs.insert(NVME_PCT_USED, n.percentage_used as u64);
                attrs.insert(NVME_CRIT_WARNING, n.critical_warning as u64);
            }
            SmartData::Unavailable { .. } => {}
        }
        HistoryPoint {
            t: s.timestamp,
            temp: s.temperature_c,
            health: s.health.health,
            perf: s.health.performance,
            poh: s.power_on_hours,
            written: s.host_written_bytes,
            read: s.host_read_bytes,
            life: s.life_remaining_pct,
            attrs,
        }
    }

    fn significant_change(&self, other: &HistoryPoint) -> bool {
        self.health != other.health
            || CRITICAL_COUNTERS
                .iter()
                .any(|id| self.attrs.get(id) != other.attrs.get(id))
    }
}

pub struct HistoryStore {
    dir: PathBuf,
    data: HashMap<String, Vec<HistoryPoint>>,
    max_points: usize,
}

impl HistoryStore {
    pub fn open(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let mut data = HashMap::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(key) = p.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
                    continue;
                };
                let Ok(text) = std::fs::read_to_string(&p) else { continue };
                let mut pts: Vec<HistoryPoint> = text
                    .lines()
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect();
                pts.sort_by_key(|p| p.t);
                data.insert(key, pts);
            }
        }
        HistoryStore {
            dir,
            data,
            max_points: 200_000,
        }
    }

    pub fn keys(&self) -> Vec<String> {
        self.data.keys().cloned().collect()
    }

    pub fn points(&self, key: &str) -> &[HistoryPoint] {
        self.data.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn last(&self, key: &str) -> Option<&HistoryPoint> {
        self.data.get(key).and_then(|v| v.last())
    }

    /// Records a snapshot when `min_interval` has passed since the last stored point
    /// or something significant changed. Returns true if a point was written.
    pub fn record(&mut self, s: &DriveSnapshot, min_interval: Duration) -> bool {
        if !s.smart.is_available() && s.temperature_c.is_none() {
            return false;
        }
        let key = s.info.key();
        let p = HistoryPoint::from_snapshot(s);
        let due = match self.last(&key) {
            None => true,
            Some(last) => p.t - last.t >= min_interval || p.significant_change(last),
        };
        if !due {
            return false;
        }
        if let Ok(line) = serde_json::to_string(&p) {
            let path = self.dir.join(format!("{key}.jsonl"));
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{line}");
            }
        }
        let v = self.data.entry(key).or_default();
        v.push(p);
        if v.len() > self.max_points {
            let excess = v.len() - self.max_points;
            v.drain(..excess);
        }
        true
    }
}

/// Least-squares fit returning (slope, intercept).
pub fn linear_fit(pts: &[(f64, f64)]) -> Option<(f64, f64)> {
    let n = pts.len() as f64;
    if pts.len() < 2 {
        return None;
    }
    let (sx, sy) = pts.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
    let (mx, my) = (sx / n, sy / n);
    let (mut num, mut den) = (0.0, 0.0);
    for (x, y) in pts {
        num += (x - mx) * (y - my);
        den += (x - mx) * (x - mx);
    }
    if den.abs() < f64::EPSILON {
        return None;
    }
    let slope = num / den;
    Some((slope, my - slope * mx))
}

#[derive(Debug, Clone, PartialEq)]
pub struct LifeProjection {
    /// Percentage points of life consumed per day.
    pub pct_per_day: f64,
    pub end_of_life: DateTime<Utc>,
}

/// Projects when an SSD reaches 0% remaining life, from up to a year of history.
pub fn life_projection(points: &[HistoryPoint]) -> Option<LifeProjection> {
    let now = points.last()?.t;
    let since = now - Duration::days(365);
    let pts: Vec<(f64, f64)> = points
        .iter()
        .filter(|p| p.t >= since)
        .filter_map(|p| p.life.map(|l| (p.t.timestamp() as f64 / 86_400.0, l as f64)))
        .collect();
    let (first, last) = (pts.first()?, pts.last()?);
    if last.0 - first.0 < 7.0 || first.1 - last.1 < 1.0 {
        return None;
    }
    let (slope, _) = linear_fit(&pts)?;
    if slope >= 0.0 {
        return None;
    }
    let days_left = last.1 / -slope;
    if !days_left.is_finite() || days_left > 365.0 * 100.0 {
        return None;
    }
    Some(LifeProjection {
        pct_per_day: -slope,
        end_of_life: now + Duration::seconds((days_left * 86_400.0) as i64),
    })
}

/// Average bytes written per day over the last 30 days of history.
pub fn writes_per_day(points: &[HistoryPoint]) -> Option<f64> {
    let now = points.last()?.t;
    let since = now - Duration::days(30);
    let w: Vec<_> = points
        .iter()
        .filter(|p| p.t >= since)
        .filter_map(|p| p.written.map(|w| (p.t, w)))
        .collect();
    let (a, b) = (w.first()?, w.last()?);
    let days = (b.0 - a.0).num_seconds() as f64 / 86_400.0;
    if days < 1.0 || b.1 < a.1 {
        return None;
    }
    Some((b.1 - a.1) as f64 / days)
}

/// (min, avg, max) temperature since `since`.
pub fn temp_stats(points: &[HistoryPoint], since: DateTime<Utc>) -> Option<(i32, f64, i32)> {
    let t: Vec<i32> = points
        .iter()
        .filter(|p| p.t >= since)
        .filter_map(|p| p.temp)
        .collect();
    if t.is_empty() {
        return None;
    }
    let min = *t.iter().min()?;
    let max = *t.iter().max()?;
    let avg = t.iter().map(|&x| x as f64).sum::<f64>() / t.len() as f64;
    Some((min, avg, max))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_detects_wear() {
        let start = Utc::now() - Duration::days(100);
        let pts: Vec<HistoryPoint> = (0..=100)
            .map(|d| HistoryPoint {
                t: start + Duration::days(d),
                temp: None,
                health: None,
                perf: None,
                poh: None,
                written: Some(d as u64 * 10_000_000_000),
                read: None,
                life: Some((90 - d / 10) as u8),
                attrs: BTreeMap::new(),
            })
            .collect();
        let p = life_projection(&pts).unwrap();
        assert!((p.pct_per_day - 0.1).abs() < 0.02, "{p:?}");
        let wpd = writes_per_day(&pts).unwrap();
        assert!((wpd - 1e10).abs() < 1e8);
    }
}
