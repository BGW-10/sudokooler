//! TOML device profile registry and USB safety validation.
//!
//! Embeds `profiles/default_devices.toml` via `include_str!` and supports optional
//! runtime overrides via `--config`. Enforces strict `VID == 0x381C` allowlisting,
//! valid control packet framing, and unique enabled PIDs.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::hid::protocol::decode_hex_packet;

/// Sudokoo USB Vendor ID (`0x381C`). All devices opened by `sudokooler` must match this VID.
pub const SUDOKOO_VID: u16 = 0x381C;

/// Built-in default device profiles embedded directly into the binary so `sudokooler`
/// works standalone without needing external config files on disk.
pub const DEFAULT_DEVICES_TOML: &str = include_str!("../profiles/default_devices.toml");

/// Structured errors returned when loading or validating a device registry.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Failed to read a custom TOML file from disk.
    #[error("failed to read config file '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// TOML syntax or field deserialization failed.
    #[error("failed to parse TOML device registry: {0}")]
    Parse(#[from] toml::de::Error),
    /// A profile violated hardware safety or protocol constraints.
    #[error("invalid profile '{name}': {reason}")]
    InvalidProfile { name: String, reason: String },
    /// Two or more enabled profiles claimed the same USB Product ID.
    #[error("duplicate enabled device profile for PID 0x{pid:04X}")]
    DuplicateEnabledPid { pid: u16 },
}

/// Wire protocol family spoken by a cooler profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolKind {
    /// Sudokoo SK700V 64-byte HID protocol (`0x10 0x68 0x01 0x09 ... 0x16`).
    Sk700v,
}

/// Temperature display unit sent to the cooler's firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TempUnit {
    /// Degrees Celsius (`0x00` wire flag).
    #[default]
    C,
    /// Degrees Fahrenheit (`0x01` wire flag).
    F,
}

/// Declarative USB HID and protocol configuration for a single Sudokoo cooler model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceProfile {
    /// Human-readable product name (e.g., `"Sudokoo SK700V"`).
    pub name: String,
    /// USB Vendor ID; validated to equal [`SUDOKOO_VID`] (`0x381C`).
    pub vid: u16,
    /// USB Product ID (e.g., `0x0003` for SK700V).
    pub pid: u16,
    /// Wire protocol encoder variant.
    pub protocol: ProtocolKind,
    /// Safety switch: only `enabled = true` profiles may be opened or written over USB.
    #[serde(default)]
    pub enabled: bool,
    /// Indicates whether the profile has been verified against physical hardware captures.
    #[serde(default)]
    pub verified: bool,
    /// Leading HID Report ID byte (typically `0x10`).
    #[serde(default = "default_report_id")]
    pub report_id: u8,
    /// Fixed padded HID output report size in bytes (typically `64`).
    #[serde(default = "default_report_length")]
    pub report_length: usize,
    /// Reference wattage corresponding to 100% on the cooler's power bar.
    #[serde(default = "default_power_bar_max_w")]
    pub power_bar_max_w: u16,
    /// Display temperature unit (`"C"` or `"F"`).
    #[serde(default)]
    pub temp_unit: TempUnit,
    /// Optional HID usage page filter for multi-collection USB devices.
    #[serde(default)]
    pub usage_page: Option<u16>,
    /// Optional USB interface number filter.
    #[serde(default)]
    pub interface_number: Option<i32>,
    /// Hex-encoded control packets transmitted immediately after opening the device.
    #[serde(default)]
    pub init_packets: Vec<String>,
    /// Hex-encoded control packets transmitted best-effort during clean shutdown.
    #[serde(default)]
    pub shutdown_packets: Vec<String>,
}

/// Root TOML document containing one or more [`DeviceProfile`] entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRegistry {
    pub devices: Vec<DeviceProfile>,
}

fn default_report_id() -> u8 {
    0x10
}

fn default_report_length() -> usize {
    64
}

fn default_power_bar_max_w() -> u16 {
    160
}

/// Loads either an explicit `--config` TOML file or the embedded `default_devices.toml`,
/// then validates every profile against our USB safety invariants.
///
/// Go note: `Result<T, E>` replaces Go's `(T, error)` pair, and `?` propagates errors
/// upward cleanly without manual `if err != nil` boilerplate.
pub fn load_registry(path: Option<&Path>) -> Result<DeviceRegistry, ConfigError> {
    let raw = match path {
        Some(p) => fs::read_to_string(p).map_err(|source| ConfigError::Io {
            path: p.display().to_string(),
            source,
        })?,
        None => DEFAULT_DEVICES_TOML.to_string(),
    };

    let registry: DeviceRegistry = toml::from_str(&raw)?;
    registry.validate()?;
    Ok(registry)
}

