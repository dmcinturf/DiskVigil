//! Derives human-meaningful metrics from raw SMART data and scores drive health
//! (likelihood of data loss) and performance (things that slow the drive down).
//!
//! The scores are heuristics. They are designed to be conservative: a single
//! reallocated sector is a small deduction, while pending / uncorrectable sectors,
//! NVMe critical warnings and threshold failures weigh heavily.

use crate::ata::counter_value;
use crate::model::*;
use crate::util::decay;

/// Default (warning, critical) temperature limits in °C for a drive.
pub fn default_temp_limits(info: &DriveInfo) -> (i32, i32) {
    match info.media {
        MediaType::Hdd => (50, 60),
        MediaType::Ssd => (65, 75),
        MediaType::Nvme => {
            let n = info.nvme.as_ref();
            let warn = n
                .and_then(|n| n.warning_temp_c)
                .filter(|&t| (40..=110).contains(&t))
                .unwrap_or(70);
            let crit = n
                .and_then(|n| n.critical_temp_c)
                .filter(|&t| t > warn && t <= 125)
                .unwrap_or(warn + 10);
            (warn, crit)
        }
        MediaType::Unknown => (55, 65),
    }
}

fn plausible_temp(t: i32) -> Option<i32> {
    (1..=125).contains(&t).then_some(t)
}

fn ata_temperature(a: &AtaSmart) -> Option<i32> {
    for id in [194u8, 190] {
        if let Some(attr) = a.attr(id) {
            if let Some(t) = plausible_temp(attr.raw[0] as i32) {
                return Some(t);
            }
        }
    }
    None
}

fn ata_host_io(a: &AtaSmart, info: &DriveInfo) -> (Option<u64>, Option<u64>) {
    let model = info.model.to_uppercase();
    let unit: u64 = if model.contains("INTEL") {
        32 * 1024 * 1024
    } else if model.starts_with("KINGSTON") || model.contains("SANDFORCE") {
        1 << 30
    } else {
        512
    };
    let sane = |bytes: u64| -> Option<u64> {
        if info.capacity_bytes > 0 && bytes / 1_000_000 > info.capacity_bytes {
            None
        } else {
            Some(bytes)
        }
    };
    let written = a
        .attr(241)
        .map(|x| x.raw_value.saturating_mul(unit))
        .or_else(|| a.attr(246).map(|x| x.raw_value.saturating_mul(512)))
        .filter(|&v| v > 0)
        .and_then(sane);
    let read = a
        .attr(242)
        .map(|x| x.raw_value.saturating_mul(unit))
        .filter(|&v| v > 0)
        .and_then(sane);
    (written, read)
}

fn ata_life(a: &AtaSmart, media: MediaType) -> Option<u8> {
    if !media.is_solid_state() {
        return None;
    }
    for id in [231u8, 233, 202, 177, 169, 173] {
        if let Some(attr) = a.attr(id) {
            if (1..=100).contains(&attr.value) {
                return Some(attr.value);
            }
        }
    }
    None
}

/// Fills all derived fields (temperature, hours, I/O totals, life, progress, health).
pub fn derive(s: &mut DriveSnapshot) {
    match &s.smart {
        SmartData::Ata(a) => {
            if let Some(t) = ata_temperature(a) {
                s.temperature_c = Some(t);
            }
            s.power_on_hours = a.attr(9).map(|x| counter_value(9, x.raw_value));
            s.power_cycles = a.attr(12).map(|x| counter_value(12, x.raw_value));
            let (w, r) = ata_host_io(a, &s.info);
            s.host_written_bytes = w;
            s.host_read_bytes = r;
            s.life_remaining_pct = ata_life(a, s.info.media);
            if a.self_test_supported || a.self_test_status != 0 {
                s.self_test_progress = Some(crate::ata::progress_from_status(a.self_test_status));
            }
        }
        SmartData::Nvme(n) => {
            if let Some(t) = n.temperature_c.and_then(plausible_temp) {
                s.temperature_c = Some(t);
            }
            s.power_on_hours = Some(n.power_on_hours);
            s.power_cycles = Some(n.power_cycles);
            s.host_written_bytes = Some(n.data_units_written.saturating_mul(512_000));
            s.host_read_bytes = Some(n.data_units_read.saturating_mul(512_000));
            s.life_remaining_pct = Some(100u8.saturating_sub(n.percentage_used.min(100)));
            s.self_test_progress = Some(match &n.current_test {
                Some(t) => SelfTestProgress {
                    running: true,
                    remaining_pct: n.current_test_pct.map(|p| 100u8.saturating_sub(p)),
                    text: format!(
                        "{t} self-test in progress, {}% complete",
                        n.current_test_pct.unwrap_or(0)
                    ),
                },
                None => SelfTestProgress {
                    running: false,
                    remaining_pct: None,
                    text: match n.self_test_log.first() {
                        Some(e) => format!("Last self-test: {}", e.status),
                        None => "No self-test has been run".into(),
                    },
                },
            });
        }
        SmartData::Unavailable { .. } => {}
    }
    s.health = score(s);
}

