use thiserror::Error;

use crate::config::{DeviceProfile, ProtocolKind, TempUnit};
use crate::sensors::Telemetry;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("hex packet must not be empty")]
    EmptyHex,
    #[error("hex packet length ({0}) must be even")]
    OddHexLength(usize),
    #[error("invalid hex byte '{0}'")]
    InvalidHexByte(String),
    #[error("packet length {actual} exceeds report_length {max}")]
    PacketTooLong { actual: usize, max: usize },
    #[error("invalid report_length {0} for sk700v (expected 20..=64)")]
    InvalidReportLength(usize),
}

/// Computes the 8-bit checksum over bytes `1..18` (excluding byte `0`, the HID Report ID).
pub fn compute_checksum(frame: &[u8]) -> u8 {
    frame
        .get(1..18)
        .unwrap_or_default()
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b))
}

/// Zero-pads a slice up to `report_length`.
pub fn pad_report(bytes: &[u8], report_length: usize) -> Result<Vec<u8>, ProtocolError> {
    if bytes.len() > report_length {
        return Err(ProtocolError::PacketTooLong {
            actual: bytes.len(),
            max: report_length,
        });
    }
    let mut padded = vec![0u8; report_length];
    padded[..bytes.len()].copy_from_slice(bytes);
    Ok(padded)
}

/// Parses a hex string (whitespace allowed) and zero-pads it to `report_length`.
pub fn decode_hex_packet(hex: &str, report_length: usize) -> Result<Vec<u8>, ProtocolError> {
    let clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.is_empty() {
        return Err(ProtocolError::EmptyHex);
    }
    if clean.len() % 2 != 0 {
        return Err(ProtocolError::OddHexLength(clean.len()));
    }

    let mut raw = Vec::with_capacity(clean.len() / 2);
    for chunk in clean.as_bytes().chunks_exact(2) {
        let pair = std::str::from_utf8(chunk)
            .map_err(|_| ProtocolError::InvalidHexByte(String::from_utf8_lossy(chunk).into()))?;
        let byte = u8::from_str_radix(pair, 16)
            .map_err(|_| ProtocolError::InvalidHexByte(pair.to_string()))?;
        raw.push(byte);
    }

    pad_report(&raw, report_length)
}

/// Encodes a `Telemetry` sample into the exact 64-byte HID report used by the Sudokoo SK700V.
///
/// Layout (verified against SK700V USB HID captures):
/// - `[0]`: HID Report ID (`0x10`)
/// - `[1..=3]`: Header (`0x68, 0x01, 0x09`)
/// - `[4]`: Payload byte count (`0x0D` = 13 bytes covering `[5..=17]`)
/// - `[5]`: Telemetry command (`0x01`)
/// - `[6]`: Display state (`0x02` = active/on; `0x00` = instant turn-off)
/// - `[7..=8]`: CPU package power in watts, big-endian `u16` (clamped `0..=999`)
/// - `[9]`: Power bar percentage `u8` (`0..=100`, scaled against `profile.power_bar_max_w`)
/// - `[10]`: Temperature unit flag (`0x00` = Celsius, `0x01` = Fahrenheit)
/// - `[11..=14]`: Temperature as IEEE-754 big-endian `f32`
/// - `[15]`: CPU usage percentage `u8` (`0..=100`)
/// - `[16..=17]`: CPU frequency in MHz, big-endian `u16`
/// - `[18]`: Checksum (`sum(pkt[1..18]) & 0xFF`)
/// - `[19]`: Terminator (`0x16`)
/// - `[20..=63]`: Zero padding (`0x00`)
pub fn encode_sk700v_packet(telemetry: &Telemetry, profile: &DeviceProfile) -> [u8; 64] {
    let mut pkt = [0u8; 64];

    pkt[0] = profile.report_id;
    pkt[1] = 0x68;
    pkt[2] = 0x01;
    pkt[3] = 0x09;
    pkt[4] = 0x0D;
    pkt[5] = 0x01;
    pkt[6] = 0x02;

    let power_w = telemetry.cpu_power_w.min(999);
    pkt[7..9].copy_from_slice(&power_w.to_be_bytes());

    let max_w = u32::from(profile.power_bar_max_w.max(1));
    let power_bar_pct = ((u32::from(power_w) * 100) / max_w).min(100) as u8;
    pkt[9] = power_bar_pct;

    let temp_c = if telemetry.cpu_temp_c.is_finite() {
        telemetry.cpu_temp_c
    } else {
        0.0
    };
    let (unit_flag, temp_val) = match profile.temp_unit {
        TempUnit::C => (0x00u8, temp_c),
        TempUnit::F => (0x01u8, temp_c * 9.0 / 5.0 + 32.0),
    };
    pkt[10] = unit_flag;
    pkt[11..15].copy_from_slice(&temp_val.to_be_bytes());

    pkt[15] = telemetry.cpu_usage_pct.min(100);
    pkt[16..18].copy_from_slice(&telemetry.cpu_freq_mhz.to_be_bytes());

    pkt[18] = compute_checksum(&pkt);
    pkt[19] = 0x16;

    pkt
}

