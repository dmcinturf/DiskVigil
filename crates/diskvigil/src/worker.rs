//! Background scanner thread for the GUI.

use chrono::{DateTime, Local};
use dv_core::alerts::{self, AlertEvent};
use dv_core::ata::FeatureChange;
use dv_core::config::{Paths, Settings};
use dv_core::history::HistoryStore;
use dv_core::service::Service;
use dv_core::{DriveInfo, DriveSnapshot, SelfTestKind};
use eframe::egui;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub enum Cmd {
    Refresh,
    SelfTest(DriveInfo, SelfTestKind),
    Feature(DriveInfo, FeatureChange),
    Settings(Settings),
}

#[derive(Default)]
pub struct Shared {
    pub drives: Vec<DriveSnapshot>,
    pub alerts: Vec<AlertEvent>,
    pub unseen_alerts: usize,
    pub busy: bool,
    pub scanned_once: bool,
    pub last_scan: Option<DateTime<Local>>,
    pub errors: Vec<String>,
    pub messages: Vec<String>,
    pub smartctl: Option<String>,
    pub native: bool,
}

pub struct Worker {
    tx: Sender<Cmd>,
    pub shared: Arc<Mutex<Shared>>,
    pub history: Arc<Mutex<HistoryStore>>,
}

impl Worker {
    pub fn spawn(settings: Settings, paths: Paths, ctx: egui::Context) -> Worker {
        let service = Service::new(settings, paths.clone());
        let history = service.history.clone();
        let shared = Arc::new(Mutex::new(Shared {
            alerts: alerts::load_log(&paths.alerts_file(), 500),
            smartctl: service.engine().smartctl_path(),
            native: service.engine().has_native(),
            busy: true,
            ..Default::default()
        }));
        let (tx, rx) = channel::<Cmd>();
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("smart-scanner".into())
            .spawn(move || {
                let mut svc = service;
                let mut next = Instant::now();
                loop {
                    let wait = next.saturating_duration_since(Instant::now());
                    match rx.recv_timeout(wait) {
                        Ok(Cmd::Refresh) | Err(RecvTimeoutError::Timeout) => {}
                        Ok(Cmd::SelfTest(info, kind)) => {
                            let msg = match svc.self_test(&info, kind) {
                                Ok(()) if kind == SelfTestKind::Abort => {
                                    format!("Self-test aborted on {}", info.display_name())
                                }
                                Ok(()) => format!(
                                    "{} self-test started on {}",
                                    kind.label(),
                                    info.display_name()
                                ),
                                Err(e) => format!(
                                    "Self-test failed to start on {}: {e:#}",
                                    info.display_name()
                                ),
                            };
                            push_msg(&sh, msg);
                            std::thread::sleep(Duration::from_millis(500));
                        }
                        Ok(Cmd::Feature(info, change)) => {
                            let msg = match svc.set_feature(&info, change) {
                                Ok(()) => format!("Setting applied to {}", info.display_name()),
                                Err(e) => format!(
                                    "Could not change setting on {}: {e:#}",
                                    info.display_name()
                                ),
                            };
                            push_msg(&sh, msg);
                        }
                        Ok(Cmd::Settings(s)) => {
                            svc.apply_settings(s);
                            if let Ok(mut g) = sh.lock() {
                                g.smartctl = svc.engine().smartctl_path();
                                g.native = svc.engine().has_native();
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    if let Ok(mut g) = sh.lock() {
                        g.busy = true;
                    }
                    ctx.request_repaint();
                    let (drives, events) = svc.scan();
                    crate::notify::dispatch(&events, &svc.settings.alerts);
                    let testing = drives
                        .iter()
                        .any(|d| d.self_test_progress.as_ref().is_some_and(|p| p.running));
                    if let Ok(mut g) = sh.lock() {
                        g.drives = drives;
                        g.unseen_alerts += events.len();
                        g.alerts.extend(events);
                        let n = g.alerts.len();
                        if n > 500 {
                            g.alerts.drain(..n - 500);
                        }
                        g.errors = svc.last_errors().to_vec();
                        g.last_scan = Some(Local::now());
                        g.busy = false;
                        g.scanned_once = true;
                    }
                    ctx.request_repaint();
                    let secs = if testing {
                        10
                    } else {
                        svc.settings.refresh_secs.max(5)
                    };
                    next = Instant::now() + Duration::from_secs(secs);
                }
            })
            .expect("failed to start scanner thread");
        Worker {
            tx,
            shared,
            history,
        }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

fn push_msg(sh: &Arc<Mutex<Shared>>, msg: String) {
    if let Ok(mut g) = sh.lock() {
        g.messages.push(msg);
    }
}
