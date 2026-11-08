//! HTML / JSON / CSV report export.

use crate::model::*;
use crate::util::{format_bytes, format_hours};
use chrono::Local;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Html,
    Json,
    Csv,
}

impl ReportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ReportFormat::Html => "html",
            ReportFormat::Json => "json",
            ReportFormat::Csv => "csv",
        }
    }
}

pub fn render(format: ReportFormat, drives: &[DriveSnapshot]) -> String {
    match format {
        ReportFormat::Html => html(drives),
        ReportFormat::Json => serde_json::to_string_pretty(drives).unwrap_or_default(),
        ReportFormat::Csv => csv(drives),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn status_class(st: HealthStatus) -> &'static str {
    match st {
        HealthStatus::Excellent | HealthStatus::Good => "ok",
        HealthStatus::Fair | HealthStatus::Poor => "warn",
        HealthStatus::Critical | HealthStatus::Failed => "bad",
        HealthStatus::Unknown => "unk",
    }
}

pub fn summary_rows(s: &DriveSnapshot) -> Vec<(&'static str, String)> {
    let i = &s.info;
    let mut v = vec![
        ("Model", i.display_name()),
        ("Serial", i.serial.clone()),
        ("Firmware", i.firmware.clone()),
        ("Capacity", format_bytes(i.capacity_bytes)),
        ("Type", i.media.label().to_string()),
        ("Interface", i.bus.label().to_string()),
        ("Device", i.path.clone()),
    ];
    if let Some(r) = i.rotation_rpm.filter(|&r| r > 0) {
        v.push(("Rotation", format!("{r} RPM")));
    }
    if !i.volumes.is_empty() {
        v.push(("Volumes", i.volumes.join(", ")));
    }
    if let Some(t) = s.temperature_c {
        v.push(("Temperature", format!("{t} °C")));
    }
    if let Some(h) = s.power_on_hours {
        v.push(("Power-on time", format_hours(h)));
    }
    if let Some(c) = s.power_cycles {
        v.push(("Power cycles", c.to_string()));
    }
    if let Some(w) = s.host_written_bytes {
        v.push(("Host writes", format_bytes(w)));
    }
    if let Some(r) = s.host_read_bytes {
        v.push(("Host reads", format_bytes(r)));
    }
    if let Some(l) = s.life_remaining_pct {
        v.push(("Remaining life", format!("{l}%")));
    }
    v.push((
        "Data source",
        format!("{} {}", i.backend, i.transport.clone().unwrap_or_default()),
    ));
    v
}

fn html(drives: &[DriveSnapshot]) -> String {
    let mut h = String::new();
    let _ = write!(
        h,
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>DiskVigil report</title><style>
:root{{--bg:#f6f7f9;--card:#fff;--fg:#1c1f24;--muted:#5f6670;--line:#e3e6ea;--ok:#1f9d55;--warn:#d98a00;--bad:#d13438;--unk:#7a828c}}
@media (prefers-color-scheme:dark){{:root{{--bg:#16181c;--card:#1f2228;--fg:#e8eaed;--muted:#9aa1ab;--line:#2e333b}}}}
body{{background:var(--bg);color:var(--fg);font:14px/1.45 system-ui,Segoe UI,Roboto,sans-serif;margin:0;padding:24px 16px}}
main{{max-width:1100px;margin:auto}} h1{{margin:0 0 4px}} .muted{{color:var(--muted)}}
.card{{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:18px;margin:18px 0}}
.head{{display:flex;flex-wrap:wrap;gap:16px;align-items:center;justify-content:space-between}}
.badge{{display:inline-block;padding:3px 10px;border-radius:99px;color:#fff;font-weight:600}}
.ok{{background:var(--ok)}} .warn{{background:var(--warn)}} .bad{{background:var(--bad)}} .unk{{background:var(--unk)}}
.scores{{display:flex;gap:18px}} .score b{{font-size:22px;display:block}}
table{{border-collapse:collapse;width:100%;margin-top:10px}} td,th{{text-align:left;padding:5px 8px;border-bottom:1px solid var(--line)}}
th{{font-weight:600;color:var(--muted)}} .scroll{{overflow-x:auto}} tr.Warning td{{color:var(--warn)}} tr.FailingNow td,tr.FailedInPast td{{color:var(--bad)}}
li.Critical{{color:var(--bad)}} li.Warning{{color:var(--warn)}}
</style></head><body><main><h1>DiskVigil report</h1><div class="muted">Generated {} · {} drive(s)</div>"#,
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        drives.len()
    );
    for s in drives {
        let hr = &s.health;
        let _ = write!(
            h,
            r#"<section class="card"><div class="head"><div><h2 style="margin:0">{}</h2><div class="muted">{} · {}</div></div>
<div class="scores"><div class="score">Health<b>{}</b></div><div class="score">Performance<b>{}</b></div><div><span class="badge {}">{}</span></div></div></div>"#,
            esc(&s.info.display_name()),
            esc(s.info.media.label()),
            esc(&format_bytes(s.info.capacity_bytes)),
            hr.health.map(|x| format!("{x}%")).unwrap_or("–".into()),
            hr.performance
                .map(|x| format!("{x}%"))
                .unwrap_or("–".into()),
            status_class(hr.status),
            hr.status.label()
        );
        h.push_str("<ul>");
        for i in &hr.issues {
            let _ = write!(
                h,
                r#"<li class="{:?}">{}</li>"#,
                i.severity,
                esc(&i.message)
            );
        }
        h.push_str("</ul><div class=\"scroll\"><table>");
        for (k, v) in summary_rows(s) {
            let _ = write!(h, "<tr><th>{}</th><td>{}</td></tr>", k, esc(&v));
        }
        h.push_str("</table></div>");
        match &s.smart {
            SmartData::Ata(a) => {
                h.push_str("<h3>SMART attributes</h3><div class=\"scroll\"><table><tr><th>ID</th><th>Attribute</th><th>Current</th><th>Worst</th><th>Threshold</th><th>Raw</th><th>Type</th><th>Status</th></tr>");
                for at in &a.attributes {
                    let _ = write!(
                        h,
                        r#"<tr class="{:?}"><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>"#,
                        at.status,
                        at.id,
                        esc(&at.name),
                        at.value,
                        at.worst,
                        at.threshold.map(|t| t.to_string()).unwrap_or("–".into()),
                        esc(&at.raw_display),
                        if at.prefailure { "Pre-fail" } else { "Old-age" },
                        at.status.label()
                    );
                }
                h.push_str("</table></div>");
            }
            SmartData::Nvme(_) => {
                h.push_str("<h3>NVMe health log</h3><div class=\"scroll\"><table>");
                for (k, v) in nvme_rows(s) {
                    let _ = write!(h, "<tr><th>{}</th><td>{}</td></tr>", esc(&k), esc(&v));
                }
                h.push_str("</table></div>");
            }
            SmartData::Unavailable { reason } => {
                let _ = write!(
                    h,
                    "<p class=\"muted\">SMART data unavailable: {}</p>",
                    esc(reason)
                );
            }
        }
        let log = s.self_test_log();
        if !log.is_empty() {
            h.push_str("<h3>Self-test log</h3><div class=\"scroll\"><table><tr><th>Type</th><th>Result</th><th>Lifetime hours</th><th>First error LBA</th></tr>");
            for e in log {
                let _ = write!(
                    h,
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    esc(&e.kind),
                    esc(&e.status),
                    e.lifetime_hours,
                    e.failing_lba.map(|l| l.to_string()).unwrap_or("–".into())
                );
            }
            h.push_str("</table></div>");
        }
        h.push_str("</section>");
    }
    h.push_str("</main></body></html>");
    h
}

pub fn nvme_rows(s: &DriveSnapshot) -> Vec<(String, String)> {
    let SmartData::Nvme(n) = &s.smart else {
        return Vec::new();
    };
    let warn = nvme_critical_warning_bits(n.critical_warning);
    let mut v = vec![
        (
            "Critical warning".to_string(),
            if warn.is_empty() {
                format!("0x{:02X} (none)", n.critical_warning)
            } else {
                format!("0x{:02X} – {}", n.critical_warning, warn.join(", "))
            },
        ),
        (
            "Composite temperature".into(),
            n.temperature_c
                .map(|t| format!("{t} °C"))
                .unwrap_or("–".into()),
        ),
        ("Available spare".into(), format!("{}%", n.available_spare)),
        (
            "Available spare threshold".into(),
            format!("{}%", n.spare_threshold),
        ),
        ("Percentage used".into(), format!("{}%", n.percentage_used)),
        (
            "Data units read".into(),
            format!(
                "{} ({})",
                n.data_units_read,
                format_bytes(n.data_units_read.saturating_mul(512_000))
            ),
        ),
        (
            "Data units written".into(),
            format!(
                "{} ({})",
                n.data_units_written,
                format_bytes(n.data_units_written.saturating_mul(512_000))
            ),
        ),
        ("Host read commands".into(), n.host_reads.to_string()),
        ("Host write commands".into(), n.host_writes.to_string()),
        (
            "Controller busy time".into(),
            format!("{} min", n.controller_busy_minutes),
        ),
        ("Power cycles".into(), n.power_cycles.to_string()),
        ("Power-on hours".into(), n.power_on_hours.to_string()),
        ("Unsafe shutdowns".into(), n.unsafe_shutdowns.to_string()),
        (
            "Media & data integrity errors".into(),
            n.media_errors.to_string(),
        ),
        ("Error log entries".into(), n.error_log_entries.to_string()),
        (
            "Warning temperature time".into(),
            format!("{} min", n.warning_temp_minutes),
        ),
        (
            "Critical temperature time".into(),
            format!("{} min", n.critical_temp_minutes),
        ),
        (
            "Thermal throttling events".into(),
            n.thermal_throttle_events.to_string(),
        ),
    ];
    for (i, t) in n.sensor_temps_c.iter().enumerate() {
        v.push((format!("Temperature sensor {}", i + 1), format!("{t} °C")));
    }
    v
}

fn csv(drives: &[DriveSnapshot]) -> String {
    let mut out = String::from("timestamp,device,model,serial,health,performance,status,temperature_c,item_id,item,current,worst,threshold,raw,item_status\n");
    for s in drives {
        let base = [
            s.timestamp.to_rfc3339(),
            s.info.path.clone(),
            s.info.display_name(),
            s.info.serial.clone(),
            s.health.health.map(|x| x.to_string()).unwrap_or_default(),
            s.health
                .performance
                .map(|x| x.to_string())
                .unwrap_or_default(),
            s.health.status.label().to_string(),
            s.temperature_c.map(|x| x.to_string()).unwrap_or_default(),
        ]
        .iter()
        .map(|x| csv_field(x))
        .collect::<Vec<_>>()
        .join(",");
        match &s.smart {
            SmartData::Ata(a) => {
                for at in &a.attributes {
                    let _ = writeln!(
                        out,
                        "{base},{},{},{},{},{},{},{}",
                        at.id,
                        csv_field(&at.name),
                        at.value,
                        at.worst,
                        at.threshold.map(|t| t.to_string()).unwrap_or_default(),
                        at.raw_value,
                        at.status.label()
                    );
                }
            }
            SmartData::Nvme(_) => {
                for (k, v) in nvme_rows(s) {
                    let _ = writeln!(out, "{base},,{},,,,{},", csv_field(&k), csv_field(&v));
                }
            }
            SmartData::Unavailable { reason } => {
                let _ = writeln!(out, "{base},,{},,,,,", csv_field(reason));
            }
        }
    }
    out
}
