//! Change-detection alert engine.
//!
//! Alerts fire on *transitions* (a counter grows, health drops, temperature
//! crosses a limit), so a degraded-but-stable drive does not spam notifications.

use crate::config::AlertSettings;
use crate::history::{tracked_name, HistoryPoint, CRITICAL_COUNTERS, NVME_CRIT_WARNING, NVME_MEDIA_ERRORS};
use crate::model::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertEvent {
    pub time: DateTime<Utc>,
    pub drive_key: String,
    pub drive_name: String,
    pub serial: String,
    pub severity: Severity,
    pub message: String,
}

#[derive(Default)]
struct DriveState {
    initialized: bool,
    health: Option<u8>,
    attrs: BTreeMap<u16, u64>,
    temp_level: u8,
    smart_failed: bool,
    life_warned: bool,
    failed_tests: usize,
}

#[derive(Default)]
pub struct AlertEngine {
    state: HashMap<String, DriveState>,
}

impl AlertEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds the baseline from the last stored history point so restarts do not
    /// forget what was already known.
    pub fn seed(&mut self, key: &str, last: &HistoryPoint, cfg: &AlertSettings) {
        let st = self.state.entry(key.to_string()).or_default();
        st.initialized = true;
        st.health = last.health;
        st.attrs = last.attrs.clone();
        st.life_warned = last.life.is_some_and(|l| l <= cfg.life_warn_pct);
        st.failed_tests = usize::MAX; // unknown: set silently on first evaluation
    }

    pub fn evaluate(&mut self, s: &DriveSnapshot, cfg: &AlertSettings, temp_limits: (i32, i32)) -> Vec<AlertEvent> {
        let mut out = Vec::new();
        if !cfg.enabled {
            return out;
        }
        let key = s.info.key();
        let st = self.state.entry(key.clone()).or_default();
        let mut push = |severity: Severity, message: String| {
            out.push(AlertEvent {
                time: s.timestamp,
                drive_key: key.clone(),
                drive_name: s.info.display_name(),
                serial: s.info.serial.clone(),
                severity,
                message,
            });
        };

        if !s.smart.is_available() {
            // Only temperature can be evaluated.
            temp_check(st, s.temperature_c, temp_limits, cfg, &mut push);
            return out;
        }

        let point = HistoryPoint::from_snapshot(s);
        let failed = s.health.status == HealthStatus::Failed
            || matches!(&s.smart, SmartData::Ata(a) if a.overall_passed == Some(false));
        if failed && !st.smart_failed {
            push(Severity::Critical, "Drive reports a SMART failure – back up your data immediately".into());
        }
        st.smart_failed = failed;

        if st.initialized {
            if let (Some(prev), Some(now)) = (st.health, s.health.health) {
                if prev.saturating_sub(now) >= cfg.health_drop.max(1) {
                    let sev = if now < 25 {
                        Severity::Critical
                    } else {
                        Severity::Warning
                    };
                    push(sev, format!("Health dropped from {prev}% to {now}%"));
                }
            }
            for &id in CRITICAL_COUNTERS {
                let (Some(&old), Some(&new)) = (st.attrs.get(&id), point.attrs.get(&id)) else {
                    continue;
                };
                if new > old {
                    if id == NVME_CRIT_WARNING {
                        let added = (new as u8) & !(old as u8);
                        for w in nvme_critical_warning_bits(added) {
                            push(Severity::Critical, format!("NVMe critical warning: {w}"));
                        }
                        continue;
                    }
                    let sev = if matches!(id, 197 | 198 | 184 | NVME_MEDIA_ERRORS) {
                        Severity::Critical
                    } else {
                        Severity::Warning
                    };
                    push(sev, format!("{} increased from {old} to {new} (+{})", tracked_name(id), new - old));
                }
            }
            if let (Some(&old), Some(&new)) = (st.attrs.get(&199), point.attrs.get(&199)) {
                if new > old {
                    push(Severity::Info, format!("Interface CRC errors increased by {} – check the cable", new - old));
                }
            }
        } else {
            // First sighting: report existing serious conditions once.
            if !failed && matches!(s.health.status, HealthStatus::Critical | HealthStatus::Poor) {
                push(s.health.status.severity(), format!("Drive health is {} ({}%)", s.health.status.label(), s.health.health.unwrap_or(0)));
            }
        }

        if let Some(life) = s.life_remaining_pct {
            if life <= cfg.life_warn_pct && !st.life_warned {
                push(Severity::Warning, format!("SSD remaining life is down to {life}%"));
                st.life_warned = true;
            }
        }

        let failed_tests = s.self_test_log().iter().filter(|e| e.passed == Some(false)).count();
        if st.initialized && st.failed_tests != usize::MAX && failed_tests > st.failed_tests {
            push(Severity::Critical, "A drive self-test has failed".into());
        }
        st.failed_tests = failed_tests;

        temp_check(st, s.temperature_c, temp_limits, cfg, &mut push);

        st.initialized = true;
        st.health = s.health.health;
        st.attrs = point.attrs;
        out
    }
}

