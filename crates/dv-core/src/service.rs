//! Monitoring service: scan -> history -> alerts. Shared by the GUI worker thread
//! and the headless `monitor` command.

use crate::alerts::{append_log, AlertEngine, AlertEvent};
use crate::ata::FeatureChange;
use crate::backend::Engine;
use crate::config::{Paths, Settings};
use crate::history::HistoryStore;
use crate::model::*;
use anyhow::Result;
use chrono::Duration;
use std::sync::{Arc, Mutex};

pub struct Service {
    pub settings: Settings,
    pub paths: Paths,
    engine: Engine,
    pub history: Arc<Mutex<HistoryStore>>,
    alerts: AlertEngine,
}

impl Service {
    pub fn new(settings: Settings, paths: Paths) -> Self {
        let engine = Engine::new(settings.backend, settings.smartctl_path.as_deref());
        let history = HistoryStore::open(paths.history_dir());
        let mut alerts = AlertEngine::new();
        for key in history.keys() {
            if let Some(last) = history.last(&key) {
                alerts.seed(&key, last, &settings.alerts);
            }
        }
        Service {
            settings,
            paths,
            engine,
            history: Arc::new(Mutex::new(history)),
            alerts,
        }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn apply_settings(&mut self, s: Settings) {
        if s.backend != self.settings.backend || s.smartctl_path != self.settings.smartctl_path {
            self.engine = Engine::new(s.backend, s.smartctl_path.as_deref());
        }
        self.settings = s;
    }

    /// Scans all drives, records history and evaluates alerts (which are also
    /// appended to the alert log).
    pub fn scan(&mut self) -> (Vec<DriveSnapshot>, Vec<AlertEvent>) {
        let drives = self.engine.scan();
        let interval = Duration::minutes(self.settings.history_interval_mins.max(1) as i64);
        if let Ok(mut h) = self.history.lock() {
            for d in &drives {
                h.record(d, interval);
            }
        }
        let mut events = Vec::new();
        for d in &drives {
            let limits = self.settings.temp_limits(&d.info);
            events.extend(self.alerts.evaluate(d, &self.settings.alerts, limits));
        }
        append_log(&self.paths.alerts_file(), &events);
        (drives, events)
    }

    pub fn last_errors(&self) -> &[String] {
        &self.engine.last_errors
    }

    pub fn self_test(&mut self, info: &DriveInfo, kind: SelfTestKind) -> Result<()> {
        self.engine.self_test(info, kind)
    }

    pub fn set_feature(&mut self, info: &DriveInfo, change: FeatureChange) -> Result<()> {
        self.engine.set_feature(info, change)
    }
}
