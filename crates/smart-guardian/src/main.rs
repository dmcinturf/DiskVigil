// Release builds on Windows are GUI-subsystem executables (no console window when
// double-clicked). CLI subcommands re-attach to the parent console.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli;
mod gui;
mod notify;
mod platform;
mod worker;

use clap::{Parser, Subcommand, ValueEnum};
use sg_core::config::{BackendMode, Paths, Settings};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "smart-guardian",
    version,
    about = "Portable HDD / SSD / NVMe health monitor. Run without a command to open the GUI."
)]
pub struct Args {
    /// Drive access backend.
    #[arg(long, global = true, value_enum)]
    backend: Option<BackendArg>,
    /// Use simulated drives (shortcut for --backend demo).
    #[arg(long, global = true)]
    demo: bool,
    /// Directory for settings, history and reports (default: portable folder next to the executable).
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Path to smartctl (smartmontools) used as fallback backend.
    #[arg(long, global = true, value_name = "PATH")]
    smartctl: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum BackendArg {
    Auto,
    Native,
    Smartctl,
    Demo,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List detected drives with health, temperature and capacity.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show full details and SMART data for one or all drives.
    Info {
        /// Drive index, device path, serial or part of the model name (default: all).
        drive: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Health check for scripts. Exit code: 0 = OK, 1 = warning, 2 = critical/failed, 3 = no drives.
    Check {
        #[arg(short, long)]
        quiet: bool,
    },
    /// Write a report of all drives.
    Report {
        #[arg(short, long, value_enum, default_value = "html")]
        format: FormatArg,
        /// Output file (default: <data dir>/reports/...).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Run headless: periodically scan, record history and raise alerts.
    Monitor {
        /// Seconds between scans (default: the configured refresh interval).
        #[arg(short, long)]
        interval: Option<u64>,
        /// Scan once and exit.
        #[arg(long)]
        once: bool,
    },
    /// Start or abort a drive self-test.
    Selftest {
        drive: String,
        #[arg(value_enum)]
        kind: TestArg,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum FormatArg {
    Html,
    Json,
    Csv,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum TestArg {
    Short,
    Extended,
    Conveyance,
    Abort,
}

fn main() {
    if std::env::args_os().len() > 1 {
        platform::attach_console();
    }
    let args = Args::parse();
    let paths = Paths::resolve(args.data_dir.clone());
    let mut settings = Settings::load(&paths);
    if let Some(b) = args.backend {
        settings.backend = match b {
            BackendArg::Auto => BackendMode::Auto,
            BackendArg::Native => BackendMode::Native,
            BackendArg::Smartctl => BackendMode::Smartctl,
            BackendArg::Demo => BackendMode::Demo,
        };
    }
    if args.demo {
        settings.backend = BackendMode::Demo;
    }
    if let Some(p) = args.smartctl.clone() {
        settings.smartctl_path = Some(p);
    }

    let code = match args.command {
        None => match gui::run(settings, paths) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("GUI error: {e:#}");
                1
            }
        },
        Some(cmd) => cli::run(cmd, settings, paths),
    };
    std::process::exit(code);
}
