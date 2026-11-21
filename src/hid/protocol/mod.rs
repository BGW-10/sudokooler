//! Display protocol abstraction, factory, and shared USB HID framing helpers.
//!
//! Each supported Sudokoo cooler protocol implements [`DisplayProtocol`] in its own
//! submodule (starting with [`sk700v`]) and is selected via [`for_protocol`].

pub mod sk700v;

use anyhow::{Context, Result, bail};

use crate::config::{DeviceProfile, ProtocolKind};
use crate::sensors::Telemetry;

/// Wire-protocol encoder and validator interface for Sudokoo display hardware.
///
/// Go note: Similar to a Go interface, each protocol struct explicitly declares
/// `impl DisplayProtocol for MyProtocol`. Returning `Vec<Vec<u8>>` lets a single
/// interface support both single-report segmented displays (`SK700V`) and future
/// multi-report image/frame transfers (e.g., LCD AIOs).
pub trait DisplayProtocol {
    /// Human-readable protocol identifier (matches TOML `protocol` name).
    fn name(&self) -> &'static str;

    /// Validates protocol-specific fields (e.g., `report_length`) on a [`DeviceProfile`].
    fn validate_profile(&self, profile: &DeviceProfile) -> Result<()>;

    /// Encodes a [`Telemetry`] sample into one or more zero-padded HID Output reports.
    fn encode_telemetry(
        &self,
        telemetry: &Telemetry,
        profile: &DeviceProfile,
    ) -> Result<Vec<Vec<u8>>>;
}

/// Factory returning the [`DisplayProtocol`] implementation for `kind`.
pub fn for_protocol(kind: ProtocolKind) -> Box<dyn DisplayProtocol> {
    match kind {
        ProtocolKind::Sk700v => Box::new(sk700v::Sk700vProtocol),
    }
}

/// Zero-pads a slice up to `report_length`.
pub fn pad_report(bytes: &[u8], report_length: usize) -> Result<Vec<u8>> {
    if bytes.len() > report_length {
        bail!(
            "packet length {} exceeds report_length {}",
            bytes.len(),
            report_length
        );
    }
    let mut padded = vec![0u8; report_length];
    padded[..bytes.len()].copy_from_slice(bytes);
    Ok(padded)
}

/// Parses a hex string (whitespace allowed) and zero-pads it to `report_length`.
pub fn decode_hex_packet(hex: &str, report_length: usize) -> Result<Vec<u8>> {
    let clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.is_empty() {
        bail!("hex packet must not be empty");
    }
    if clean.len() % 2 != 0 {
        bail!("hex packet length ({}) must be even", clean.len());
    }

    let mut raw = Vec::with_capacity(clean.len() / 2);
    for chunk in clean.as_bytes().chunks_exact(2) {
        let pair = std::str::from_utf8(chunk).with_context(|| {
            format!(
                "invalid UTF-8 in hex chunk '{}'",
                String::from_utf8_lossy(chunk)
            )
        })?;
        let byte =
            u8::from_str_radix(pair, 16).with_context(|| format!("invalid hex byte '{pair}'"))?;
        raw.push(byte);
    }

    pad_report(&raw, report_length)
}

/// Formats a byte slice as lowercase contiguous hex.
pub fn format_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}