struct Acc {
    health: f64,
    perf: f64,
    cap: f64,
    failed: bool,
    issues: Vec<Issue>,
}

impl Acc {
    fn issue(&mut self, severity: Severity, message: impl Into<String>) {
        self.issues.push(Issue {
            severity,
            message: message.into(),
        });
    }
}

pub fn score(s: &DriveSnapshot) -> HealthReport {
    let mut acc = Acc {
        health: 100.0,
        perf: 100.0,
        cap: 100.0,
        failed: false,
        issues: Vec::new(),
    };
    match &s.smart {
        SmartData::Unavailable { reason } => {
            return HealthReport {
                health: None,
                performance: None,
                status: HealthStatus::Unknown,
                issues: vec![Issue {
                    severity: Severity::Info,
                    message: reason.clone(),
                }],
            };
        }
        SmartData::Ata(a) => score_ata(a, s, &mut acc),
        SmartData::Nvme(n) => score_nvme(n, &mut acc),
    }

    // Wear: remaining life scales health for SSDs.
    if let Some(life) = s.life_remaining_pct {
        acc.health *= (life as f64 / 100.0).max(0.0);
        if life <= 3 {
            acc.issue(Severity::Critical, format!("SSD has reached the end of its rated endurance ({life}% life left) – replace it"));
        } else if life <= 10 {
            acc.issue(
                Severity::Warning,
                format!("SSD is approaching the end of its rated endurance ({life}% life left)"),
            );
        } else if life <= 30 {
            acc.issue(
                Severity::Info,
                format!("SSD wear: {life}% of rated life remaining"),
            );
        }
    }

    // Temperature affects performance (throttling) more than immediate health.
    if let Some(t) = s.temperature_c {
        let (warn, crit) = default_temp_limits(&s.info);
        if t >= crit {
            acc.perf -= 25.0;
            acc.issue(
                Severity::Warning,
                format!(
                    "Temperature {t} °C is above the critical limit ({crit} °C) – improve cooling"
                ),
            );
        } else if t >= warn {
            acc.perf -= 10.0;
            acc.issue(
                Severity::Info,
                format!("Temperature {t} °C is above the recommended limit ({warn} °C)"),
            );
        }
    }

    let health = acc.health.min(acc.cap).clamp(0.0, 100.0).round() as u8;
    let performance = acc.perf.clamp(0.0, 100.0).round() as u8;
    let status = if acc.failed {
        HealthStatus::Failed
    } else {
        HealthStatus::from_score(health)
    };
    acc.issues.sort_by_key(|i| std::cmp::Reverse(i.severity));
    if acc.issues.is_empty() {
        acc.issues.push(Issue {
            severity: Severity::Info,
            message: "No problems detected".into(),
        });
    }
    HealthReport {
        health: Some(health),
        performance: Some(performance),
        status,
        issues: acc.issues,
    }
}

