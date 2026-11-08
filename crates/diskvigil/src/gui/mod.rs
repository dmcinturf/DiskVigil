//! egui desktop UI.

mod widgets;

use crate::platform;
use crate::worker::{Cmd, Worker};
use anyhow::anyhow;
use chrono::{DateTime, Duration as CDuration, Local, Utc};
use dv_core::ata::{attributes, FeatureChange};
use dv_core::config::{BackendMode, Paths, Settings, TempLimit, Theme};
use dv_core::history::{self, tracked_name, HistoryPoint};
use dv_core::report::{self, ReportFormat};
use dv_core::util::{format_bytes, format_hours, format_temp};
use dv_core::*;
use eframe::egui::{self, vec2, Align2, Color32, FontId, Rect, RichText, Sense, Ui};
use std::time::{Duration, Instant};
use widgets::*;

pub fn run(settings: Settings, paths: Paths) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DiskVigil")
            .with_inner_size([1220.0, 800.0])
            .with_min_inner_size([880.0, 560.0])
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "DiskVigil",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, settings, paths)))),
    )
    .map_err(|e| anyhow!("{e}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Smart,
    History,
    SelfTests,
    Advanced,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Range {
    Day,
    Week,
    Month,
    Year,
    All,
}

impl Range {
    fn label(self) -> &'static str {
        match self {
            Range::Day => "24 hours",
            Range::Week => "7 days",
            Range::Month => "30 days",
            Range::Year => "1 year",
            Range::All => "All",
        }
    }
    fn since(self) -> DateTime<Utc> {
        let now = Utc::now();
        match self {
            Range::Day => now - CDuration::days(1),
            Range::Week => now - CDuration::days(7),
            Range::Month => now - CDuration::days(30),
            Range::Year => now - CDuration::days(365),
            Range::All => DateTime::<Utc>::MIN_UTC,
        }
    }
}

enum Pending {
    SelfTest(DriveInfo, SelfTestKind),
    Feature(DriveInfo, FeatureChange, String),
}

struct View {
    drives: Vec<DriveSnapshot>,
    busy: bool,
    scanned_once: bool,
    last_scan: Option<DateTime<Local>>,
    errors: Vec<String>,
    smartctl: Option<String>,
    native: bool,
    unseen: usize,
}

struct App {
    worker: Worker,
    settings: Settings,
    draft: Settings,
    paths: Paths,
    elevated: bool,
    selected: Option<String>,
    tab: Tab,
    range: Range,
    chart_attr: Option<u16>,
    show_settings: bool,
    show_alerts: bool,
    show_about: bool,
    pending: Option<Pending>,
    toast: Option<(String, Instant)>,
    apm_edit: u8,
    aam_edit: u8,
    applied_theme: Option<Theme>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, settings: Settings, paths: Paths) -> Self {
        let worker = Worker::spawn(settings.clone(), paths.clone(), cc.egui_ctx.clone());
        App {
            worker,
            draft: settings.clone(),
            settings,
            paths,
            elevated: dv_core::backend::is_elevated(),
            selected: None,
            tab: Tab::Overview,
            range: Range::Week,
            chart_attr: None,
            show_settings: false,
            show_alerts: false,
            show_about: false,
            pending: None,
            toast: None,
            apm_edit: 128,
            aam_edit: 254,
            applied_theme: None,
        }
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn temp(&self, c: i32) -> String {
        format_temp(c, self.settings.fahrenheit)
    }

    fn temp_val(&self, c: f64) -> f64 {
        if self.settings.fahrenheit {
            c * 9.0 / 5.0 + 32.0
        } else {
            c
        }
    }

    fn export(&mut self, fmt: ReportFormat, drives: &[DriveSnapshot]) {
        let dir = self.paths.reports_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!(
            "diskvigil-{}.{}",
            Local::now().format("%Y%m%d-%H%M%S"),
            fmt.extension()
        ));
        match std::fs::write(&path, report::render(fmt, drives)) {
            Ok(()) => {
                self.toast(format!("Report saved to {}", path.display()));
                platform::open_path(if fmt == ReportFormat::Html {
                    &path
                } else {
                    &dir
                });
            }
            Err(e) => self.toast(format!("Could not save report: {e}")),
        }
    }

    // ------------------------------------------------------------------ chrome

    fn toolbar(&mut self, ui: &mut Ui, v: &View) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("DiskVigil").size(18.0).strong());
            ui.add_space(12.0);
            let refresh = ui.add_enabled(!v.busy, egui::Button::new("⟳ Refresh"));
            if refresh.clicked() {
                self.worker.send(Cmd::Refresh);
            }
            if v.busy {
                ui.spinner();
            }
            ui.menu_button("Export", |ui| {
                for (label, fmt) in [
                    ("HTML report", ReportFormat::Html),
                    ("JSON data", ReportFormat::Json),
                    ("CSV attributes", ReportFormat::Csv),
                ] {
                    if ui.button(label).clicked() {
                        self.export(fmt, &v.drives);
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Open reports folder").clicked() {
                    let d = self.paths.reports_dir();
                    let _ = std::fs::create_dir_all(&d);
                    platform::open_path(&d);
                    ui.close();
                }
            });
            let alerts_label = if v.unseen > 0 {
                format!("🔔 Alerts ({})", v.unseen)
            } else {
                "🔔 Alerts".into()
            };
            let alerts_btn = egui::Button::new(if v.unseen > 0 {
                RichText::new(alerts_label).color(AMBER)
            } else {
                RichText::new(alerts_label)
            });
            if ui.add(alerts_btn).clicked() {
                self.show_alerts = true;
                if let Ok(mut g) = self.worker.shared.lock() {
                    g.unseen_alerts = 0;
                }
            }
            if ui.button("⚙ Settings").clicked() {
                self.draft = self.settings.clone();
                self.show_settings = true;
            }
            if ui.button("About").clicked() {
                self.show_about = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(t) = v.last_scan {
                    ui.label(RichText::new(format!("Updated {}", t.format("%H:%M:%S"))).weak());
                }
            });
        });
    }

    fn banner(&mut self, ui: &mut Ui) {
        let frame = egui::Frame::group(ui.style())
            .fill(Color32::from_rgba_unmultiplied(0xF2, 0xB7, 0x05, 40))
            .inner_margin(8.0);
        frame.show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(AMBER, "⚠");
                if cfg!(windows) {
                    ui.label("DiskVigil is not running as Administrator, so SMART data can't be read directly from the drives.");
                    if ui.button("Restart as Administrator").clicked() {
                        match platform::relaunch_elevated(&self.relaunch_args()) {
                            Ok(()) => std::process::exit(0),
                            Err(e) => self.toast(format!("{e:#}")),
                        }
                    }
                } else if cfg!(target_os = "linux") {
                    ui.label("Not running as root – SMART data needs root privileges (or run `sudo diskvigil`).");
                    if ui.button("Restart with pkexec").clicked() {
                        match platform::relaunch_elevated(&self.relaunch_args()) {
                            Ok(()) => std::process::exit(0),
                            Err(e) => self.toast(format!("{e:#}")),
                        }
                    }
                } else {
                    ui.label("Not running as root – run `sudo diskvigil` for SMART data.");
                }
                if ui.button("Try demo mode").clicked() {
                    self.draft = self.settings.clone();
                    self.draft.backend = BackendMode::Demo;
                    self.apply_settings();
                }
            });
        });
    }

    fn relaunch_args(&self) -> Vec<String> {
        if self.paths.portable {
            vec![]
        } else {
            vec![
                "--data-dir".into(),
                self.paths.data_dir.display().to_string(),
            ]
        }
    }

    fn status_bar(&mut self, ui: &mut Ui, v: &View) {
        ui.horizontal(|ui| {
            let mode = self.settings.backend;
            ui.label(RichText::new(format!("Backend: {}", mode.label())).weak().size(12.0));
            ui.separator();
            let sc = v.smartctl.as_deref().map(|_| "smartctl: found").unwrap_or("smartctl: not found");
            ui.label(RichText::new(sc).weak().size(12.0)).on_hover_text(v.smartctl.clone().unwrap_or_else(|| {
                "Place smartctl (smartmontools) next to DiskVigil to enable the fallback backend".into()
            }));
            ui.separator();
            ui.label(RichText::new(if self.elevated { "Elevated: yes" } else { "Elevated: no" }).weak().size(12.0));
            ui.separator();
            ui.label(
                RichText::new(format!("Data: {}{}", self.paths.data_dir.display(), if self.paths.portable { " (portable)" } else { "" }))
                    .weak()
                    .size(12.0),
            );
            if let Some((msg, at)) = &self.toast {
                if at.elapsed() < Duration::from_secs(8) {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(msg).color(BLUE).size(12.5));
                    });
                }
            }
            if !v.errors.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.colored_label(ORANGE, "⚠ backend errors").on_hover_text(v.errors.join("\n"));
                });
            }
        });
    }

    // ------------------------------------------------------------------ drive list

    fn drive_list(&mut self, ui: &mut Ui, v: &View) {
        ui.add_space(6.0);
        ui.label(RichText::new(format!("Drives ({})", v.drives.len())).strong());
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for d in &v.drives {
                    self.drive_card(ui, d);
                    ui.add_space(6.0);
                }
                if v.drives.is_empty() && v.scanned_once {
                    ui.label(RichText::new("No drives detected.").weak());
                }
            });
    }

    fn drive_card(&mut self, ui: &mut Ui, d: &DriveSnapshot) {
        let key = d.info.key();
        let selected = self.selected.as_deref() == Some(key.as_str());
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 70.0), Sense::click());
        let vis = ui.visuals().clone();
        let p = ui.painter();
        let bg = if selected {
            vis.selection.bg_fill.gamma_multiply(0.45)
        } else if resp.hovered() {
            vis.widgets.hovered.weak_bg_fill
        } else {
            vis.faint_bg_color
        };
        p.rect_filled(rect, 6.0, bg);
        let hc = score_color(d.health.health, d.health.status);
        p.rect_filled(
            Rect::from_min_size(rect.min, vec2(5.0, rect.height())),
            3.0,
            hc,
        );

        let right_w = 70.0;
        let text_clip = Rect::from_min_max(rect.min, pos(rect.right() - right_w, rect.bottom()));
        let tp = p.with_clip_rect(text_clip);
        let chip_txt = d.info.media.short();
        let chip = tp.layout_no_wrap(chip_txt.into(), FontId::proportional(10.5), Color32::WHITE);
        let chip_rect =
            Rect::from_min_size(rect.min + vec2(14.0, 9.0), chip.size() + vec2(10.0, 3.0));
        let chip_col = match d.info.media {
            MediaType::Hdd => PURPLE,
            MediaType::Ssd => TEAL,
            MediaType::Nvme => BLUE,
            MediaType::Unknown => GRAY,
        };
        tp.rect_filled(chip_rect, 6.0, chip_col);
        tp.galley(chip_rect.min + vec2(5.0, 1.5), chip, Color32::WHITE);
        tp.text(
            pos(chip_rect.right() + 6.0, chip_rect.center().y),
            Align2::LEFT_CENTER,
            d.info.display_name(),
            FontId::proportional(14.0),
            vis.strong_text_color(),
        );
        let mut sub = format_bytes(d.info.capacity_bytes);
        if !d.info.volumes.is_empty() {
            sub.push_str(&format!("  ·  {}", d.info.volumes.join(" ")));
        }
        tp.text(
            rect.min + vec2(14.0, 30.0),
            Align2::LEFT_TOP,
            sub,
            FontId::proportional(12.0),
            vis.weak_text_color(),
        );
        if let Some(t) = d.temperature_c {
            let lim = self.settings.temp_limits(&d.info);
            tp.text(
                rect.min + vec2(14.0, 48.0),
                Align2::LEFT_TOP,
                format!("🌡 {}", self.temp(t)),
                FontId::proportional(12.0),
                temp_color(t, lim),
            );
        } else {
            tp.text(
                rect.min + vec2(14.0, 48.0),
                Align2::LEFT_TOP,
                d.info.bus.label(),
                FontId::proportional(12.0),
                vis.weak_text_color(),
            );
        }
        let hx = rect.right() - 10.0;
        p.text(
            pos(hx, rect.top() + 12.0),
            Align2::RIGHT_TOP,
            d.health
                .health
                .map(|h| format!("{h}%"))
                .unwrap_or_else(|| "–".into()),
            FontId::proportional(20.0),
            hc,
        );
        p.text(
            pos(hx, rect.top() + 40.0),
            Align2::RIGHT_TOP,
            d.health.status.label(),
            FontId::proportional(11.5),
            hc,
        );
        if resp.clicked() {
            self.selected = Some(key);
            self.chart_attr = None;
        }
        resp.on_hover_text(format!(
            "{}\n{}",
            d.info.path,
            d.health
                .issues
                .first()
                .map(|i| i.message.as_str())
                .unwrap_or("")
        ));
    }

    // ------------------------------------------------------------------ central

    fn central(&mut self, ui: &mut Ui, v: &View) {
        let Some(d) = v
            .drives
            .iter()
            .find(|d| Some(d.info.key()) == self.selected)
            .cloned()
        else {
            ui.centered_and_justified(|ui| {
                if !v.scanned_once {
                    ui.label(RichText::new("Scanning drives…").size(16.0));
                } else if v.drives.is_empty() {
                    let mut t = String::from("No drives were found.");
                    if !v.errors.is_empty() {
                        t.push_str("\n\n");
                        t.push_str(&v.errors.join("\n"));
                    }
                    ui.label(RichText::new(t).size(15.0));
                }
            });
            return;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(d.info.display_name()).size(22.0).strong());
            ui.add_space(8.0);
            let hc = score_color(d.health.health, d.health.status);
            chip(ui, d.health.status.label(), hc);
            chip(ui, d.info.media.short(), GRAY);
            chip(ui, d.info.bus.label(), GRAY);
        });
        ui.label(
            RichText::new(format!(
                "{} · S/N {} · FW {} · {}",
                format_bytes(d.info.capacity_bytes),
                if d.info.serial.is_empty() {
                    "?"
                } else {
                    &d.info.serial
                },
                if d.info.firmware.is_empty() {
                    "?"
                } else {
                    &d.info.firmware
                },
                d.info.path
            ))
            .weak(),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            for (t, label) in [
                (Tab::Overview, "Overview"),
                (Tab::Smart, "SMART"),
                (Tab::History, "History"),
                (Tab::SelfTests, "Self-tests"),
                (Tab::Advanced, "Advanced"),
            ] {
                ui.selectable_value(&mut self.tab, t, RichText::new(label).size(14.5));
            }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.tab {
                Tab::Overview => self.overview(ui, &d),
                Tab::Smart => self.smart_tab(ui, &d),
                Tab::History => self.history_tab(ui, &d),
                Tab::SelfTests => self.selftest_tab(ui, &d),
                Tab::Advanced => self.advanced_tab(ui, &d),
            });
    }

    fn card<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
        egui::Frame::group(ui.style())
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_width(width);
                ui.vertical_centered(add).inner
            })
            .inner
    }

    fn overview(&mut self, ui: &mut Ui, d: &DriveSnapshot) {
        let hc = score_color(d.health.health, d.health.status);
        let pc = score_color(d.health.performance, HealthStatus::Unknown);
        ui.horizontal_wrapped(|ui| {
            Self::card(ui, 160.0, |ui| {
                gauge(ui, d.health.health, "Health", hc, 140.0)
            });
            Self::card(ui, 160.0, |ui| {
                gauge(ui, d.health.performance, "Performance", pc, 140.0)
            });
            let lim = self.settings.temp_limits(&d.info);
            Self::card(ui, 160.0, |ui| {
                ui.add_space(14.0);
                match d.temperature_c {
                    Some(t) => {
                        ui.label(
                            RichText::new(self.temp(t))
                                .size(32.0)
                                .color(temp_color(t, lim)),
                        );
                    }
                    None => {
                        ui.label(RichText::new("–").size(32.0));
                    }
                }
                ui.label(RichText::new("Temperature").weak());
                ui.label(
                    RichText::new(format!(
                        "warn {} · crit {}",
                        self.temp(lim.0),
                        self.temp(lim.1)
                    ))
                    .weak()
                    .size(11.0),
                );
                ui.add_space(8.0);
            });
            if let Some(life) = d.life_remaining_pct {
                let lc = score_color(Some(life), HealthStatus::Unknown);
                Self::card(ui, 160.0, |ui| {
                    gauge(ui, Some(life), "Life remaining", lc, 140.0)
                });
            }
            Self::card(ui, 160.0, |ui| {
                ui.add_space(14.0);
                let h = d.power_on_hours.unwrap_or(0);
                ui.label(
                    RichText::new(if d.power_on_hours.is_some() {
                        format!("{:.1}", h as f64 / 8760.0)
                    } else {
                        "–".into()
                    })
                    .size(32.0),
                );
                ui.label(RichText::new("Years powered on").weak());
                ui.label(
                    RichText::new(
                        d.power_on_hours
                            .map(|h| format!("{h} hours"))
                            .unwrap_or_default(),
                    )
                    .weak()
                    .size(11.0),
                );
                ui.add_space(8.0);
            });
        });
        ui.add_space(8.0);

        egui::Frame::group(ui.style())
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Assessment").strong());
                for i in &d.health.issues {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(severity_color(i.severity), "●");
                        ui.label(&i.message);
                    });
                }
                if let Some(p) = &d.self_test_progress {
                    ui.horizontal(|ui| {
                        ui.colored_label(if p.running { BLUE } else { GRAY }, "●");
                        ui.label(&p.text);
                    });
                }
            });
        ui.add_space(8.0);

        let pts: Vec<HistoryPoint> = self
            .worker
            .history
            .lock()
            .map(|h| h.points(&d.info.key()).to_vec())
            .unwrap_or_default();

        let wide = ui.available_width() > 760.0;
        let info_rows = self.info_rows(d);
        let usage_rows = self.usage_rows(d, &pts);
        if wide {
            ui.columns(2, |cols| {
                kv_group(&mut cols[0], "Device", &info_rows);
                kv_group(&mut cols[1], "Usage & insights", &usage_rows);
            });
        } else {
            kv_group(ui, "Device", &info_rows);
            ui.add_space(8.0);
            kv_group(ui, "Usage & insights", &usage_rows);
        }
        if !d.warnings.is_empty() {
            ui.add_space(8.0);
            ui.collapsing(format!("Read notes ({})", d.warnings.len()), |ui| {
                for w in &d.warnings {
                    ui.label(RichText::new(w).weak());
                }
            });
        }
    }

    fn info_rows(&self, d: &DriveSnapshot) -> Vec<(String, String)> {
        let i = &d.info;
        let mut r = vec![
            ("Model".into(), i.display_name()),
            ("Serial number".into(), i.serial.clone()),
            ("Firmware".into(), i.firmware.clone()),
            (
                "Capacity".into(),
                format!(
                    "{} ({} bytes)",
                    format_bytes(i.capacity_bytes),
                    i.capacity_bytes
                ),
            ),
            ("Type".into(), i.media.label().into()),
            ("Interface".into(), i.bus.label().into()),
        ];
        if let Some(rpm) = i.rotation_rpm.filter(|&r| r > 0) {
            r.push(("Rotation speed".into(), format!("{rpm} RPM")));
        }
        if let Some(a) = &i.ata {
            if let (Some(c), Some(m)) = (a.sata_cur_gen, a.sata_max_gen) {
                r.push((
                    "SATA link".into(),
                    format!("{} (max {})", sata_gen_speed(c), sata_gen_speed(m)),
                ));
            }
            if let Some(v) = &a.ata_version {
                r.push(("ATA standard".into(), v.clone()));
            }
            if a.logical_sector_size > 0 {
                r.push((
                    "Sector size".into(),
                    format!(
                        "{} logical / {} physical",
                        a.logical_sector_size, a.physical_sector_size
                    ),
                ));
            }
        }
        if let Some(n) = &i.nvme {
            if let Some(v) = &n.version {
                r.push(("NVMe version".into(), v.clone()));
            }
            r.push(("Namespaces".into(), n.namespaces.to_string()));
        }
        r.push(("Device path".into(), i.path.clone()));
        if !i.volumes.is_empty() {
            r.push(("Volumes".into(), i.volumes.join(", ")));
        }
        r.push((
            "Data source".into(),
            format!(
                "{} – {}",
                i.backend,
                i.transport.clone().unwrap_or_else(|| "n/a".into())
            ),
        ));
        r
    }

    fn usage_rows(&self, d: &DriveSnapshot, pts: &[HistoryPoint]) -> Vec<(String, String)> {
        let mut r = Vec::new();
        if let Some(h) = d.power_on_hours {
            r.push(("Power-on time".into(), format_hours(h)));
        }
        if let Some(c) = d.power_cycles {
            r.push(("Power cycles".into(), c.to_string()));
        }
        if let Some(w) = d.host_written_bytes {
            r.push(("Total host writes".into(), format_bytes(w)));
        }
        if let Some(rd) = d.host_read_bytes {
            r.push(("Total host reads".into(), format_bytes(rd)));
        }
        if let (Some(w), Some(h)) = (d.host_written_bytes, d.power_on_hours.filter(|&h| h > 24)) {
            r.push((
                "Lifetime avg. writes".into(),
                format!(
                    "{}/day",
                    format_bytes((w as f64 / (h as f64 / 24.0)) as u64)
                ),
            ));
        }
        if let Some(wpd) = history::writes_per_day(pts) {
            r.push((
                "Recent writes (30d)".into(),
                format!("{}/day", format_bytes(wpd as u64)),
            ));
        }
        if let Some(l) = d.life_remaining_pct {
            r.push(("Remaining life".into(), format!("{l}%")));
        }
        if let Some(p) = history::life_projection(pts) {
            r.push((
                "Projected wear-out".into(),
                format!(
                    "{} ({:.3}%/day)",
                    p.end_of_life.with_timezone(&Local).format("%Y-%m-%d"),
                    p.pct_per_day
                ),
            ));
        } else if d.info.media.is_solid_state() {
            r.push((
                "Projected wear-out".into(),
                "needs ≥ 1 week of history".into(),
            ));
        }
        if let Some((mn, avg, mx)) = history::temp_stats(pts, Utc::now() - CDuration::days(1)) {
            r.push((
                "Temperature (24h)".into(),
                format!(
                    "min {} · avg {} · max {}",
                    self.temp(mn),
                    self.temp(avg.round() as i32),
                    self.temp(mx)
                ),
            ));
        }
        if let SmartData::Ata(a) = &d.smart {
            if let Some(e) = a.error_count {
                r.push(("ATA error log entries".into(), e.to_string()));
            }
        }
        if let SmartData::Nvme(n) = &d.smart {
            r.push(("Unsafe shutdowns".into(), n.unsafe_shutdowns.to_string()));
            r.push(("Error log entries".into(), n.error_log_entries.to_string()));
        }
        r.push(("History points".into(), pts.len().to_string()));
        r
    }

    fn smart_tab(&mut self, ui: &mut Ui, d: &DriveSnapshot) {
        match &d.smart {
            SmartData::Unavailable { reason } => {
                ui.add_space(10.0);
                ui.label(
                    RichText::new("SMART data is not available for this drive")
                        .size(16.0)
                        .strong(),
                );
                ui.label(reason);
                ui.add_space(8.0);
                ui.label(RichText::new("Tips: run DiskVigil with administrator/root rights; for USB enclosures with unusual bridge chips or RAID controllers, put smartctl (smartmontools) next to the executable so the automatic fallback can use it.").weak());
            }
            SmartData::Ata(a) => {
                ui.horizontal_wrapped(|ui| {
                    let (txt, col) = match a.overall_passed {
                        Some(true) => ("PASSED", GREEN),
                        Some(false) => ("FAILED", RED),
                        None => ("unknown", GRAY),
                    };
                    ui.label("SMART overall assessment:");
                    ui.colored_label(col, RichText::new(txt).strong());
                    ui.separator();
                    ui.label(format!("{} attributes", a.attributes.len()));
                    if let Some(e) = a.error_count {
                        ui.separator();
                        ui.label(format!("{e} logged errors"));
                    }
                    if !a.checksum_ok {
                        ui.separator();
                        ui.colored_label(AMBER, "checksum mismatch");
                    }
                });
                ui.label(RichText::new("Click an attribute to chart its raw value over time. Hover the name for an explanation.").weak().size(12.0));
                ui.add_space(4.0);
                let ssd = d.info.media.is_solid_state();
                egui::Grid::new("attrs")
                    .striped(true)
                    .num_columns(9)
                    .spacing([14.0, 5.0])
                    .min_col_width(10.0)
                    .show(ui, |ui| {
                        for h in [
                            "",
                            "ID",
                            "Attribute",
                            "Current",
                            "Worst",
                            "Threshold",
                            "Raw value",
                            "Type",
                            "Status",
                        ] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        for at in &a.attributes {
                            let col = match at.status {
                                AttrStatus::Ok => GREEN,
                                AttrStatus::Warning => AMBER,
                                AttrStatus::FailedInPast => ORANGE,
                                AttrStatus::FailingNow => RED,
                            };
                            ui.colored_label(col, "●");
                            ui.label(
                                RichText::new(format!("{:>3}  {:02X}", at.id, at.id)).monospace(),
                            );
                            let sel = self.chart_attr == Some(at.id as u16);
                            if ui
                                .selectable_label(sel, &at.name)
                                .on_hover_text(attributes::meta(at.id, ssd).description)
                                .clicked()
                            {
                                self.chart_attr = if sel { None } else { Some(at.id as u16) };
                            }
                            ui.label(at.value.to_string());
                            ui.label(at.worst.to_string());
                            ui.label(
                                at.threshold
                                    .map(|t| t.to_string())
                                    .unwrap_or_else(|| "–".into()),
                            );
                            ui.label(RichText::new(&at.raw_display).monospace());
                            ui.label(if at.prefailure { "Pre-fail" } else { "Old-age" });
                            ui.colored_label(col, at.status.label());
                            ui.end_row();
                        }
                    });
                if let Some(id) = self.chart_attr {
                    ui.add_space(10.0);
                    self.attr_chart(ui, d, id);
                }
            }
            SmartData::Nvme(n) => {
                let warn = nvme_critical_warning_bits(n.critical_warning);
                ui.horizontal(|ui| {
                    ui.label("Critical warning:");
                    if warn.is_empty() {
                        ui.colored_label(GREEN, RichText::new("none").strong());
                    } else {
                        ui.colored_label(RED, RichText::new(warn.join(", ")).strong());
                    }
                });
                ui.add_space(4.0);
                egui::Grid::new("nvme")
                    .striped(true)
                    .num_columns(2)
                    .spacing([24.0, 5.0])
                    .show(ui, |ui| {
                        for (k, v) in report::nvme_rows(d) {
                            ui.label(RichText::new(k).strong());
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label("Chart:");
                    for id in [
                        history::NVME_MEDIA_ERRORS,
                        history::NVME_SPARE,
                        history::NVME_PCT_USED,
                        history::NVME_UNSAFE_SHUTDOWNS,
                        history::NVME_ERROR_LOG,
                    ] {
                        let sel = self.chart_attr == Some(id);
                        if ui.selectable_label(sel, tracked_name(id)).clicked() {
                            self.chart_attr = if sel { None } else { Some(id) };
                        }
                    }
                });
                if let Some(id) = self.chart_attr {
                    self.attr_chart(ui, d, id);
                }
            }
        }
    }

    fn points_in_range(&self, d: &DriveSnapshot) -> Vec<HistoryPoint> {
        let since = self.range.since();
        self.worker
            .history
            .lock()
            .map(|h| {
                h.points(&d.info.key())
                    .iter()
                    .filter(|p| p.t >= since)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn range_picker(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label("Period:");
            for r in [
                Range::Day,
                Range::Week,
                Range::Month,
                Range::Year,
                Range::All,
            ] {
                ui.selectable_value(&mut self.range, r, r.label());
            }
        });
    }

    fn attr_chart(&mut self, ui: &mut Ui, d: &DriveSnapshot, id: u16) {
        self.range_picker(ui);
        let pts = self.points_in_range(d);
        let s = Series {
            name: tracked_name(id),
            color: BLUE,
            pts: pts
                .iter()
                .filter_map(|p| {
                    p.attrs
                        .get(&id)
                        .map(|v| (p.t.timestamp() as f64, *v as f64))
                })
                .collect(),
        };
        let note = if s.pts.is_empty() {
            " (not tracked in history yet)"
        } else {
            ""
        };
        chart(
            ui,
            &format!("{} – raw value{note}", tracked_name(id)),
            &[s],
            &|v| format!("{v:.0}"),
            ChartOpts {
                y_zero: true,
                ..Default::default()
            },
        );
    }

    fn history_tab(&mut self, ui: &mut Ui, d: &DriveSnapshot) {
        self.range_picker(ui);
        ui.add_space(6.0);
        let pts = self.points_in_range(d);
        let ts = |p: &HistoryPoint| p.t.timestamp() as f64;
        let lim = self.settings.temp_limits(&d.info);
        let unit = if self.settings.fahrenheit {
            "°F"
        } else {
            "°C"
        };
        let temps = Series {
            name: "Temperature".into(),
            color: ORANGE,
            pts: pts
                .iter()
                .filter_map(|p| p.temp.map(|t| (ts(p), self.temp_val(t as f64))))
                .collect(),
        };
        let refs = [
            (self.temp_val(lim.0 as f64), AMBER, "warning"),
            (self.temp_val(lim.1 as f64), RED, "critical"),
        ];
        chart(
            ui,
            "Temperature",
            &[temps],
            &|v| format!("{v:.0}{unit}"),
            ChartOpts {
                refs: &refs,
                min_span: 10.0,
                ..Default::default()
            },
        );
        ui.add_space(10.0);
        let health = Series {
            name: "Health".into(),
            color: GREEN,
            pts: pts
                .iter()
                .filter_map(|p| p.health.map(|h| (ts(p), h as f64)))
                .collect(),
        };
        let perf = Series {
            name: "Performance".into(),
            color: BLUE,
            pts: pts
                .iter()
                .filter_map(|p| p.perf.map(|h| (ts(p), h as f64)))
                .collect(),
        };
        let mut hs = vec![health, perf];
        if d.info.media.is_solid_state() {
            hs.push(Series {
                name: "Life remaining".into(),
                color: PURPLE,
                pts: pts
                    .iter()
                    .filter_map(|p| p.life.map(|h| (ts(p), h as f64)))
                    .collect(),
            });
        }
        chart(
            ui,
            "Health, performance and wear (%)",
            &hs,
            &|v| format!("{v:.0}%"),
            ChartOpts {
                y_zero: true,
                y_max: Some(100.0),
                min_span: 20.0,
                ..Default::default()
            },
        );
        ui.add_space(10.0);
        let writes = Series {
            name: "Host writes".into(),
            color: TEAL,
            pts: pts
                .iter()
                .filter_map(|p| p.written.map(|w| (ts(p), w as f64)))
                .collect(),
        };
        if !writes.pts.is_empty() {
            chart(
                ui,
                "Cumulative host writes",
                &[writes],
                &|v| format_bytes(v.max(0.0) as u64),
                ChartOpts::default(),
            );
            ui.add_space(10.0);
        }
        ui.label(
            RichText::new(format!(
                "{} points in this period. A point is stored every {} minutes, or immediately when something important changes.",
                pts.len(),
                self.settings.history_interval_mins
            ))
            .weak()
            .size(12.0),
        );
    }

    fn selftest_tab(&mut self, ui: &mut Ui, d: &DriveSnapshot) {
        if !d.smart.is_available() {
            ui.label("Self-tests need SMART access to this drive.");
            return;
        }
        let (short, ext, conv, conv_ok, supported) = match &d.smart {
            SmartData::Ata(a) => (
                a.short_test_minutes,
                a.extended_test_minutes,
                a.conveyance_test_minutes,
                a.conveyance_supported,
                a.self_test_supported,
            ),
            SmartData::Nvme(_) => (
                Some(2),
                d.info.nvme.as_ref().and_then(|n| n.extended_test_minutes),
                None,
                false,
                d.info.nvme.as_ref().is_some_and(|n| n.self_test_supported)
                    || d.info.backend == "demo",
            ),
            _ => (None, None, None, false, false),
        };
        if !supported {
            ui.label("This drive does not report self-test support.");
        }
        ui.label("Self-tests run inside the drive firmware and do not modify data. The drive stays usable but may be slower while a test runs.");
        ui.add_space(6.0);
        if let Some(p) = &d.self_test_progress {
            if p.running {
                let done = p
                    .remaining_pct
                    .map(|r| 1.0 - r as f32 / 100.0)
                    .unwrap_or(0.0);
                ui.add(egui::ProgressBar::new(done).text(&p.text).fill(BLUE));
            } else {
                ui.label(RichText::new(&p.text).strong());
            }
        }
        ui.add_space(6.0);
        let running = d.self_test_progress.as_ref().is_some_and(|p| p.running);
        ui.horizontal_wrapped(|ui| {
            let mins = |m: Option<u16>| m.map(|m| format!(" (~{m} min)")).unwrap_or_default();
            let mut start = |ui: &mut Ui, kind: SelfTestKind, label: String, enabled: bool| {
                if ui
                    .add_enabled(enabled && supported && !running, egui::Button::new(label))
                    .clicked()
                {
                    self.pending = Some(Pending::SelfTest(d.info.clone(), kind));
                }
            };
            start(
                ui,
                SelfTestKind::Short,
                format!("Short test{}", mins(short)),
                true,
            );
            start(
                ui,
                SelfTestKind::Extended,
                format!("Extended test{}", mins(ext)),
                true,
            );
            if !matches!(d.smart, SmartData::Nvme(_)) {
                start(
                    ui,
                    SelfTestKind::Conveyance,
                    format!("Conveyance test{}", mins(conv)),
                    conv_ok,
                );
            }
            if ui
                .add_enabled(running, egui::Button::new("Abort test"))
                .clicked()
            {
                self.worker
                    .send(Cmd::SelfTest(d.info.clone(), SelfTestKind::Abort));
            }
        });
        ui.add_space(12.0);
        ui.label(RichText::new("Self-test log (newest first)").strong());
        let log = d.self_test_log();
        if log.is_empty() {
            ui.label(RichText::new("No self-tests recorded.").weak());
        } else {
            egui::Grid::new("stlog")
                .striped(true)
                .num_columns(5)
                .spacing([18.0, 5.0])
                .show(ui, |ui| {
                    for h in ["", "Test", "Result", "At power-on hour", "First error LBA"] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    for e in log {
                        let c = match e.passed {
                            Some(true) => GREEN,
                            Some(false) => RED,
                            None => GRAY,
                        };
                        ui.colored_label(c, "●");
                        ui.label(&e.kind);
                        ui.colored_label(c, &e.status);
                        ui.label(e.lifetime_hours.to_string());
                        ui.label(
                            e.failing_lba
                                .map(|l| l.to_string())
                                .unwrap_or_else(|| "–".into()),
                        );
                        ui.end_row();
                    }
                });
        }
    }

    fn advanced_tab(&mut self, ui: &mut Ui, d: &DriveSnapshot) {
        if let Some(a) = d.info.ata.clone() {
            ui.label(RichText::new("ATA features").strong());
            egui::Grid::new("feat")
                .striped(true)
                .num_columns(2)
                .spacing([24.0, 4.0])
                .show(ui, |ui| {
                    let yn = |b: bool| if b { "Yes" } else { "No" };
                    let rows: Vec<(&str, String)> = vec![
                        ("48-bit LBA", yn(a.lba48).into()),
                        (
                            "NCQ",
                            if a.ncq {
                                format!("Yes (depth {})", a.ncq_depth.unwrap_or(0))
                            } else {
                                "No".into()
                            },
                        ),
                        ("TRIM", yn(a.trim).into()),
                        (
                            "Write cache",
                            format!(
                                "{}{}",
                                yn(a.write_cache_supported),
                                if a.write_cache_enabled {
                                    ", enabled"
                                } else {
                                    ""
                                }
                            ),
                        ),
                        (
                            "Security (ATA password)",
                            format!(
                                "{}{}",
                                yn(a.security_supported),
                                if a.security_enabled { ", ENABLED" } else { "" }
                            ),
                        ),
                        (
                            "Advanced Power Management",
                            if a.apm_supported {
                                a.apm_level
                                    .map(|l| format!("enabled, level {l}"))
                                    .unwrap_or("disabled".into())
                            } else {
                                "not supported".into()
                            },
                        ),
                        (
                            "Automatic Acoustic Management",
                            if a.aam_supported {
                                a.aam_level
                                    .map(|l| format!("enabled, level {l}"))
                                    .unwrap_or("disabled".into())
                            } else {
                                "not supported".into()
                            },
                        ),
                    ];
                    for (k, v) in rows {
                        ui.label(k);
                        ui.label(v);
                        ui.end_row();
                    }
                });
            let can_set = matches!(d.info.backend.as_str(), "native" | "demo");
            if a.apm_supported || a.aam_supported {
                ui.add_space(10.0);
                ui.label(RichText::new("Power & acoustics").strong());
                if !can_set {
                    ui.label(
                        RichText::new("Changing these settings needs the native backend.").weak(),
                    );
                }
                ui.label(RichText::new("Settings are applied to the drive immediately and usually reset at the next power cycle. A low APM level can cause frequent head parking (watch Load/Unload Cycle Count).").weak().size(12.0));
            }
            if a.apm_supported {
                ui.horizontal(|ui| {
                    ui.label("APM level");
                    ui.add(egui::Slider::new(&mut self.apm_edit, 1..=254));
                    ui.label(
                        RichText::new(match self.apm_edit {
                            1..=127 => "power saving, allows spin-down",
                            128..=253 => "power saving, no spin-down",
                            _ => "maximum performance",
                        })
                        .weak(),
                    );
                    if ui
                        .add_enabled(can_set, egui::Button::new("Apply"))
                        .clicked()
                    {
                        self.pending = Some(Pending::Feature(
                            d.info.clone(),
                            FeatureChange::Apm(Some(self.apm_edit)),
                            format!("Set APM level to {}", self.apm_edit),
                        ));
                    }
                    if ui
                        .add_enabled(can_set, egui::Button::new("Disable APM"))
                        .clicked()
                    {
                        self.pending = Some(Pending::Feature(
                            d.info.clone(),
                            FeatureChange::Apm(None),
                            "Disable APM".into(),
                        ));
                    }
                });
            }
            if a.aam_supported {
                ui.horizontal(|ui| {
                    ui.label("AAM level");
                    ui.add(egui::Slider::new(&mut self.aam_edit, 128..=254));
                    ui.label(
                        RichText::new(if self.aam_edit < 192 { "quiet" } else { "fast" }).weak(),
                    );
                    if ui
                        .add_enabled(can_set, egui::Button::new("Apply"))
                        .clicked()
                    {
                        self.pending = Some(Pending::Feature(
                            d.info.clone(),
                            FeatureChange::Aam(Some(self.aam_edit)),
                            format!("Set AAM level to {}", self.aam_edit),
                        ));
                    }
                    if ui
                        .add_enabled(can_set, egui::Button::new("Disable AAM"))
                        .clicked()
                    {
                        self.pending = Some(Pending::Feature(
                            d.info.clone(),
                            FeatureChange::Aam(None),
                            "Disable AAM".into(),
                        ));
                    }
                });
            }
            ui.add_space(10.0);
        }
        if let Some(n) = &d.info.nvme {
            ui.label(RichText::new("NVMe controller").strong());
            egui::Grid::new("nvmeinfo")
                .striped(true)
                .num_columns(2)
                .spacing([24.0, 4.0])
                .show(ui, |ui| {
                    let rows = [
                        ("Version", n.version.clone().unwrap_or_default()),
                        ("PCI vendor ID", format!("{:04X}", n.pci_vendor_id)),
                        ("Namespaces", n.namespaces.to_string()),
                        (
                            "Warning temperature",
                            n.warning_temp_c.map(|t| self.temp(t)).unwrap_or("–".into()),
                        ),
                        (
                            "Critical temperature",
                            n.critical_temp_c
                                .map(|t| self.temp(t))
                                .unwrap_or("–".into()),
                        ),
                        (
                            "Device self-test",
                            if n.self_test_supported {
                                "supported".into()
                            } else {
                                "not supported".into()
                            },
                        ),
                    ];
                    for (k, v) in rows {
                        ui.label(k);
                        ui.label(v);
                        ui.end_row();
                    }
                });
            ui.add_space(10.0);
        }
        ui.collapsing("Raw snapshot (JSON)", |ui| {
            let json = serde_json::to_string_pretty(d).unwrap_or_default();
            if ui.button("Copy to clipboard").clicked() {
                ui.ctx().copy_text(json.clone());
                self.toast("Copied snapshot JSON to the clipboard");
            }
            ui.add(
                egui::TextEdit::multiline(&mut json.as_str())
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(18),
            );
        });
    }

    // ------------------------------------------------------------------ windows

    fn apply_settings(&mut self) {
        self.settings = self.draft.clone();
        if let Err(e) = self.settings.save(&self.paths) {
            self.toast(format!("Could not save settings: {e}"));
        }
        self.worker.send(Cmd::Settings(self.settings.clone()));
        self.worker.send(Cmd::Refresh);
        self.applied_theme = None;
    }

    fn windows(&mut self, ctx: &egui::Context, v: &View) {
        if self.show_settings {
            let mut open = true;
            let mut save = false;
            let mut cancel = false;
            egui::Window::new("Settings")
                .open(&mut open)
                .default_width(520.0)
                .resizable(true)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(560.0)
                        .show(ui, |ui| {
                            self.settings_ui(ui, v);
                        });
                    ui.separator();
                    ui.horizontal(|ui| {
                        save = ui.button("Save").clicked();
                        cancel = ui.button("Cancel").clicked();
                    });
                });
            if save {
                self.apply_settings();
                self.toast("Settings saved");
            }
            self.show_settings = open && !save && !cancel;
        }
        if self.show_alerts {
            let mut open = true;
            egui::Window::new("Alerts").open(&mut open).default_width(640.0).default_height(420.0).show(ctx, |ui| {
                let alerts = self.worker.shared.lock().map(|g| g.alerts.clone()).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(format!("{} alerts (log: {})", alerts.len(), self.paths.alerts_file().display()));
                    if ui.button("Clear list").clicked() {
                        if let Ok(mut g) = self.worker.shared.lock() {
                            g.alerts.clear();
                        }
                    }
                    if ui.button("Test notification").clicked() {
                        crate::notify::desktop("DiskVigil", "This is a test notification.");
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    if alerts.is_empty() {
                        ui.label(RichText::new("No alerts yet. Alerts are raised when a drive's condition changes (new bad sectors, health drop, overheating, failed self-test…).").weak());
                    }
                    for a in alerts.iter().rev() {
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(severity_color(a.severity), "●");
                            ui.label(RichText::new(a.time.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()).weak().monospace());
                            ui.label(RichText::new(&a.drive_name).strong());
                            ui.label(&a.message);
                        });
                    }
                });
            });
            self.show_alerts = open;
        }
        if self.show_about {
            let mut open = true;
            egui::Window::new("About DiskVigil").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label(RichText::new(format!("DiskVigil {}", env!("CARGO_PKG_VERSION"))).size(16.0).strong());
                ui.label("Portable, cross-platform health monitor for hard disks, SATA SSDs and NVMe SSDs.");
                ui.add_space(6.0);
                ui.label("• Native ATA, SAT (USB) and NVMe access on Windows and Linux");
                ui.label("• smartctl fallback for macOS, RAID controllers and exotic USB bridges");
                ui.label("• Health & performance scoring, wear projection, history charts");
                ui.label("• Alerts via desktop notifications or your own command");
                ui.add_space(6.0);
                ui.label(RichText::new("Health scores are estimates derived from SMART data. Always keep backups.").weak());
            });
            self.show_about = open;
        }
        if let Some(p) = &self.pending {
            let (title, text) = match p {
                Pending::SelfTest(info, kind) => (
                    "Start self-test",
                    format!(
                        "Start a {} self-test on {}?\n\n\
                         • The drive runs the test internally. No data is written or erased.\n\
                         • The drive stays usable but may be slower until the test finishes (an extended test can take hours).\n\
                         • You can abort the test at any time from the Self-tests tab.",
                        kind.label().to_lowercase(),
                        info.display_name()
                    ),
                ),
                Pending::Feature(info, _, what) => (
                    "Change drive configuration",
                    format!(
                        "{what} on {}?\n\n\
                         WARNING: this changes the drive's own configuration (ATA SET FEATURES).\n\
                         • Aggressive power management (low APM levels) can cause frequent head parking and extra wear.\n\
                         • The change usually resets at the next power cycle. To undo it sooner, set the previous level again.\n\
                         • DiskVigil changes nothing else on your system.",
                        info.display_name()
                    ),
                ),
            };
            let mut decision: Option<bool> = None;
            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(text);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Continue").clicked() {
                            decision = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            decision = Some(false);
                        }
                    });
                });
            if let Some(go) = decision {
                if let Some(p) = self.pending.take() {
                    if go {
                        match p {
                            Pending::SelfTest(info, kind) => {
                                self.worker.send(Cmd::SelfTest(info, kind))
                            }
                            Pending::Feature(info, change, _) => {
                                self.worker.send(Cmd::Feature(info, change))
                            }
                        }
                    }
                }
            }
        }
    }

    fn settings_ui(&mut self, ui: &mut Ui, v: &View) {
        let s = &mut self.draft;
        ui.label(RichText::new("Monitoring").strong());
        egui::Grid::new("set1")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                ui.label("Refresh interval");
                ui.add(
                    egui::DragValue::new(&mut s.refresh_secs)
                        .range(10..=86_400)
                        .suffix(" s"),
                );
                ui.end_row();
                ui.label("History interval");
                ui.add(
                    egui::DragValue::new(&mut s.history_interval_mins)
                        .range(1..=1440)
                        .suffix(" min"),
                );
                ui.end_row();
                ui.label("Temperature unit");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut s.fahrenheit, false, "°C");
                    ui.radio_value(&mut s.fahrenheit, true, "°F");
                });
                ui.end_row();
                ui.label("Theme");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut s.theme, Theme::Dark, "Dark");
                    ui.radio_value(&mut s.theme, Theme::Light, "Light");
                });
                ui.end_row();
                ui.label("Backend");
                egui::ComboBox::from_id_salt("backend")
                    .selected_text(s.backend.label())
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        for m in [
                            BackendMode::Auto,
                            BackendMode::Native,
                            BackendMode::Smartctl,
                            BackendMode::Demo,
                        ] {
                            ui.selectable_value(&mut s.backend, m, m.label());
                        }
                    });
                ui.end_row();
                ui.label("smartctl path");
                let mut p = s.smartctl_path.clone().unwrap_or_default();
                ui.add(
                    egui::TextEdit::singleline(&mut p)
                        .hint_text(v.smartctl.clone().unwrap_or_else(|| "auto-detect".into()))
                        .desired_width(300.0),
                );
                s.smartctl_path = (!p.trim().is_empty()).then_some(p);
                ui.end_row();
            });
        ui.add_space(8.0);
        ui.label(RichText::new("Alerts").strong());
        let a = &mut s.alerts;
        ui.checkbox(&mut a.enabled, "Enable alerts");
        ui.add_enabled_ui(a.enabled, |ui| {
            ui.checkbox(&mut a.desktop_notifications, "Show desktop notifications");
            ui.checkbox(&mut a.temperature_alerts, "Temperature alerts");
            egui::Grid::new("set2").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                ui.label("Alert when health drops by");
                ui.add(egui::DragValue::new(&mut a.health_drop).range(1..=50).suffix(" %"));
                ui.end_row();
                ui.label("Alert when SSD life falls to");
                ui.add(egui::DragValue::new(&mut a.life_warn_pct).range(0..=50).suffix(" %"));
                ui.end_row();
                ui.label("Run command on alert");
                ui.add(egui::TextEdit::singleline(&mut a.command).hint_text("e.g. curl -d \"$DV_MESSAGE\" ntfy.sh/my-topic").desired_width(300.0));
                ui.end_row();
            });
            ui.label(RichText::new("The command receives DV_SEVERITY, DV_DRIVE, DV_MODEL, DV_SERIAL, DV_MESSAGE and DV_TIME as environment variables.").weak().size(11.5));
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Temperature limits per drive").strong());
        egui::Grid::new("set3")
            .num_columns(4)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                for d in &v.drives {
                    let key = d.info.key();
                    let def = dv_core::health::default_temp_limits(&d.info);
                    let cur = a.temp_overrides.get(&key).copied().unwrap_or(TempLimit {
                        warn: def.0,
                        crit: def.1,
                    });
                    let (mut w, mut c) = (cur.warn, cur.crit);
                    ui.label(d.info.display_name());
                    ui.add(
                        egui::DragValue::new(&mut w)
                            .range(20..=120)
                            .prefix("warn ")
                            .suffix(" °C"),
                    );
                    ui.add(
                        egui::DragValue::new(&mut c)
                            .range(20..=125)
                            .prefix("crit ")
                            .suffix(" °C"),
                    );
                    let reset = ui
                        .add_enabled(
                            a.temp_overrides.contains_key(&key),
                            egui::Button::new("Default"),
                        )
                        .clicked();
                    if reset {
                        a.temp_overrides.remove(&key);
                    } else if (w, c) != def || a.temp_overrides.contains_key(&key) {
                        a.temp_overrides.insert(
                            key,
                            TempLimit {
                                warn: w,
                                crit: c.max(w + 1),
                            },
                        );
                    }
                    ui.end_row();
                }
            });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Data folder: {}", self.paths.data_dir.display())).weak(),
            );
            if ui.small_button("Open").clicked() {
                platform::open_path(&self.paths.data_dir);
            }
        });
    }
}