/// Encodes `telemetry` according to `profile.protocol` and sizes it to `profile.report_length`.
pub fn encode_telemetry(
    telemetry: &Telemetry,
    profile: &DeviceProfile,
) -> Result<Vec<u8>, ProtocolError> {
    match profile.protocol {
        ProtocolKind::Sk700v => {
            if !(20..=64).contains(&profile.report_length) {
                return Err(ProtocolError::InvalidReportLength(profile.report_length));
            }
            let frame = encode_sk700v_packet(telemetry, profile);
            Ok(frame[..profile.report_length].to_vec())
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SUDOKOO_VID;

    fn sk700v_profile(power_bar_max_w: u16, temp_unit: TempUnit) -> DeviceProfile {
        DeviceProfile {
            name: "Sudokoo SK700V".to_string(),
            vid: SUDOKOO_VID,
            pid: 0x0003,
            protocol: ProtocolKind::Sk700v,
            enabled: true,
            verified: true,
            report_id: 0x10,
            report_length: 64,
            power_bar_max_w,
            temp_unit,
            usage_page: None,
            interface_number: None,
            init_packets: vec!["106801090203017816".to_string()],
            shutdown_packets: vec!["106801090203007716".to_string()],
        }
    }

    #[test]
    fn matches_captured_sk700v_packets_byte_for_byte() {
        // MasterCraft USB HID captures were recorded on a Ryzen 9800X3D
        // using floor(power_w * 100 / 156) for the power bar byte.
        let profile = sk700v_profile(156, TempUnit::C);

        let samples = [
            (
                Telemetry {
                    cpu_power_w: 40,
                    cpu_temp_c: 47.0,
                    cpu_usage_pct: 16,
                    cpu_freq_mhz: 5025,
                },
                "106801090d010200281900423c00001013a10516",
            ),
            (
                Telemetry {
                    cpu_power_w: 51,
                    cpu_temp_c: 48.0,
                    cpu_usage_pct: 20,
                    cpu_freq_mhz: 5025,
                },
                "106801090d010200332000424000001413a11f16",
            ),
            (
                Telemetry {
                    cpu_power_w: 50,
                    cpu_temp_c: 48.0,
                    cpu_usage_pct: 20,
                    cpu_freq_mhz: 5025,
                },
                "106801090d010200322000424000001413a11e16",
            ),
            (
                Telemetry {
                    cpu_power_w: 52,
                    cpu_temp_c: 49.0,
                    cpu_usage_pct: 19,
                    cpu_freq_mhz: 5225,
                },
                "106801090d01020034210042440000131469ed16",
            ),
            (
                Telemetry {
                    cpu_power_w: 55,
                    cpu_temp_c: 49.0,
                    cpu_usage_pct: 22,
                    cpu_freq_mhz: 5225,
                },
                "106801090d01020037230042440000161469f516",
            ),
            (
                Telemetry {
                    cpu_power_w: 67,
                    cpu_temp_c: 53.0,
                    cpu_usage_pct: 80,
                    cpu_freq_mhz: 5005,
                },
                "106801090d010200432a004254000050138d7516",
            ),
            (
                Telemetry {
                    cpu_power_w: 78,
                    cpu_temp_c: 56.0,
                    cpu_usage_pct: 97,
                    cpu_freq_mhz: 4960,
                },
                "106801090d0102004e3200426000006113607816",
            ),
            (
                Telemetry {
                    cpu_power_w: 77,
                    cpu_temp_c: 65.0,
                    cpu_usage_pct: 89,
                    cpu_freq_mhz: 4935,
                },
                "106801090d0102004d3100428200005913477716",
            ),
        ];

        for (telemetry, expected_short_hex) in samples {
            let expected = decode_hex_packet(expected_short_hex, 64).unwrap();
            let actual = encode_telemetry(&telemetry, &profile).unwrap();
            assert_eq!(actual.len(), 64);
            assert_eq!(
                format_hex(&actual),
                format_hex(&expected),
                "mismatch for {telemetry:?}"
            );
        }
    }

    #[test]
    fn matches_max_values_packet() {
        // Boundary frame verification: 999W (0x03E7), 100% power bar, 99.0C (BE f32),
        // 100% load, 9995 MHz (0x270B).
        let profile = sk700v_profile(160, TempUnit::C);
        let telemetry = Telemetry {
            cpu_power_w: 999,
            cpu_temp_c: 99.0,
            cpu_usage_pct: 100,
            cpu_freq_mhz: 9995,
        };
        let actual = encode_telemetry(&telemetry, &profile).unwrap();

        // Sum of bytes [1..18]:
        // 0x68 + 0x01 + 0x09 + 0x0d + 0x01 + 0x02 + 0x03 + 0xe7 + 0x64 + 0x00
        // + 0x42 + 0xc6 + 0x00 + 0x00 + 0x64 + 0x27 + 0x0b = 0x36E -> 0x6E
        let expected =
            decode_hex_packet("106801090d010203e7640042c6000064270b6e16", 64).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn encodes_fahrenheit_flag_and_conversion() {
        let profile = sk700v_profile(160, TempUnit::F);
        let telemetry = Telemetry {
            cpu_power_w: 0,
            cpu_temp_c: 25.0, // 77.0 F -> 0x429a0000
            cpu_usage_pct: 5,
            cpu_freq_mhz: 3600,
        };
        let pkt = encode_sk700v_packet(&telemetry, &profile);
        assert_eq!(pkt[10], 0x01);
        assert_eq!(&pkt[11..15], &77.0f32.to_be_bytes());
        assert_eq!(pkt[18], compute_checksum(&pkt));
        assert_eq!(pkt[19], 0x16);
    }

    #[test]
    fn init_and_shutdown_packets_have_valid_checksums_and_padding() {
        let init = decode_hex_packet("106801090203017816", 64).unwrap();
        let off = decode_hex_packet("106801090203007716", 64).unwrap();

        assert_eq!(init.len(), 64);
        assert_eq!(off.len(), 64);

        let init_csum = init[1..7].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        let off_csum = off[1..7].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        assert_eq!(init_csum, init[7]);
        assert_eq!(off_csum, off[7]);
        assert!(init[9..].iter().all(|&b| b == 0));
        assert!(off[9..].iter().all(|&b| b == 0));
    }
}
