// SPDX-License-Identifier: GPL-3.0-or-later
//! Command line interface of the USB Flash Memory Toolset.

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use usbflash_core::device::RangeList;
use usbflash_core::format::FsChoice;
use usbflash_core::guard::{self, Confirmation};
use usbflash_core::health::Verdict;
use usbflash_core::report::Report;
use usbflash_core::reset::{EraseChoice, ResetOptions, TableChoice};
use usbflash_core::scan::ScanMode;
use usbflash_core::units::{bytes, duration, speed};
use usbflash_core::wipe::{WipeMode, WipeStatus};
use usbflash_core::{CancelFlag, Ctx, DeviceInfo, Error, Overrides, Result, Target, ops};

#[derive(Parser)]
#[command(name = "usbflash", version, about = "Analyze, test, wipe and reset USB flash drives")]
struct Cli {
    /// Print results as JSON
    #[arg(long, global = true)]
    json: bool,
    /// Treat DEVICE as a disk image file. Useful to try things out safely.
    #[arg(long, global = true)]
    image: bool,
    /// Sector size of an image file
    #[arg(long, global = true, default_value_t = 512, value_name = "BYTES")]
    sector_size: u32,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct DestructiveArgs {
    /// Skip the confirmation prompt
    #[arg(long)]
    yes: bool,
    /// Also allow devices that report fixed media, which could be external disks
    #[arg(long)]
    allow_fixed: bool,
}

#[derive(Subcommand)]
enum Command {
    /// List USB and SD devices
    List {
        /// Show all disks, including internal ones (which are never modified)
        #[arg(long)]
        all: bool,
    },
    /// Show partitions, filesystems and what kind of data is on a device
    Analyze { device: String },
    /// Read test and SMART data, with a health verdict
    Health {
        device: String,
        /// Read every sector instead of samples
        #[arg(long)]
        full: bool,
    },
    /// Find bad flash and fake capacity by writing and reading back. DESTROYS ALL DATA
    Test {
        device: String,
        /// Write every sector instead of probes. Slow, but authoritative
        #[arg(long)]
        full: bool,
        #[command(flatten)]
        safety: DestructiveArgs,
    },
    /// Overwrite the entire device. DESTROYS ALL DATA
    Wipe {
        device: String,
        #[arg(long, value_enum, default_value_t = WipeArg::Random)]
        mode: WipeArg,
        /// Skip reading back the written data
        #[arg(long)]
        no_verify: bool,
        #[command(flatten)]
        safety: DestructiveArgs,
    },
    /// Restore the factory state: erase, new partition, new filesystem. DESTROYS ALL DATA
    Reset {
        device: String,
        /// Only clear metadata instead of overwriting everything. Old data stays recoverable
        #[arg(long)]
        quick: bool,
        /// Pattern for the full erase
        #[arg(long, value_enum, default_value_t = WipeArg::Random)]
        mode: WipeArg,
        #[arg(long, value_enum, default_value_t = FsArg::Auto)]
        fs: FsArg,
        #[arg(long, value_enum, default_value_t = TableArg::Auto)]
        table: TableArg,
        /// Volume label
        #[arg(long, default_value = "")]
        label: String,
        #[command(flatten)]
        safety: DestructiveArgs,
    },
    /// Save a device to an image file, tolerating read errors
    Backup { device: String, file: PathBuf },
    /// Write an image file to a device. DESTROYS ALL DATA
    Restore {
        file: PathBuf,
        device: String,
        #[command(flatten)]
        safety: DestructiveArgs,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum WipeArg {
    Zero,
    Random,
    RandomZero,
}

impl From<WipeArg> for WipeMode {
    fn from(a: WipeArg) -> Self {
        match a {
            WipeArg::Zero => WipeMode::Zero,
            WipeArg::Random => WipeMode::Random,
            WipeArg::RandomZero => WipeMode::RandomThenZero,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum FsArg {
    Auto,
    Fat,
    Exfat,
    None,
}

#[derive(Clone, Copy, ValueEnum)]
enum TableArg {
    Auto,
    Mbr,
    Gpt,
    None,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(true) => ExitCode::SUCCESS,
        // The operation ran, but its result is negative (e.g. bad blocks found).
        Ok(false) => ExitCode::from(2),
        Err(Error::Cancelled) => {
            eprintln!("\nCancelled. The device may be in an incomplete state.");
            ExitCode::from(130)
        }
        Err(e) => {
            eprintln!("error: {e}");
            if matches!(e, Error::PermissionDenied(_)) {
                eprintln!("hint: run from an elevated terminal (Windows) or with sudo (Linux)");
            }
            ExitCode::FAILURE
        }
    }
}

/// Builds a context printing a single updating status line to stderr.
fn context(cli: &Cli) -> Ctx {
    let cancel = CancelFlag::default();
    let handler = cancel.clone();
    // A second Ctrl-C after a failed registration simply kills the process.
    ctrlc_register(move || handler.store(true, Ordering::Relaxed));
    let visible = !cli.json && std::io::stderr().is_terminal();
    let mut last_phase = String::new();
    Ctx::new(cancel, move |p| {
        if !visible {
            return;
        }
        let mut err = std::io::stderr();
        if p.phase != last_phase {
            if !last_phase.is_empty() {
                let _ = writeln!(err);
            }
            last_phase.clone_from(&p.phase);
        }
        let eta = p.eta_secs.map(duration).unwrap_or_default();
        let bad = if p.errors > 0 { format!("  {} failed", bytes(p.errors)) } else { String::new() };
        let _ = if p.total > 0 {
            write!(err, "\r{}: {:5.1}%  {}  ETA {eta}{bad}    ", p.phase, p.fraction() * 100.0, speed(p.bytes_per_sec))
        } else {
            write!(err, "\r{}...    ", p.phase)
        };
        let _ = err.flush();
    })
}

#[cfg(windows)]
fn ctrlc_register(f: impl Fn() + Send + 'static) {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    static HANDLER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    let shared = std::sync::Mutex::new(f);
    if HANDLER.set(Box::new(move || (shared.lock().unwrap())())).is_err() {
        return;
    }
    unsafe extern "system" fn on_event(_: u32) -> i32 {
        if let Some(h) = HANDLER.get() {
            h();
        }
        1
    }
    // SAFETY: registers a static function that lives for the entire process.
    unsafe { SetConsoleCtrlHandler(Some(on_event), 1) };
}

#[cfg(unix)]
fn ctrlc_register(f: impl Fn() + Send + 'static) {
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicBool;
    static FLAG: AtomicBool = AtomicBool::new(false);
    static STARTED: OnceLock<()> = OnceLock::new();
    extern "C" fn on_signal(_: libc::c_int) {
        FLAG.store(true, Ordering::Relaxed);
    }
    if STARTED.set(()).is_err() {
        return;
    }
    // SAFETY: the handler only touches an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    std::thread::spawn(move || {
        while !FLAG.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        f();
    });
}

#[cfg(not(any(windows, unix)))]
fn ctrlc_register(_: impl Fn() + Send + 'static) {}

fn end_progress(cli: &Cli) {
    if !cli.json && std::io::stderr().is_terminal() {
        eprintln!();
    }
}

fn find_device(name: &str) -> Result<DeviceInfo> {
    let devices = ops::list_devices()?;
    let canonical = |s: &str| s.trim_start_matches("\\\\.\\").to_lowercase();
    let matches: Vec<&DeviceInfo> = devices
        .iter()
        .filter(|d| canonical(&d.path) == canonical(name) || (!d.serial.is_empty() && d.serial == name))
        .collect();
    match matches.as_slice() {
        [d] => Ok((*d).clone()),
        [] => Err(Error::Invalid(format!("no device '{name}'; see 'usbflash list'"))),
        _ => Err(Error::Invalid(format!("'{name}' matches several devices; use the path"))),
    }
}

fn target(cli: &Cli, name: &str) -> Result<Target> {
    if cli.image {
        Ok(Target::Image { path: PathBuf::from(name), sector_size: cli.sector_size })
    } else {
        Ok(Target::Device(find_device(name)?))
    }
}

/// Applies the policy up front for a clear message, and obtains confirmation.
/// `ops` checks the policy again, right before opening the device.
fn confirm(t: &Target, action: &str, safety: &DestructiveArgs) -> Result<Overrides> {
    let overrides = Overrides { allow_fixed: safety.allow_fixed };
    let Target::Device(d) = t else { return Ok(overrides) };
    guard::check_destructive(d, overrides)?;
    if safety.yes {
        return Ok(overrides);
    }
    if !std::io::stdin().is_terminal() {
        return Err(Error::Refused("confirmation needed; pass --yes in scripts".to_string()));
    }
    eprintln!("\nThis will {action} on:\n");
    print_device(d, &mut std::io::stderr());
    eprintln!();
    let word = match guard::required_confirmation(d) {
        Confirmation::Simple => "yes",
        Confirmation::Typed => {
            eprintln!("WARNING: this may be an external disk rather than a flash stick.");
            "ERASE"
        }
    };
    eprint!("ALL DATA WILL BE LOST. Type '{word}' to continue: ");
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if answer.trim() != word {
        return Err(Error::Cancelled);
    }
    Ok(overrides)
}

fn print_device(d: &DeviceInfo, out: &mut dyn Write) {
    let _ = writeln!(out, "  {}  ({})", d.display_name(), d.path);
    let _ = writeln!(out, "  Size:    {}", if d.media_present { bytes(d.size_bytes) } else { "no media".into() });
    let serial = if d.serial.is_empty() { "unknown" } else { &d.serial };
    let _ = writeln!(out, "  Serial:  {serial}");
    let _ = writeln!(out, "  Bus:     {}{}", d.bus.label(), if d.removable_media { ", removable" } else { ", fixed" });
    if !d.mount_points.is_empty() {
        let _ = writeln!(out, "  Mounted: {}", d.mount_points.join(", "));
    }
}

fn emit<T: Serialize>(cli: &Cli, name: &str, t: &Target, result: &T, human: impl FnOnce()) {
    if cli.json {
        println!("{}", Report::new(name, t.device_info(), result).to_json());
    } else {
        human();
    }
}

fn print_ranges(label: &str, r: &RangeList) {
    if r.is_empty() {
        return;
    }
    println!("  {label}: {} in {} area(s)", bytes(r.total()), r.ranges.len());
    for x in r.ranges.iter().take(8) {
        println!("    at {:>14}  {}", x.offset, bytes(x.len));
    }
    if r.ranges.len() > 8 {
        println!("    ... and {} more", r.ranges.len() - 8);
    }
}

fn run(cli: &Cli) -> Result<bool> {
    match &cli.command {
        Command::List { all } => cmd_list(cli, *all),
        Command::Analyze { device } => cmd_analyze(cli, device),
        Command::Health { device, full } => cmd_health(cli, device, *full),
        Command::Test { device, full, safety } => cmd_test(cli, device, *full, safety),
        Command::Wipe { device, mode, no_verify, safety } => cmd_wipe(cli, device, (*mode).into(), !no_verify, safety),
        Command::Reset { device, quick, mode, fs, table, label, safety } => {
            let opts = ResetOptions {
                erase: if *quick { EraseChoice::Quick } else { EraseChoice::Full((*mode).into()) },
                filesystem: match fs {
                    FsArg::Auto => FsChoice::Auto,
                    FsArg::Fat => FsChoice::Fat,
                    FsArg::Exfat => FsChoice::Exfat,
                    FsArg::None => FsChoice::None,
                },
                table: match table {
                    TableArg::Auto => TableChoice::Auto,
                    TableArg::Mbr => TableChoice::Mbr,
                    TableArg::Gpt => TableChoice::Gpt,
                    TableArg::None => TableChoice::None,
                },
                label: label.clone(),
            };
            cmd_reset(cli, device, &opts, safety)
        }
        Command::Backup { device, file } => cmd_backup(cli, device, file),
        Command::Restore { file, device, safety } => cmd_restore(cli, file, device, safety),
    }
}

fn cmd_list(cli: &Cli, all: bool) -> Result<bool> {
    let devices: Vec<DeviceInfo> =
        ops::list_devices()?.into_iter().filter(|d| all || (d.bus.is_flash_bus() && !d.is_system)).collect();
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&devices).unwrap_or_default());
        return Ok(true);
    }
    if devices.is_empty() {
        println!("No USB or SD devices found. 'usbflash list --all' shows all disks.");
        return Ok(true);
    }
    println!("{:<22} {:>10}  {:<5} {:<9} {:<28} {:<20} Mounted", "DEVICE", "SIZE", "BUS", "STATE", "NAME", "SERIAL");
    for d in &devices {
        let size = if d.media_present { bytes(d.size_bytes) } else { "no media".into() };
        let state = if d.is_system {
            "SYSTEM"
        } else if d.read_only {
            "read-only"
        } else if d.removable_media {
            "ok"
        } else {
            "fixed"
        };
        println!(
            "{:<22} {:>10}  {:<5} {:<9} {:<28} {:<20} {}",
            d.path,
            size,
            d.bus.label(),
            state,
            d.display_name(),
            d.serial,
            d.mount_points.join(", ")
        );
    }
    Ok(true)
}

fn cmd_analyze(cli: &Cli, device: &str) -> Result<bool> {
    let t = target(cli, device)?;
    let a = ops::analyze(&t, &mut context(cli))?;
    end_progress(cli);
    emit(cli, "analyze", &t, &a, || {
        if let Target::Device(d) = &t {
            print_device(d, &mut std::io::stdout());
        }
        println!("  Layout:  {} ({} partition(s))", a.table_kind.label(), a.partitions.len());
        println!("  Content: {}", a.content.label());
        if let Some(f) = &a.whole_device_fs {
            println!("\n  Whole device: {} {}", f.name, f.label);
        }
        for p in &a.partitions {
            let fs = p.filesystem.as_ref();
            println!(
                "\n  Partition {}: {} at {}, {}",
                p.partition.index,
                bytes(p.partition.size),
                bytes(p.partition.start),
                p.partition.type_name
            );
            match fs {
                Some(f) => {
                    println!("    Filesystem: {}", f.name);
                    if !f.label.is_empty() {
                        println!("    Label:      {}", f.label);
                    }
                    if !f.uuid.is_empty() {
                        println!("    ID:         {}", f.uuid);
                    }
                }
                None => println!("    Filesystem: not recognized"),
            }
        }
        if !a.findings.is_empty() {
            println!("\n  Findings:");
            for f in &a.findings {
                println!("    - {f}");
            }
        }
    });
    Ok(true)
}

fn cmd_health(cli: &Cli, device: &str, full: bool) -> Result<bool> {
    if cli.image {
        return Err(Error::Invalid("health needs a real device".to_string()));
    }
    let d = find_device(device)?;
    let mode = if full { ScanMode::Full } else { ScanMode::Quick };
    let h = ops::health(&d, mode, &mut context(cli))?;
    end_progress(cli);
    let t = Target::Device(d.clone());
    emit(cli, "health", &t, &h, || {
        print_device(&d, &mut std::io::stdout());
        println!("\n  Verdict: {}", h.verdict.label().to_uppercase());
        for r in &h.reasons {
            println!("    - {r}");
        }
        if let Some(s) = &h.scan {
            println!(
                "\n  Read test ({:?}): {} read, avg {}, min {}, max {}",
                s.mode,
                bytes(s.bytes_read),
                speed(s.avg_bytes_per_sec),
                speed(s.min_bytes_per_sec),
                speed(s.max_bytes_per_sec)
            );
            print_ranges("Unreadable", &s.unreadable);
        }
        match &h.smart {
            Some(usbflash_core::smart::SmartOutcome::Available { attributes }) => {
                println!("\n  SMART attributes:");
                for a in attributes {
                    println!("    {:>3} {:<32} current {:>3}  raw {}", a.id, a.name, a.current, a.raw);
                }
            }
            Some(usbflash_core::smart::SmartOutcome::Unavailable { reason }) => {
                println!("\n  SMART: not available ({reason})");
            }
            None => {}
        }
        if !h.notes.is_empty() {
            println!();
            for n in &h.notes {
                println!("  Note: {n}");
            }
        }
    });
    Ok(h.verdict == Verdict::Good)
}

fn cmd_test(cli: &Cli, device: &str, full: bool, safety: &DestructiveArgs) -> Result<bool> {
    let t = target(cli, device)?;
    let overrides = confirm(&t, "run a destructive capacity test", safety)?;
    let mode = if full { ScanMode::Full } else { ScanMode::Quick };
    let r = ops::capacity_test(&t, mode, overrides, &mut context(cli))?;
    end_progress(cli);
    emit(cli, "capacity-test", &t, &r, || {
        if r.passed() {
            println!("PASSED: all tested data was stored correctly.");
        } else if r.looks_fake {
            println!("FAKE CAPACITY SUSPECTED: this stick claims {}.", bytes(r.claimed_bytes));
            if let Some(real) = r.estimated_real_bytes {
                println!("  Estimated real capacity: {}", bytes(real));
            }
        } else {
            println!("FAILED: the device does not store data reliably.");
        }
        if !r.passed() {
            println!("  First failure at offset {}", bytes(r.verified_bytes));
        }
        if r.aborted {
            println!("  The device stopped responding.");
        }
        println!("  Write speed {}, read speed {}", speed(r.write_bytes_per_sec), speed(r.read_bytes_per_sec));
        print_ranges("Write failures", &r.write_failures);
        print_ranges("Wrong data", &r.verify_failures);
        if !full && r.passed() {
            println!("  Quick test: only probes were checked. --full is authoritative.");
        }
    });
    Ok(r.passed())
}

fn cmd_wipe(cli: &Cli, device: &str, mode: WipeMode, verify: bool, safety: &DestructiveArgs) -> Result<bool> {
    let t = target(cli, device)?;
    let overrides = confirm(&t, "overwrite everything", safety)?;
    let r = ops::wipe(&t, mode, verify, overrides, &mut context(cli))?;
    end_progress(cli);
    emit(cli, "wipe", &t, &r, || {
        match r.status {
            WipeStatus::Verified => println!("WIPED and verified: {}, {}", bytes(r.bytes), mode.label()),
            WipeStatus::WrittenUnverified => println!("WRITTEN, not verified: {}, {}", bytes(r.bytes), mode.label()),
            WipeStatus::Incomplete => {
                println!("INCOMPLETE: old data may remain. Destroy this stick if it held sensitive data.")
            }
        }
        print_ranges("Write failures", &r.write_failures);
        print_ranges("Verification failures", &r.verify_failures);
        println!("  Duration: {}", duration(r.duration_secs));
    });
    Ok(r.status != WipeStatus::Incomplete)
}

fn cmd_reset(cli: &Cli, device: &str, opts: &ResetOptions, safety: &DestructiveArgs) -> Result<bool> {
    let t = target(cli, device)?;
    let overrides = confirm(&t, "restore the factory state", safety)?;
    let r = ops::factory_reset(&t, opts, overrides, &mut context(cli))?;
    end_progress(cli);
    emit(cli, "factory-reset", &t, &r, || {
        if r.success() {
            println!("DONE: the device was reset.");
        } else {
            println!("PROBLEMS during the reset:");
        }
        for p in &r.problems {
            println!("  - {p}");
        }
        if let Some(f) = &r.format {
            println!("  Filesystem: {} ({} clusters of {})", f.filesystem, f.clusters, bytes(f.cluster_size as u64));
            if !f.label.is_empty() {
                println!("  Label:      {}", f.label);
            }
        }
        println!(
            "  Layout:     {}, partition at {}, {}",
            r.table.label(),
            bytes(r.partition_start),
            bytes(r.partition_size)
        );
        println!(
            "  Old data:   {}",
            if r.data_erased { "overwritten and verified" } else { "NOT erased, still recoverable" }
        );
        println!("  Duration:   {}", duration(r.duration_secs));
    });
    Ok(r.success())
}

fn cmd_backup(cli: &Cli, device: &str, file: &Path) -> Result<bool> {
    let t = target(cli, device)?;
    let r = ops::backup(&t, file, &mut context(cli))?;
    end_progress(cli);
    emit(cli, "backup", &t, &r, || {
        println!("Saved {} to {}", bytes(r.bytes), r.path);
        println!("  SHA-256: {}", r.sha256);
        print_ranges("Unreadable (zero-filled)", &r.unreadable);
        if r.aborted {
            println!("  The device stopped responding; the image is incomplete.");
        }
    });
    Ok(r.unreadable.is_empty() && !r.aborted)
}

fn cmd_restore(cli: &Cli, file: &Path, device: &str, safety: &DestructiveArgs) -> Result<bool> {
    let t = target(cli, device)?;
    let overrides = confirm(&t, &format!("write {}", file.display()), safety)?;
    let r = ops::restore(&t, file, overrides, &mut context(cli))?;
    end_progress(cli);
    emit(cli, "restore", &t, &r, || {
        if r.verify_failures.is_empty() {
            println!("Restored and verified {}.", bytes(r.bytes));
        } else {
            println!("Restored {}, but verification FAILED.", bytes(r.bytes));
        }
        println!("  SHA-256: {}", r.sha256);
        print_ranges("Verification failures", &r.verify_failures);
    });
    Ok(r.verify_failures.is_empty())
}