fn score_ata(a: &AtaSmart, s: &DriveSnapshot, acc: &mut Acc) {
    let ssd = s.info.media.is_solid_state();
    if a.overall_passed == Some(false) {
        acc.failed = true;
        acc.cap = acc.cap.min(5.0);
        acc.issue(
            Severity::Critical,
            "SMART overall health self-assessment FAILED – back up your data and replace the drive",
        );
    }
    for attr in &a.attributes {
        match attr.status {
            AttrStatus::FailingNow if attr.prefailure => {
                acc.failed = true;
                acc.cap = acc.cap.min(10.0);
                acc.issue(Severity::Critical, format!("Pre-failure attribute {} ({}) is below its threshold – failure is imminent", attr.id, attr.name));
            }
            AttrStatus::FailingNow => {
                acc.health -= 15.0;
                acc.issue(
                    Severity::Warning,
                    format!(
                        "Old-age attribute {} ({}) is below its threshold",
                        attr.id, attr.name
                    ),
                );
            }
            AttrStatus::FailedInPast => {
                acc.health -= 5.0;
                acc.issue(
                    Severity::Info,
                    format!(
                        "Attribute {} ({}) dropped below its threshold in the past",
                        attr.id, attr.name
                    ),
                );
            }
            _ => {}
        }
    }
    let c = |id: u8| {
        a.attr(id)
            .map(|x| counter_value(id, x.raw_value))
            .unwrap_or(0)
    };

    let realloc = c(5);
    if realloc > 0 {
        acc.health -= 3.0 + decay(realloc, 67.0, 60.0);
        let sev = if realloc >= 100 {
            Severity::Critical
        } else {
            Severity::Warning
        };
        acc.issue(
            sev,
            format!(
                "{realloc} reallocated {} – the media has developed bad areas",
                if ssd { "blocks" } else { "sectors" }
            ),
        );
    }
    let events = c(196);
    if events > 0 {
        acc.health -= decay(events, 15.0, 40.0);
        if realloc == 0 {
            acc.issue(
                Severity::Warning,
                format!("{events} reallocation events recorded"),
            );
        }
    }
    let pending = c(197);
    if pending > 0 {
        acc.health -= 5.0 + decay(pending, 55.0, 10.0);
        let sev = if pending >= 10 {
            Severity::Critical
        } else {
            Severity::Warning
        };
        acc.issue(
            sev,
            format!(
                "{pending} pending (unstable) sectors – data in these sectors may be unreadable"
            ),
        );
    }
    let offline = c(198);
    if offline > 0 {
        acc.health -= 5.0 + decay(offline, 55.0, 10.0);
        let sev = if offline >= 10 {
            Severity::Critical
        } else {
            Severity::Warning
        };
        acc.issue(sev, format!("{offline} offline uncorrectable sectors"));
    }
    let reported = c(187);
    if reported > 0 {
        acc.health -= 2.0 + decay(reported, 40.0, 20.0);
        acc.issue(
            Severity::Warning,
            format!("{reported} uncorrectable errors reported to the host"),
        );
    }
    let spin = c(10);
    if spin > 0 && !ssd {
        acc.health -= 5.0 + decay(spin, 25.0, 5.0);
        acc.perf -= 10.0;
        acc.issue(
            Severity::Warning,
            format!("{spin} spin-up retries – possible motor or power supply problem"),
        );
    }
    let e2e = c(184);
    if e2e > 0 {
        acc.health -= 10.0 + decay(e2e, 40.0, 5.0);
        acc.issue(
            Severity::Critical,
            format!("{e2e} end-to-end data path errors – controller or cache problem"),
        );
    }
    if ssd {
        let fails = c(171) + c(172) + c(181) + c(182);
        if fails > 0 {
            acc.health -= decay(fails, 30.0, 10.0);
            acc.issue(
                Severity::Warning,
                format!("{fails} flash program/erase failures"),
            );
        }
    }
    let crc = c(199);
    if crc > 0 {
        acc.perf -= (5.0 + decay(crc, 25.0, 50.0)).min(30.0);
        acc.issue(
            Severity::Info,
            format!("{crc} interface CRC errors – check or replace the SATA cable / connector"),
        );
    }
    let timeouts = c(188);
    if timeouts > 0 {
        acc.perf -= decay(timeouts, 15.0, 20.0);
        acc.issue(Severity::Info, format!("{timeouts} command timeouts"));
    }
    for id in [2u8, 8] {
        if let Some(attr) = a.attr(id) {
            if attr.status == AttrStatus::FailingNow {
                acc.perf -= 40.0;
            }
        }
    }
    if let Some(f) = &s.info.ata {
        if let (Some(cur), Some(max)) = (f.sata_cur_gen, f.sata_max_gen) {
            if cur < max {
                acc.perf -= 15.0;
                acc.issue(
                    Severity::Info,
                    format!(
                        "SATA link runs at {} although the drive supports {}",
                        sata_gen_speed(cur),
                        sata_gen_speed(max)
                    ),
                );
            }
        }
    }
    if let Some(last) = a.self_test_log.first() {
        if last.passed == Some(false) {
            acc.cap = acc.cap.min(20.0);
            acc.issue(
                Severity::Critical,
                format!("Most recent self-test failed: {}", last.status),
            );
        }
    }
}

