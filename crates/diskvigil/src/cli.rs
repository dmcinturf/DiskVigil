//! Command-line interface.

use crate::{Command, FormatArg, TestArg};
use dv_core::config::{Paths, Settings};
use dv_core::report::{self, ReportFormat};
use dv_core::service::Service;
use dv_core::util::{format_bytes, format_hours, format_temp};
use dv_core::*;
use std::time::Duration;

pub fn run(cmd: Command, settings: Settings, paths: Paths) -> i32 {
    let fahrenheit = settings.fahrenheit;
    let mut svc = Service::new(settings, paths.clone());
    match cmd {
        Command::List { json } => {
            let (drives, _) = svc.scan();
            print_errors(&svc);
            if json {
                let v: Vec<_> = drives
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "index": d.info.index, "path": d.info.path, "key": d.info.key(),
                            "model": d.info.model, "serial": d.info.serial, "type": d.info.media.short(),
                            "bus": d.info.bus.label(), "capacity_bytes": d.info.capacity_bytes,
                            "health": d.health.health, "performance": d.health.performance,
                            "status": d.health.status.label(), "temperature_c": d.temperature_c,
                            "volumes": d.info.volumes,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                print_list(&drives, fahrenheit);
            }
            if drives.is_empty() {
                3
            } else {
                0
            }
        }
        Command::Info { drive, json } => {
            let (drives, _) = svc.scan();
            print_errors(&svc);
            let selected: Vec<&DriveSnapshot> = match &drive {
                Some(sel) => match find(&drives, sel) {
                    Some(d) => vec![d],
                    None => {
                        eprintln!("No drive matches '{sel}'. Use `diskvigil list` to see drives.");
                        return 2;
                    }
                },
                None => drives.iter().collect(),
            };
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&selected).unwrap_or_default()
                );
            } else {
                for d in selected {
                    print_info(d, fahrenheit);
                }
            }
            0
        }
        Command::Check { quiet } => {
            let (drives, _) = svc.scan();
            if drives.is_empty() {
                if !quiet {
                    println!("No drives found.");
                    print_errors(&svc);
                }
                return 3;
            }
            let mut code: i32 = 0;
            for d in &drives {
                let c: usize = match d.health.status {
                    HealthStatus::Critical | HealthStatus::Failed => 2,
                    HealthStatus::Fair | HealthStatus::Poor => 1,
                    _ => 0,
                };
                code = code.max(c as i32);
                if !quiet {
                    let tag = ["OK", "WARN", "CRIT"][c];
                    let top = d
                        .health
                        .issues
                        .first()
                        .map(|i| i.message.as_str())
                        .unwrap_or("");
                    println!(
                        "[{tag:4}] {:<36} health {:>4}  {}",
                        trunc(&d.info.display_name(), 36),
                        pct(d.health.health),
                        top
                    );
                }
            }
            code
        }
        Command::Report { format, output } => {
            let (drives, _) = svc.scan();
            print_errors(&svc);
            let fmt = match format {
                FormatArg::Html => ReportFormat::Html,
                FormatArg::Json => ReportFormat::Json,
                FormatArg::Csv => ReportFormat::Csv,
            };
            let path = output.unwrap_or_else(|| {
                let _ = std::fs::create_dir_all(paths.reports_dir());
                paths.reports_dir().join(format!(
                    "diskvigil-{}.{}",
                    chrono::Local::now().format("%Y%m%d-%H%M%S"),
                    fmt.extension()
                ))
            });
            match std::fs::write(&path, report::render(fmt, &drives)) {
                Ok(()) => {
                    println!("Report written to {}", path.display());
                    0
                }
                Err(e) => {
                    eprintln!("Could not write {}: {e}", path.display());
                    1
                }
            }
        }
        Command::Monitor { interval, once } => {
            let secs = interval.unwrap_or(svc.settings.refresh_secs).max(5);
            println!(
                "DiskVigil monitor – data in {} – interval {secs}s (Ctrl+C to stop)",
                paths.data_dir.display()
            );
            loop {
                let (drives, events) = svc.scan();
                crate::notify::dispatch(&events, &svc.settings.alerts);
                let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                let summary: Vec<String> = drives
                    .iter()
                    .map(|d| {
                        format!(
                            "{} {} {}",
                            trunc(&d.info.display_name(), 20),
                            pct(d.health.health),
                            d.temperature_c
                                .map(|t| format_temp(t, fahrenheit))
                                .unwrap_or_default()
                        )
                    })
                    .collect();
                println!("[{now}] {}", summary.join(" | "));
                for e in &events {
                    println!(
                        "  ALERT [{}] {}: {}",
                        e.severity.label(),
                        e.drive_name,
                        e.message
                    );
                }
                print_errors(&svc);
                if once {
                    return 0;
                }
                std::thread::sleep(Duration::from_secs(secs));
            }
        }
        Command::Selftest { drive, kind } => {
            let (drives, _) = svc.scan();
            let Some(d) = find(&drives, &drive) else {
                eprintln!("No drive matches '{drive}'.");
                return 2;
            };
            let k = match kind {
                TestArg::Short => SelfTestKind::Short,
                TestArg::Extended => SelfTestKind::Extended,
                TestArg::Conveyance => SelfTestKind::Conveyance,
                TestArg::Abort => SelfTestKind::Abort,
            };
            let info = d.info.clone();
            match svc.self_test(&info, k) {
                Ok(()) => {
                    if k == SelfTestKind::Abort {
                        println!("Self-test aborted on {}.", info.display_name());
                    } else {
                        println!(
                            "{} self-test started on {}. Check progress with `diskvigil info \"{}\"`.",
                            k.label(),
                            info.display_name(),
                            info.serial
                        );
                    }
                    0
                }
                Err(e) => {
                    eprintln!("Self-test could not be started: {e:#}");
                    1
                }
            }
        }
    }
}

