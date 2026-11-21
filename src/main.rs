//! CLI entrypoint and 1 Hz display-polling daemon for Sudokoo USB HID CPU coolers.
//!
//! Coordinates configuration loading ([`config`]), platform CPU telemetry
//! ([`sensors`]), packet encoding ([`hid::protocol`]), and safe reconnect-aware USB
//! HID transport ([`hid`]).

mod config;
mod hid;
mod sensors;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use crate::config::{DeviceRegistry, SUDOKOO_VID, load_registry};
use crate::hid::protocol::{encode_telemetry, format_hex};
use crate::hid::{CoolerDevice, list_sudokoo_devices};
use crate::sensors::default_provider;

#[derive(Debug, Parser)]
#[command(
    name = "sudokooler",
    version,
    about = "Lightweight daemon to drive digital segmented displays on Sudokoo CPU coolers"
)]
struct Cli {
    /// Enumerate detected Sudokoo (VID 0x381C) USB HID interfaces and exit.
    #[arg(long)]
    list_devices: bool,

    /// Sample live sensors and print encoded HID packets to stdout without writing to USB.
    #[arg(long)]
    dry_run: bool,

    /// Path to an override TOML device registry (defaults to built-in profiles).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Polling interval in milliseconds.
    #[arg(long, default_value_t = 1000)]
    interval_ms: u64,

    /// Optional sample count before exiting cleanly (0 = run until Ctrl+C).
    #[arg(long, default_value_t = 0)]
    count: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let registry = load_registry(cli.config.as_deref())
        .context("failed to load Sudokoo device registry")?;

    if cli.list_devices {
        return cmd_list_devices(&registry);
    }

    // Go note: `Arc<AtomicBool>` is an atomic reference-counted flag shared safely
    // between the main loop and the OS signal callback thread without a mutex.
    let running = Arc::new(AtomicBool::new(true));
    let signal_flag = Arc::clone(&running);
    ctrlc::set_handler(move || {
        signal_flag.store(false, Ordering::SeqCst);
    })
    .context("failed to install Ctrl+C signal handler")?;

    let interval = Duration::from_millis(cli.interval_ms.max(100));

    if cli.dry_run {
        return cmd_dry_run(&registry, interval, cli.count, &running);
    }

    cmd_run_daemon(&registry, interval, cli.count, &running)
}

/// Prints every connected Sudokoo (`VID == 0x381C`) HID interface and its matching profile status.
fn cmd_list_devices(registry: &DeviceRegistry) -> Result<()> {
    let devices = list_sudokoo_devices(registry)?;
    if devices.is_empty() {
        println!(
            "No Sudokoo USB HID interfaces detected (VID 0x{SUDOKOO_VID:04X})."
        );
        return Ok(());
    }

    println!(
        "Detected {} Sudokoo HID interface(s) (VID 0x{SUDOKOO_VID:04X}):",
        devices.len()
    );
    for (idx, dev) in devices.iter().enumerate() {
        let profile_status = match (&dev.matched_profile, dev.enabled) {
            (Some(name), true) => format!("matched: {name} [enabled]"),
            (Some(name), false) => format!("matched: {name} [disabled / unverified]"),
            (None, _) => "unrecognized PID".to_string(),
        };
        let mfg = if dev.manufacturer.is_empty() {
            String::new()
        } else {
            format!(" mfg='{}'", dev.manufacturer)
        };
        println!(
            "  [{idx}] {:04X}:{:04X} iface={} usage_page=0x{:04X} usage=0x{:04X}{} product='{}' ({profile_status})\n       path={}",
            dev.vid,
            dev.pid,
            dev.interface_number,
            dev.usage_page,
            dev.usage,
            mfg,
            dev.product,
            dev.path,
        );
    }
    Ok(())
}

