//! Sudokoo SK700V 64-byte USB HID wire protocol (`VID = 0x381C`, `PID = 0x0003`).
//!
//! All SK700V HID reports are zero-padded to **64 bytes** starting with Report ID `0x10`:
//!
//! ```text
//! Control (Display ON/OFF, 9 bytes + 55 zero bytes):
//!   [0]=0x10  [1..=3]=68 01 09  [4]=0x02  [5]=0x03  [6]=state(01|00)  [7]=csum  [8]=0x16
//!
//! Telemetry (20 bytes + 44 zero bytes):
//!   [0]=0x10  [1..=3]=68 01 09  [4]=0x0D  [5]=0x01  [6]=0x02(active)
//!   [7..=8]=power_w (BE u16)  [9]=power_bar_pct (u8)  [10]=unit(00=C|01=F)
//!   [11..=14]=temp (BE IEEE-754 f32)  [15]=usage_pct (u8)  [16..=17]=freq_mhz (BE u16)
//!   [18]=sum(pkt[1..18]) & 0xFF  [19]=0x16  [20..=63]=0x00
//! ```

use anyhow::{Result, bail};

use crate::config::{DeviceProfile, TempUnit};
use crate::sensors::Telemetry;

use super::DisplayProtocol;

/// Protocol encoder for the Sudokoo SK700V digital segmented cooler display.
pub struct Sk700vProtocol;

impl DisplayProtocol for Sk700vProtocol {
    fn name(&self) -> &'static str {
        "sk700v"
    }

    fn validate_profile(&self, profile: &DeviceProfile) -> Result<()> {
        if !(20..=64).contains(&profile.report_length) {
            bail!(
                "invalid profile '{}': sk700v report_length {} must be in 20..=64",
                profile.name,
                profile.report_length
            );
        }
        Ok(())
    }

    fn encode_telemetry(
        &self,
        telemetry: &Telemetry,
        profile: &DeviceProfile,
    ) -> Result<Vec<Vec<u8>>> {
        self.validate_profile(profile)?;
        let frame = encode_packet(telemetry, profile);
        Ok(vec![frame[..profile.report_length].to_vec()])
    }
}

/// Computes the 8-bit checksum over bytes `1..18` (excluding byte `0`, the HID Report ID).
pub fn compute_checksum(frame: &[u8]) -> u8 {
    frame
        .get(1..18)
        .unwrap_or_default()
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b))
}

/// Encodes a [`Telemetry`] sample into the raw 64-byte SK700V HID report.
pub fn encode_packet(telemetry: &Telemetry, profile: &DeviceProfile) -> [u8; 64] {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProtocolKind, SUDOKOO_VID};
    use crate::hid::protocol::{decode_hex_packet, for_protocol, format_hex};

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
        let protocol = for_protocol(profile.protocol);
        assert_eq!(protocol.name(), "sk700v");

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
            let reports = protocol.encode_telemetry(&telemetry, &profile).unwrap();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].len(), 64);
            assert_eq!(
                format_hex(&reports[0]),
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
        let reports = Sk700vProtocol
            .encode_telemetry(&telemetry, &profile)
            .unwrap();

        // Sum of bytes [1..18]:
        // 0x68 + 0x01 + 0x09 + 0x0d + 0x01 + 0x02 + 0x03 + 0xe7 + 0x64 + 0x00
        // + 0x42 + 0xc6 + 0x00 + 0x00 + 0x64 + 0x27 + 0x0b = 0x36E -> 0x6E
        let expected = decode_hex_packet("106801090d010203e7640042c6000064270b6e16", 64).unwrap();
        assert_eq!(reports[0], expected);
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
        let pkt = encode_packet(&telemetry, &profile);
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
