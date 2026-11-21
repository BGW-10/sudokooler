pub mod protocol;

use anyhow::{Context, Result, anyhow};
use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::config::{DeviceProfile, DeviceRegistry, SUDOKOO_VID};
use crate::sensors::Telemetry;

use self::protocol::{decode_hex_packet, encode_telemetry};

/// Metadata about a discovered Sudokoo (`0x381C`) HID interface for diagnostics and `--list-devices`.
#[derive(Debug, Clone)]
pub struct DetectedDevice {
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub interface_number: i32,
    pub manufacturer: String,
    pub product: String,
    pub path: String,
    pub matched_profile: Option<String>,
    pub enabled: bool,
}

/// Active connection to an allowlisted Sudokoo HID cooler.
///
/// Go note: Rust has no garbage collector or `defer`; when `CoolerDevice` is dropped
/// (e.g., on USB disconnect or shutdown), `hidapi::HidDevice` closes its OS handle via RAII.
pub struct CoolerDevice {
    device: HidDevice,
    profile: DeviceProfile,
}

impl CoolerDevice {
    /// Enumerates USB HID interfaces, filters strictly to `VID == 0x381C` and an
    /// `enabled = true` profile in `registry`, opens the interface, and sends `init_packets`.
    pub fn open_first(registry: &DeviceRegistry) -> Result<Option<Self>> {
        let api = HidApi::new().context("failed to initialize hidapi")?;

        // Collect all matching enabled candidates, prioritizing vendor-defined usage pages
        // (0x00FF / >= 0xFF00) if a composite USB device exposes multiple HID collections.
        let mut candidates: Vec<(&DeviceInfo, &DeviceProfile)> = api
            .device_list()
            .filter(|dev| dev.vendor_id() == SUDOKOO_VID)
            .filter_map(|dev| {
                let profile = registry.find_enabled_by_pid(dev.product_id())?;
                if !matches_interface_filters(dev, profile) {
                    return None;
                }
                Some((dev, profile))
            })
            .collect();

        candidates.sort_by_key(|(dev, _)| {
            if dev.usage_page() == 0x00FF || dev.usage_page() >= 0xFF00 {
                0
            } else {
                1
            }
        });

        let Some((info, profile)) = candidates.into_iter().next() else {
            return Ok(None);
        };

        // Re-assert safety invariant before opening any HID path.
        if info.vendor_id() != SUDOKOO_VID || !profile.enabled {
            return Err(anyhow!(
                "refusing to open non-allowlisted device {:04X}:{:04X}",
                info.vendor_id(),
                info.product_id()
            ));
        }

        let device = api
            .open_path(info.path())
            .with_context(|| format!("failed to open HID path for {}", profile.name))?;

        let cooler = Self {
            device,
            profile: profile.clone(),
        };
        cooler.send_init()?;
        Ok(Some(cooler))
    }

    pub fn profile(&self) -> &DeviceProfile {
        &self.profile
    }

    pub fn send_init(&self) -> Result<()> {
        for hex in &self.profile.init_packets {
            let report = decode_hex_packet(hex, self.profile.report_length)?;
            self.write_report(&report)
                .context("failed to send init packet")?;
        }
        Ok(())
    }

    pub fn send_telemetry(&self, telemetry: &Telemetry) -> Result<()> {
        let report = encode_telemetry(telemetry, &self.profile)?;
        self.write_report(&report)
            .context("failed to send telemetry report")
    }

    /// Best-effort display turn-off on clean shutdown (ignores write errors if already unplugged).
    pub fn send_shutdown(&self) {
        for hex in &self.profile.shutdown_packets {
            if let Ok(report) = decode_hex_packet(hex, self.profile.report_length) {
                let _ = self.write_report(&report);
            }
        }
    }

    fn write_report(&self, report: &[u8]) -> Result<()> {
        let written = self
            .device
            .write(report)
            .context("hidapi write error (device disconnected or busy)")?;
        if written == 0 {
            return Err(anyhow!("hidapi wrote 0 bytes"));
        }
        Ok(())
    }
}

/// Lists every connected HID interface matching `VID == 0x381C` without opening or writing to them.
pub fn list_sudokoo_devices(registry: &DeviceRegistry) -> Result<Vec<DetectedDevice>> {
    let api = HidApi::new().context("failed to initialize hidapi")?;
    let mut devices = Vec::new();

    for info in api.device_list() {
        if info.vendor_id() != SUDOKOO_VID {
            continue;
        }

        let profile = registry.find_by_pid(info.product_id());
        devices.push(DetectedDevice {
            vid: info.vendor_id(),
            pid: info.product_id(),
            usage_page: info.usage_page(),
            usage: info.usage(),
            interface_number: info.interface_number(),
            manufacturer: info.manufacturer_string().unwrap_or("").to_string(),
            product: info.product_string().unwrap_or("").to_string(),
            path: info.path().to_string_lossy().into_owned(),
            matched_profile: profile.map(|p| p.name.clone()),
            enabled: profile.is_some_and(|p| p.enabled),
        });
    }

    Ok(devices)
}

fn matches_interface_filters(info: &DeviceInfo, profile: &DeviceProfile) -> bool {
    if let Some(expected_page) = profile.usage_page
        && info.usage_page() != expected_page
    {
        return false;
    }
    if let Some(expected_iface) = profile.interface_number
        && info.interface_number() != expected_iface
    {
        return false;
    }
    true
}