/// Polls live sensors and prints encoded 64-byte HID packets to stdout without touching USB.
fn cmd_dry_run(
    registry: &DeviceRegistry,
    interval: Duration,
    max_count: usize,
    running: &AtomicBool,
) -> Result<()> {
    let profile = registry
        .first_enabled()
        .context("no enabled device profiles found in registry")?;
    let mut sensors = default_provider()?;
    println!(
        "[dry-run] sensor backend: {} | target profile: {} ({:04X}:{:04X})",
        sensors.backend_name(),
        profile.name,
        profile.vid,
        profile.pid
    );

    let mut iterations = 0usize;
    while running.load(Ordering::SeqCst) {
        let telemetry = sensors.read()?;
        let packet = encode_telemetry(&telemetry, profile)?;
        println!(
            "[dry-run] temp={:.1}C load={}% power={}W freq={}MHz | {}",
            telemetry.cpu_temp_c,
            telemetry.cpu_usage_pct,
            telemetry.cpu_power_w,
            telemetry.cpu_freq_mhz,
            format_hex(&packet)
        );

        iterations += 1;
        if max_count > 0 && iterations >= max_count {
            break;
        }
        sleep_interruptible(interval, running);
    }

    Ok(())
}

/// Connects to an allowlisted Sudokoo device and streams telemetry at `interval` until stopped.
///
/// Any USB I/O failure drops `active_device` (closing the stale OS handle via RAII) and
/// retries after a 2-second backoff instead of panicking.
fn cmd_run_daemon(
    registry: &DeviceRegistry,
    interval: Duration,
    max_count: usize,
    running: &AtomicBool,
) -> Result<()> {
    let mut sensors = default_provider()?;
    println!("[daemon] sensor backend: {}", sensors.backend_name());

    let mut active_device: Option<CoolerDevice> = None;
    let mut waiting_logged = false;
    let mut iterations = 0usize;

    while running.load(Ordering::SeqCst) {
        if active_device.is_none() {
            match CoolerDevice::open_first(registry) {
                Ok(Some(dev)) => {
                    println!(
                        "[daemon] connected to {} ({:04X}:{:04X})",
                        dev.profile().name,
                        dev.profile().vid,
                        dev.profile().pid
                    );
                    active_device = Some(dev);
                    waiting_logged = false;
                }
                Ok(None) => {
                    if !waiting_logged {
                        eprintln!(
                            "[daemon] waiting for an enabled Sudokoo device (VID 0x{SUDOKOO_VID:04X})..."
                        );
                        waiting_logged = true;
                    }
                    sleep_interruptible(Duration::from_secs(2), running);
                    continue;
                }
                Err(err) => {
                    eprintln!("[daemon] failed to open Sudokoo device: {err:#}; retrying in 2s");
                    sleep_interruptible(Duration::from_secs(2), running);
                    continue;
                }
            }
        }

        let telemetry = match sensors.read() {
            Ok(t) => t,
            Err(err) => {
                eprintln!("[daemon] sensor read error: {err:#}");
                sleep_interruptible(interval, running);
                continue;
            }
        };

        if let Some(dev) = active_device.as_ref()
            && let Err(err) = dev.send_telemetry(&telemetry)
        {
            eprintln!("[daemon] USB write error ({err:#}); dropping handle and reconnecting...");
            active_device = None;
            sleep_interruptible(Duration::from_secs(2), running);
            continue;
        }

        iterations += 1;
        if max_count > 0 && iterations >= max_count {
            break;
        }
        sleep_interruptible(interval, running);
    }

    // `Option::take` transfers ownership out so `send_shutdown` runs once and the
    // `CoolerDevice` handle closes immediately at the end of this block.
    if let Some(dev) = active_device.take() {
        println!("[daemon] shutting down display cleanly...");
        dev.send_shutdown();
    }

    Ok(())
}

/// Sleeps in 100 ms chunks so `Ctrl+C` exits promptly rather than waiting a full polling interval.
fn sleep_interruptible(duration: Duration, running: &AtomicBool) {
    let step = Duration::from_millis(100);
    let mut remaining = duration;
    while remaining > Duration::ZERO && running.load(Ordering::SeqCst) {
        let chunk = remaining.min(step);
        std::thread::sleep(chunk);
        remaining = remaining.saturating_sub(chunk);
    }
}
