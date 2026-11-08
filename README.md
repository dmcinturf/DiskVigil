# DiskVigil

A portable, cross-platform health monitor for **hard disks, SATA SSDs and NVMe SSDs**, written in Rust.
It is inspired by HDDGuardian, rebuilt from scratch with modern drives in mind.

![Overview](docs/overview.png)

## Highlights

| | HDDGuardian | DiskVigil |
|---|---|---|
| SATA / PATA hard disks | ✅ | ✅ native ATA pass-through |
| USB enclosures | ✅ (SAT) | ✅ native SAT, plus smartctl fallback for exotic bridges |
| **NVMe SSDs** | ❌ | ✅ native (Windows stornvme, Linux admin ioctl) |
| SSD wear / remaining life | partial | ✅ ATA wear attributes + NVMe *Percentage Used* |
| Health **and** performance score | ✅ | ✅ with a plain-language explanation of every deduction |
| Wear-out date projection | ❌ | ✅ linear regression over stored history |
| History charts | temperature only | temperature, health, performance, life, writes, any attribute |
| Self-tests | ✅ | ✅ short / extended / conveyance / abort with live progress |
| APM / AAM | ✅ | ✅ (with confirmation) |
| Alerts | tray / e-mail | desktop notifications + **custom command hook** (ntfy, curl, scripts…) |
| Reports | text | HTML, JSON, CSV |
| CLI / scripting | ❌ | ✅ `list`, `info`, `check` (exit codes), `report`, `monitor`, `selftest` |
| Platforms | Windows | **Windows, Linux, macOS** |
| Portable | ✅ | ✅ single executable, data folder next to it |

## Download