fn score_nvme(n: &NvmeHealth, acc: &mut Acc) {
    let cw = n.critical_warning;
    if cw & 0x01 != 0 {
        acc.cap = acc.cap.min(30.0);
        acc.issue(
            Severity::Critical,
            "Available spare capacity is below the threshold",
        );
    }
    if cw & 0x02 != 0 {
        acc.perf -= 20.0;
        acc.issue(
            Severity::Warning,
            "Controller reports a temperature warning",
        );
    }
    if cw & 0x04 != 0 {
        acc.cap = acc.cap.min(20.0);
        acc.failed = true;
        acc.issue(
            Severity::Critical,
            "NVM subsystem reliability is degraded (media or internal errors)",
        );
    }
    if cw & 0x08 != 0 {
        acc.cap = acc.cap.min(5.0);
        acc.failed = true;
        acc.issue(Severity::Critical, "Drive has switched to read-only mode");
    }
    if cw & 0x10 != 0 {
        acc.cap = acc.cap.min(40.0);
        acc.issue(
            Severity::Warning,
            "Volatile memory backup device has failed",
        );
    }
    if cw & 0x20 != 0 {
        acc.cap = acc.cap.min(10.0);
        acc.issue(
            Severity::Critical,
            "Persistent memory region has become read-only",
        );
    }
    if n.media_errors > 0 {
        acc.health -= 5.0 + decay(n.media_errors, 55.0, 20.0);
        let sev = if n.media_errors >= 10 {
            Severity::Critical
        } else {
            Severity::Warning
        };
        acc.issue(
            sev,
            format!("{} media and data integrity errors", n.media_errors),
        );
    }
    if n.available_spare < 100 {
        acc.health -= (100 - n.available_spare) as f64 * 0.3;
        if n.available_spare < 50 {
            acc.issue(
                Severity::Warning,
                format!(
                    "Available spare is {}% (threshold {}%)",
                    n.available_spare, n.spare_threshold
                ),
            );
        }
    }
    if n.percentage_used > 100 {
        acc.issue(
            Severity::Warning,
            format!("Rated endurance exceeded ({}% used)", n.percentage_used),
        );
    }
    if n.critical_temp_minutes > 0 {
        acc.perf -= (5.0 + (n.critical_temp_minutes as f64).ln() * 3.0).min(25.0);
        acc.issue(
            Severity::Warning,
            format!(
                "Spent {} minutes above the critical temperature",
                n.critical_temp_minutes
            ),
        );
    } else if n.warning_temp_minutes > 0 {
        acc.perf -= (2.0 + (n.warning_temp_minutes as f64).ln() * 2.0).min(15.0);
        acc.issue(
            Severity::Info,
            format!(
                "Spent {} minutes above the warning temperature",
                n.warning_temp_minutes
            ),
        );
    }
    if n.thermal_throttle_events > 0 {
        acc.perf -= decay(n.thermal_throttle_events as u64, 15.0, 50.0);
        acc.issue(
            Severity::Info,
            format!("{} thermal throttling events", n.thermal_throttle_events),
        );
    }
    if let Some(last) = n.self_test_log.first() {
        if last.passed == Some(false) {
            acc.cap = acc.cap.min(20.0);
            acc.issue(
                Severity::Critical,
                format!("Most recent self-test failed: {}", last.status),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ata::make_attribute;
    use crate::util::raw48_bytes;
    use chrono::Utc;

    fn ata_snapshot(attrs: Vec<AtaAttribute>, media: MediaType) -> DriveSnapshot {
        let info = DriveInfo {
            model: "TEST".into(),
            media,
            capacity_bytes: 1_000_000_000_000,
            ..Default::default()
        };
        let smart = AtaSmart {
            attributes: attrs,
            overall_passed: Some(true),
            ..Default::default()
        };
        let mut s = DriveSnapshot::new(Utc::now(), info, SmartData::Ata(smart));
        derive(&mut s);
        s
    }

    fn attr(id: u8, value: u8, thr: u8, raw: u64) -> AtaAttribute {
        make_attribute(
            id,
            0x33,
            value,
            value,
            Some(thr),
            raw48_bytes(raw),
            false,
            None,
        )
    }

    #[test]
    fn healthy_drive_scores_100() {
        let s = ata_snapshot(
            vec![attr(5, 100, 36, 0), attr(194, 100, 0, 35)],
            MediaType::Hdd,
        );
        assert_eq!(s.health.health, Some(100));
        assert_eq!(s.health.status, HealthStatus::Excellent);
        assert_eq!(s.temperature_c, Some(35));
    }

    #[test]
    fn pending_sectors_hurt_a_lot() {
        let s = ata_snapshot(
            vec![attr(197, 100, 0, 24), attr(5, 100, 36, 200)],
            MediaType::Hdd,
        );
        let h = s.health.health.unwrap();
        assert!(h < 30, "health {h}");
        assert!(s
            .health
            .issues
            .iter()
            .any(|i| i.severity == Severity::Critical));
    }

    #[test]
    fn failing_prefail_attribute_marks_failed() {
        let s = ata_snapshot(vec![attr(5, 30, 36, 3000)], MediaType::Hdd);
        assert_eq!(s.health.status, HealthStatus::Failed);
        assert!(s.health.health.unwrap() <= 10);
    }

    #[test]
    fn nvme_wear_scales_health() {
        let info = DriveInfo {
            media: MediaType::Nvme,
            ..Default::default()
        };
        let n = NvmeHealth {
            available_spare: 100,
            spare_threshold: 10,
            percentage_used: 20,
            ..Default::default()
        };
        let mut s = DriveSnapshot::new(Utc::now(), info, SmartData::Nvme(n));
        derive(&mut s);
        assert_eq!(s.life_remaining_pct, Some(80));
        assert_eq!(s.health.health, Some(80));
    }
}