fn temp_check(
    st: &mut DriveState,
    temp: Option<i32>,
    (warn, crit): (i32, i32),
    cfg: &AlertSettings,
    push: &mut impl FnMut(Severity, String),
) {
    let Some(t) = temp else { return };
    if !cfg.temperature_alerts {
        return;
    }
    let level = if t >= crit {
        2
    } else if t >= warn {
        1
    } else if t <= warn - 3 {
        0
    } else {
        st.temp_level.min(1) // hysteresis band
    };
    if level > st.temp_level {
        if level == 2 {
            push(Severity::Critical, format!("Temperature {t} °C reached the critical limit ({crit} °C)"));
        } else {
            push(Severity::Warning, format!("Temperature {t} °C exceeds the warning limit ({warn} °C)"));
        }
    }
    st.temp_level = level;
}

pub fn append_log(path: &Path, events: &[AlertEvent]) {
    if events.is_empty() {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        for e in events {
            if let Ok(line) = serde_json::to_string(e) {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}

pub fn load_log(path: &Path, max: usize) -> Vec<AlertEvent> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut v: Vec<AlertEvent> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    if v.len() > max {
        v.drain(..v.len() - max);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ata::make_attribute;
    use crate::util::raw48_bytes;

    fn snap(realloc: u64, temp: u64) -> DriveSnapshot {
        let attrs = vec![
            make_attribute(5, 0x33, 100, 100, Some(36), raw48_bytes(realloc), false, None),
            make_attribute(194, 0x22, 100, 100, Some(0), raw48_bytes(temp), false, None),
        ];
        let info = DriveInfo {
            model: "X".into(),
            serial: "1".into(),
            media: MediaType::Hdd,
            ..Default::default()
        };
        let mut s = DriveSnapshot::new(
            Utc::now(),
            info,
            SmartData::Ata(AtaSmart {
                attributes: attrs,
                overall_passed: Some(true),
                ..Default::default()
            }),
        );
        crate::health::derive(&mut s);
        s
    }

    #[test]
    fn alerts_on_transitions_only() {
        let cfg = AlertSettings::default();
        let mut e = AlertEngine::new();
        assert!(e.evaluate(&snap(0, 35), &cfg, (50, 60)).is_empty());
        assert!(e.evaluate(&snap(0, 35), &cfg, (50, 60)).is_empty());
        let ev = e.evaluate(&snap(4, 35), &cfg, (50, 60));
        assert!(ev.iter().any(|a| a.message.contains("Reallocated")), "{ev:?}");
        assert!(ev.iter().any(|a| a.message.contains("Health dropped")), "{ev:?}");
        assert!(e.evaluate(&snap(4, 35), &cfg, (50, 60)).is_empty());
        let ev = e.evaluate(&snap(4, 52), &cfg, (50, 60));
        assert_eq!(ev.len(), 1);
        // stays hot: no repeat
        assert!(e.evaluate(&snap(4, 51), &cfg, (50, 60)).is_empty());
        let ev = e.evaluate(&snap(4, 61), &cfg, (50, 60));
        assert_eq!(ev[0].severity, Severity::Critical);
    }
}