Get the latest portable build from the **[Releases page](https://github.com/dmcinturf/DiskVigil/releases/latest)**:

| Package | System |
|---|---|
| `diskvigil-…-windows-x86_64.zip` | Windows 10/11 (64-bit) |
| `diskvigil-…-linux-x86_64.tar.gz` | Linux x86_64 |
| `diskvigil-…-macos-apple-silicon.tar.gz` | macOS on M-series Macs |
| `diskvigil-…-macos-intel.tar.gz` | macOS on Intel Macs |

The binaries aren't code-signed. On Windows, SmartScreen may ask you to confirm (*More info → Run anyway*). On macOS,
run `xattr -d com.apple.quarantine diskvigil` once after extracting.
See the [code signing policy](#code-signing-policy) and [privacy](#privacy) sections.

### Verifying downloads

Every release package has a [build provenance attestation](https://docs.github.com/actions/security-for-github-actions/using-artifact-attestations)
signed with [Sigstore](https://www.sigstore.dev/). It proves the file was built by this repository's GitHub Actions
workflow from a specific commit and hasn't been modified since. With the [GitHub CLI](https://cli.github.com/):

```
gh attestation verify diskvigil-v0.1.0-windows-x86_64.zip --repo dmcinturf/DiskVigil
```

`SHA256SUMS.txt` in each release lists the checksums of all packages.

## Using it

Extract the package (or build it yourself) and run `diskvigil` (`diskvigil.exe` on Windows). No installation is needed.

* **Windows:** run as Administrator, or use the *Restart as Administrator* button.
* **Linux:** run with `sudo`, or use the *Restart with pkexec* button. Temperatures from hwmon work without root.
* **macOS:** install smartmontools (`brew install smartmontools`) and run with `sudo`. DiskVigil uses `smartctl` on macOS.
* Want to look around first? `diskvigil --demo` shows simulated drives, including a failing one.

### Portable data

Settings, history (`history/*.jsonl`), the alert log and reports live in a `DiskVigilData` folder next to the
executable when that location is writable. Otherwise they go to the per-user data directory
(`%LOCALAPPDATA%`, `~/.local/share`, `~/Library/Application Support`). Override this with `--data-dir` or `DV_DATA_DIR`.

### Optional: smartctl fallback

If `smartctl` is placed next to the executable (or in a `smartmontools` subfolder, on `PATH`, or in its default install
location), the **Automatic** backend uses it for drives that native access can't read. Examples: RAID controllers,
JMicron/Realtek USB-NVMe bridges, and Intel RST setups.

### Command line

```
diskvigil list [--json]
diskvigil info [DRIVE] [--json]     # DRIVE = index, device path, serial or part of the model
diskvigil check [--quiet]           # exit 0 OK, 1 warning, 2 critical/failed, 3 no drives
diskvigil report --format html|json|csv [--output FILE]
diskvigil monitor [--interval 300] [--once]
diskvigil selftest DRIVE short|extended|conveyance|abort
Global options: --backend auto|native|smartctl|demo, --demo, --data-dir DIR, --smartctl PATH
```

`monitor` is a headless mode for servers or scheduled tasks. It records history and raises the same alerts as the GUI.

### Alert command hook

Settings → *Run command on alert* runs any shell command, with these environment variables set:
`DV_SEVERITY`, `DV_DRIVE`, `DV_MODEL`, `DV_SERIAL`, `DV_MESSAGE`, `DV_TIME`. For example:

```
curl -d "$DV_MODEL: $DV_MESSAGE" ntfy.sh/my-disks
```

Alerts fire on **changes**, so a stable but degraded drive doesn't spam you. Triggers include new reallocated, pending or
uncorrectable sectors, a health drop, crossing a temperature limit (with hysteresis), a new self-test failure, NVMe
critical-warning bits, and SSD life falling below a threshold.

## How health is scored

Every drive starts at 100% health and 100% performance. Deductions are shown in the *Assessment* list.

* **Health:** reallocated, pending and offline-uncorrectable sectors weigh most. Each uses diminishing-returns curves,
  so one bad sector is a small hit and hundreds are a large one. Uncorrectable errors, spin retries, end-to-end errors,
  flash program/erase failures and NVMe media errors also count. SSD health is scaled by the remaining rated life.
  A SMART *FAILED* status, a pre-fail attribute below its threshold, NVMe read-only/degraded flags, or a failed
  self-test caps the score.
* **Performance:** interface CRC errors (usually cabling), command timeouts, thermal throttling or time spent above the
  temperature limits, failed throughput/seek attributes, and a SATA link running slower than the drive supports.

These scores are estimates. Keep backups regardless.

## Building

Requires Rust 1.80+.

```
cargo build --release
cargo test --workspace
```

On Linux the GUI needs the usual X11/Wayland development packages
(`libxkbcommon-dev libgl1-mesa-dev libwayland-dev libx11-dev`). The included GitHub Actions workflow builds and tests
Windows, Linux and macOS (Intel and Apple Silicon).

## Architecture

```
crates/
  dv-core/                 library: no GUI dependencies
    ata/                   ATA command set, IDENTIFY / SMART / self-test log decoding, SAT (SCSI ATA translation)
    nvme.rs                Identify Controller, SMART/Health log, self-test log decoding
    backend/windows.rs     IOCTL_ATA_PASS_THROUGH, IOCTL_SCSI_PASS_THROUGH (SAT), NVMe protocol queries
    backend/linux.rs       SG_IO (SAT), NVME_IOCTL_ADMIN_CMD, sysfs, hwmon
    backend/smartctl.rs    smartctl --json backend / fallback (all platforms, required on macOS)
    backend/demo.rs        simulated drives
    health.rs              derived metrics + health/performance scoring
    history.rs             JSON Lines history, trend analysis, wear projection
    alerts.rs              change-detection alert engine
    report.rs              HTML / JSON / CSV
    service.rs             scan → history → alerts pipeline shared by the GUI and `monitor`
  diskvigil/               the app: egui GUI + clap CLI in one portable binary
```

## Known limitations

* The Windows inbox NVMe driver doesn't allow starting NVMe self-tests. Use the smartctl backend or the vendor's tool.
  Health logs and self-test *logs* read fine.
* Some USB-to-NVMe bridges and hardware RAID controllers need vendor-specific commands. Use the smartctl fallback.
* SAS/SCSI drives report health status through smartctl only. There is no attribute table.
* APM/AAM changes usually reset at the next power cycle.

## Code signing policy

Free code signing for Windows is provided by [SignPath.io](https://about.signpath.io/), with a certificate by the
[SignPath Foundation](https://signpath.org/). *(Application pending: current releases are not yet signed.)*

* Only executables built by this repository's GitHub Actions workflow from its own source code are signed.
* Every signing request is approved manually by a project approver before a release is published.
* Signed files carry the product name **DiskVigil** and the release version in their file properties.

Team roles:

| Role | Members |
|---|---|
| Committers and reviewers | [dmcinturf](https://github.com/dmcinturf) |
| Approvers | [dmcinturf](https://github.com/dmcinturf) |

All team members use multi-factor authentication for GitHub and SignPath.

## Privacy

DiskVigil does not collect telemetry and does not send any information to other networked systems unless you
specifically configure it to. The only feature that can transmit data is the optional *Run command on alert* hook,
which runs a command that you write yourself. Everything else (settings, history, alert log, reports) stays in the
local data folder.

DiskVigil can use [smartmontools](https://www.smartmontools.org/) (`smartctl`) if you install it. smartctl runs
locally and doesn't send data anywhere.

## License

[MIT](LICENSE)
