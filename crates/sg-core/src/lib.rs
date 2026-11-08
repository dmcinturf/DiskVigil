//! SMART Guardian core library.
//!
//! * [`backend`] talks to drives (native ATA / SAT / NVMe pass-through on Windows and
//!   Linux, `smartctl` everywhere, and a demo backend).
//! * [`ata`] and [`nvme`] decode raw device structures.
//! * [`health`] turns raw SMART data into health / performance scores and issues.
//! * [`history`], [`alerts`], [`report`] and [`service`] build the monitoring product
//!   on top of that.

pub mod alerts;
pub mod ata;
pub mod backend;
pub mod config;
pub mod health;
pub mod history;
pub mod model;
pub mod nvme;
pub mod report;
pub mod service;
pub mod util;

pub use model::*;