fn pos(x: f32, y: f32) -> egui::Pos2 {
    egui::pos2(x, y)
}

fn kv_group(ui: &mut Ui, title: &str, rows: &[(String, String)]) {
    egui::Frame::group(ui.style())
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong());
            egui::Grid::new(title)
                .num_columns(2)
                .spacing([18.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    for (k, v) in rows {
                        ui.label(RichText::new(k).weak());
                        ui.label(v);
                        ui.end_row();
                    }
                });
        });
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.applied_theme != Some(self.settings.theme) {
            ctx.set_visuals(match self.settings.theme {
                Theme::Dark => egui::Visuals::dark(),
                Theme::Light => egui::Visuals::light(),
            });
            self.applied_theme = Some(self.settings.theme);
        }
        let v = {
            let Ok(mut g) = self.worker.shared.lock() else {
                return;
            };
            let msgs: Vec<String> = std::mem::take(&mut g.messages);
            let v = View {
                drives: g.drives.clone(),
                busy: g.busy,
                scanned_once: g.scanned_once,
                last_scan: g.last_scan,
                errors: g.errors.clone(),
                smartctl: g.smartctl.clone(),
                native: g.native,
                unseen: g.unseen_alerts,
            };
            drop(g);
            if let Some(m) = msgs.into_iter().last() {
                self.toast(m);
            }
            v
        };
        if !v.drives.iter().any(|d| Some(d.info.key()) == self.selected) {
            self.selected = v.drives.first().map(|d| d.info.key());
        }
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui, &v);
            ui.add_space(2.0);
        });
        if !self.elevated
            && self.settings.backend != BackendMode::Demo
            && (v.native || cfg!(target_os = "macos"))
        {
            egui::Panel::top("banner")
                .show_separator_line(false)
                .show(ui, |ui| self.banner(ui));
        }
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui, &v));
        egui::Panel::left("drives")
            .resizable(true)
            .default_size(300.0)
            .size_range(240.0..=460.0)
            .show(ui, |ui| self.drive_list(ui, &v));
        egui::CentralPanel::default_margins().show(ui, |ui| self.central(ui, &v));
        self.windows(&ctx, &v);
        ctx.request_repaint_after(Duration::from_secs(2));
    }
}