impl DeviceRegistry {
    /// Enforces hardware safety and wire-framing invariants across every configured profile.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut enabled_pids = HashSet::new();

        for profile in &self.devices {
            if profile.name.trim().is_empty() {
                return Err(ConfigError::InvalidProfile {
                    name: "<unnamed>".to_string(),
                    reason: "profile name must not be empty".to_string(),
                });
            }

            if profile.vid != SUDOKOO_VID {
                return Err(ConfigError::InvalidProfile {
                    name: profile.name.clone(),
                    reason: format!(
                        "VID 0x{:04X} does not match Sudokoo VID 0x{:04X}",
                        profile.vid, SUDOKOO_VID
                    ),
                });
            }

            if profile.power_bar_max_w == 0 {
                return Err(ConfigError::InvalidProfile {
                    name: profile.name.clone(),
                    reason: "power_bar_max_w must be greater than 0".to_string(),
                });
            }

            match profile.protocol {
                ProtocolKind::Sk700v => {
                    if !(20..=64).contains(&profile.report_length) {
                        return Err(ConfigError::InvalidProfile {
                            name: profile.name.clone(),
                            reason: format!(
                                "sk700v report_length {} must be in 20..=64",
                                profile.report_length
                            ),
                        });
                    }
                }
            }

            for pkt_hex in profile
                .init_packets
                .iter()
                .chain(profile.shutdown_packets.iter())
            {
                let decoded = decode_hex_packet(pkt_hex, profile.report_length).map_err(|e| {
                    ConfigError::InvalidProfile {
                        name: profile.name.clone(),
                        reason: format!("invalid control packet '{pkt_hex}': {e}"),
                    }
                })?;

                if decoded[0] != profile.report_id {
                    return Err(ConfigError::InvalidProfile {
                        name: profile.name.clone(),
                        reason: format!(
                            "control packet '{pkt_hex}' starts with 0x{:02X}, expected report_id 0x{:02X}",
                            decoded[0], profile.report_id
                        ),
                    });
                }
            }

            if profile.enabled && !enabled_pids.insert(profile.pid) {
                return Err(ConfigError::DuplicateEnabledPid { pid: profile.pid });
            }
        }

        Ok(())
    }

    /// Returns the enabled device profile matching `pid`, if any.
    pub fn find_enabled_by_pid(&self, pid: u16) -> Option<&DeviceProfile> {
        self.devices.iter().find(|d| d.enabled && d.pid == pid)
    }

    /// Returns any configured profile (enabled or disabled) matching `pid` for diagnostics.
    pub fn find_by_pid(&self, pid: u16) -> Option<&DeviceProfile> {
        self.devices.iter().find(|d| d.pid == pid)
    }

    /// Returns the first enabled device profile (useful for `--dry-run` without attached hardware).
    pub fn first_enabled(&self) -> Option<&DeviceProfile> {
        self.devices.iter().find(|d| d.enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_default_registry_is_valid() {
        let registry = load_registry(None).expect("embedded default_devices.toml must be valid");
        let sk700v = registry
            .find_enabled_by_pid(0x0003)
            .expect("SK700V (0x0003) must be enabled in default registry");
        assert_eq!(sk700v.vid, SUDOKOO_VID);
        assert!(sk700v.verified);
        assert_eq!(sk700v.report_length, 64);

        // Unverified PIDs must remain disabled by default.
        for unverified_pid in [0x0001u16, 0x0002, 0x0004, 0x0005] {
            assert!(
                registry.find_enabled_by_pid(unverified_pid).is_none(),
                "unverified PID 0x{unverified_pid:04X} must not be enabled by default"
            );
            assert!(
                registry.find_by_pid(unverified_pid).is_some(),
                "unverified PID 0x{unverified_pid:04X} should be documented in default registry"
            );
        }
    }

    #[test]
    fn rejects_non_sudokoo_vid() {
        let bad = r#"
[[devices]]
name = "Wrong Vendor"
vid = 0x3633
pid = 0x0003
protocol = "sk700v"
enabled = true
"#;
        let parsed: DeviceRegistry = toml::from_str(bad).unwrap();
        let err = parsed.validate().unwrap_err();
        assert!(err.to_string().contains("0x381C"));
    }
}