fn print_errors(svc: &Service) {
    for e in svc.last_errors() {
        eprintln!("warning: {e}");
    }
    if !dv_core::backend::is_elevated()
        && svc.settings.backend != dv_core::config::BackendMode::Demo
    {
        eprintln!(
            "note: not running {} – SMART data may be unavailable.",
            if cfg!(windows) {
                "as Administrator"
            } else {
                "as root"
            }
        );
    }
}

fn find<'a>(drives: &'a [DriveSnapshot], sel: &str) -> Option<&'a DriveSnapshot> {
    let s = sel.to_lowercase();
    if let Ok(i) = sel.parse::<u32>() {
        if let Some(d) = drives.iter().find(|d| d.info.index == Some(i)) {
            return Some(d);
        }
        if let Some(d) = drives.get(i as usize) {
            return Some(d);
        }
    }
    drives
        .iter()
        .find(|d| {
            d.info.path.eq_ignore_ascii_case(sel)
                || d.info.serial.eq_ignore_ascii_case(sel)
                || d.info.key() == sel
        })
        .or_else(|| {
            drives.iter().find(|d| {
                d.info.model.to_lowercase().contains(&s)
                    || d.info.serial.to_lowercase().contains(&s)
            })
        })
}

fn pct(v: Option<u8>) -> String {
    v.map(|x| format!("{x}%")).unwrap_or_else(|| "–".into())
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}

#[allow(clippy::print_literal)]
fn print_list(drives: &[DriveSnapshot], f: bool) {
    if drives.is_empty() {
        println!("No drives found.");
        return;
    }
    println!(
        "{:<3} {:<34} {:<5} {:>9} {:>7} {:>6} {:>7}  {:<10} {}",
        "#", "Model", "Type", "Capacity", "Health", "Perf", "Temp", "Status", "Volumes"
    );
    for (i, d) in drives.iter().enumerate() {
        println!(
            "{:<3} {:<34} {:<5} {:>9} {:>7} {:>6} {:>7}  {:<10} {}",
            d.info.index.unwrap_or(i as u32),
            trunc(&d.info.display_name(), 34),
            d.info.media.short(),
            format_bytes(d.info.capacity_bytes),
            pct(d.health.health),
            pct(d.health.performance),
            d.temperature_c
                .map(|t| format_temp(t, f))
                .unwrap_or_else(|| "–".into()),
            d.health.status.label(),
            d.info.volumes.join(" ")
        );
    }
}

#[allow(clippy::print_literal)]
fn print_info(d: &DriveSnapshot, f: bool) {
    let line = "─".repeat(78);
    println!("{line}");
    println!(
        "{}  [{}]  Health {}  Performance {}",
        d.info.display_name(),
        d.health.status.label(),
        pct(d.health.health),
        pct(d.health.performance)
    );
    println!("{line}");
    for (k, v) in report::summary_rows(d) {
        if k == "Temperature" {
            if let Some(t) = d.temperature_c {
                println!("  {k:<16} {}", format_temp(t, f));
                continue;
            }
        }
        println!("  {k:<16} {v}");
    }
    if let Some(p) = &d.self_test_progress {
        println!("  {:<16} {}", "Self-test", p.text);
    }
    println!("\n  Assessment:");
    for i in &d.health.issues {
        println!("    [{:<8}] {}", i.severity.label(), i.message);
    }
    match &d.smart {
        SmartData::Ata(a) => {
            println!(
                "\n  {:>3} {:<34} {:>4} {:>5} {:>4} {:>18}  {:<8} {}",
                "ID", "Attribute", "Cur", "Worst", "Thr", "Raw", "Type", "Status"
            );
            for at in &a.attributes {
                println!(
                    "  {:>3} {:<34} {:>4} {:>5} {:>4} {:>18}  {:<8} {}",
                    at.id,
                    trunc(&at.name, 34),
                    at.value,
                    at.worst,
                    at.threshold
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| "–".into()),
                    trunc(&at.raw_display, 18),
                    if at.prefailure { "Pre-fail" } else { "Old-age" },
                    at.status.label()
                );
            }
        }
        SmartData::Nvme(_) => {
            println!();
            for (k, v) in report::nvme_rows(d) {
                println!("  {k:<32} {v}");
            }
        }
        SmartData::Unavailable { reason } => println!("\n  SMART unavailable: {reason}"),
    }
    let log = d.self_test_log();
    if !log.is_empty() {
        println!("\n  Self-test log (newest first):");
        for e in log.iter().take(10) {
            println!(
                "    {:<22} {:<34} {:>8}  {}",
                trunc(&e.kind, 22),
                trunc(&e.status, 34),
                format_hours(e.lifetime_hours)
                    .split(' ')
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" "),
                e.failing_lba
                    .map(|l| format!("LBA {l}"))
                    .unwrap_or_default()
            );
        }
    }
    for w in &d.warnings {
        println!("  note: {w}");
    }
    println!();
}
