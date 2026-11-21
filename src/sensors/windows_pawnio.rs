//! Standalone Windows CPU sensor backend via the signed **PawnIO** kernel driver.
//!
//! Communicates directly with `\\?\GLOBALROOT\Device\PawnIO` over Win32 `DeviceIoControl`
//! without requiring HWiNFO64 or any external runtime:
//!
//! 1. **`IOCTL_PIO_LOAD_BINARY`** (`0xA1B2_2084`): loads a signed module binary
//!    (`AMDFamily17.bin` on AMD Ryzen Zen 1–5, or `IntelMSR.bin` on Intel Core),
//!    searched in `SUDOKOOLER_PAWNIO_MODULE`, `<exe_dir>\pawnio\`, `<exe_dir>\`,
//!    `C:\Program Files\PawnIO\modules\`, `C:\Program Files\PawnIO\`, and `.\pawnio\`.
//! 2. **`IOCTL_PIO_EXECUTE_FN`** (`0xA1B2_2104`): sends a 32-byte ASCII function name
//!    followed by little-endian `u64` arguments and receives little-endian `u64` results:
//!    - AMD (`AMDFamily17.bin`): `ioctl_read_smn([0x00059800])` for `Tctl/Tdie` temp and
//!      `ioctl_read_msr([0xC0010299 | 0xC001029B])` for RAPL package wattage.
//!    - Intel (`IntelMSR.bin`): `ioctl_read_msr([0x1A2 | 0x1B1 | 0x19C])` for package temp.

use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, GENERIC_READ, GENERIC_WRITE, HANDLE, WAIT_ABANDONED,
    WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::PCWSTR;

use super::windows_pdh::{PdhCpuCounters, to_wide_null};
use super::{SensorProvider, Telemetry};

const PAWNIO_DEVICE_PATH: &str = r"\\?\GLOBALROOT\Device\PawnIO";
const ACCESS_PCI_MUTEX_NAME: &str = r"Global\Access_PCI";

const PAWNIO_DEVICE_TYPE: u32 = 41394u32 << 16;
const IOCTL_PIO_LOAD_BINARY: u32 = PAWNIO_DEVICE_TYPE | (0x821 << 2);
const IOCTL_PIO_EXECUTE_FN: u32 = PAWNIO_DEVICE_TYPE | (0x841 << 2);
const FN_NAME_LENGTH: usize = 32;

// AMD Family 17h-1Ah (Zen 1-5) SMN & RAPL constants (for AMDFamily17.bin)
const AMD_SMN_CUR_TEMP: u64 = 0x0005_9800;
const AMD_MSR_PWR_UNIT: u64 = 0xC001_0299;
const AMD_MSR_PKG_ENERGY_STAT: u64 = 0xC001_029B;

// Intel Core MSR constants (for IntelMSR.bin)
const INTEL_MSR_TEMPERATURE_TARGET: u64 = 0x0000_01A2;
const INTEL_MSR_PACKAGE_THERM_STATUS: u64 = 0x0000_01B1;
const INTEL_MSR_IA32_THERM_STATUS: u64 = 0x0000_019C;

/// Standalone Windows sensor provider backed by PawnIO (`AMDFamily17.bin` / `IntelMSR.bin`)
/// for CPU temperature & wattage, combined with Win32 PDH for CPU usage & effective clock.
pub struct WindowsPawnIoSensors {
    pdh: PdhCpuCounters,
    pawnio: PawnIoDevice,
}

impl WindowsPawnIoSensors {
    /// Attempts to open the PawnIO kernel driver and load the matching CPU module.
    /// Returns a human-readable reason if PawnIO is unavailable or lacks a module so the
    /// factory can fall back cleanly to HWiNFO64 shared memory.
    pub fn try_new() -> std::result::Result<Self, String> {
        let pawnio = PawnIoDevice::try_open()?;
        let pdh = PdhCpuCounters::new().map_err(|e| format!("PDH init failed: {e:#}"))?;
        Ok(Self { pdh, pawnio })
    }
}

impl SensorProvider for WindowsPawnIoSensors {
    fn read(&mut self) -> Result<Telemetry> {
        let (cpu_usage_pct, cpu_freq_mhz) = self.pdh.sample()?;
        let (temp_c, power_w) = self.pawnio.read();
        Ok(Telemetry {
            cpu_temp_c: temp_c.unwrap_or(0.0),
            cpu_usage_pct,
            cpu_power_w: power_w.unwrap_or(0),
            cpu_freq_mhz,
        })
    }

    fn backend_name(&self) -> &'static str {
        match self.pawnio.state {
            BackendState::AmdFamily17 { .. } => "windows (PawnIO AMDFamily17 + Win32 PDH)",
            BackendState::IntelMsr { .. } => "windows (PawnIO IntelMSR + Win32 PDH)",
        }
    }
}

struct PawnIoHandle(HANDLE);

impl Drop for PawnIoHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: `self.0` was returned by `CreateFileW` and is closed once on drop.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

impl PawnIoHandle {
    fn open() -> std::result::Result<Self, String> {
        let wide = to_wide_null(PAWNIO_DEVICE_PATH);
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that lives across `CreateFileW`.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };

        match handle {
            Ok(h) if !h.is_invalid() => Ok(Self(h)),
            Ok(_) => Err("PawnIO returned an invalid handle".to_string()),
            Err(err) => {
                if err.code() == ERROR_ACCESS_DENIED.to_hresult() {
                    Err("access denied opening PawnIO (run as Administrator)".to_string())
                } else {
                    Err(format!("PawnIO driver unavailable ({})", err.message()))
                }
            }
        }
    }

    fn load_binary(&self, blob: &[u8]) -> Result<()> {
        let len = u32::try_from(blob.len()).context("PawnIO module exceeds 4 GiB")?;
        let mut bytes_returned: u32 = 0;
        // SAFETY: `self.0` is an open PawnIO handle; `blob` is readable for `len` bytes.
        unsafe {
            DeviceIoControl(
                self.0,
                IOCTL_PIO_LOAD_BINARY,
                Some(blob.as_ptr().cast::<c_void>()),
                len,
                None,
                0,
                Some(&mut bytes_returned),
                None,
            )
        }
        .context("IOCTL_PIO_LOAD_BINARY failed")
    }

    fn execute(&self, fn_name: &str, args: &[u64], out_len: usize) -> Result<Vec<u64>> {
        let input = build_execute_input(fn_name, args);
        let in_len = u32::try_from(input.len()).context("PawnIO input too large")?;
        let mut raw_out = vec![0u8; out_len * std::mem::size_of::<u64>()];
        let out_bytes_len = u32::try_from(raw_out.len()).context("PawnIO output too large")?;
        let mut bytes_returned: u32 = 0;

        // SAFETY: `input` and `raw_out` remain valid for `in_len` and `out_bytes_len` bytes during synchronous I/O.
        unsafe {
            DeviceIoControl(
                self.0,
                IOCTL_PIO_EXECUTE_FN,
                Some(input.as_ptr().cast::<c_void>()),
                in_len,
                Some(raw_out.as_mut_ptr().cast::<c_void>()),
                out_bytes_len,
                Some(&mut bytes_returned),
                None,
            )
        }
        .with_context(|| format!("PawnIO function '{fn_name}' failed"))?;

        let count = (bytes_returned as usize) / std::mem::size_of::<u64>();
        let mut out = Vec::with_capacity(count);
        for chunk in raw_out[..count * 8].chunks_exact(8) {
            out.push(u64::from_le_bytes(chunk.try_into().expect("8-byte chunk")));
        }
        Ok(out)
    }
}

/// Best-effort mutual exclusion matching LibreHardwareMonitor's `Global\Access_PCI` mutex
/// before issuing PCI config-space SMN index/data cycles.
struct PciMutexGuard {
    handle: HANDLE,
    acquired: bool,
}

impl PciMutexGuard {
    fn acquire_best_effort() -> Self {
        let wide = to_wide_null(ACCESS_PCI_MUTEX_NAME);
        // SAFETY: `wide` is NUL-terminated UTF-16; ownership of the created/opened mutex handle lives in `Self`.
        let Ok(handle) = (unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }) else {
            return Self {
                handle: HANDLE::default(),
                acquired: false,
            };
        };

        // SAFETY: `handle` is a valid mutex handle created/opened directly above.
        let wait = unsafe { WaitForSingleObject(handle, 1000) };
        let acquired = wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED;
        Self { handle, acquired }
    }
}

impl Drop for PciMutexGuard {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            if self.acquired {
                // SAFETY: We own the mutex (`WAIT_OBJECT_0` / `WAIT_ABANDONED`) and release it before closing.
                unsafe {
                    let _ = ReleaseMutex(self.handle);
                }
            }
            // SAFETY: `self.handle` is valid and closed once here on drop.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModuleKind {
    AmdFamily17,
    IntelMsr,
}

struct AmdRaplState {
    energy_unit_j: f64,
    prev_energy_raw: u64,
    prev_at: Instant,
}

enum BackendState {
    AmdFamily17 { rapl: Option<AmdRaplState> },
    IntelMsr { tj_max_c: Option<f32> },
}

struct PawnIoDevice {
    handle: PawnIoHandle,
    state: BackendState,
}

impl PawnIoDevice {
    fn try_open() -> std::result::Result<Self, String> {
        let handle = PawnIoHandle::open()?;
        let kind = load_matching_module(&handle)?;

        let state = match kind {
            ModuleKind::AmdFamily17 => {
                let rapl = init_amd_rapl(&handle);
                BackendState::AmdFamily17 { rapl }
            }
            ModuleKind::IntelMsr => {
                let tj_max_c = handle
                    .execute("ioctl_read_msr", &[INTEL_MSR_TEMPERATURE_TARGET], 1)
                    .ok()
                    .and_then(|out| out.first().copied())
                    .map(|val| ((val >> 16) & 0xFF) as f32)
                    .filter(|&tj| tj > 0.0 && tj <= 150.0);
                BackendState::IntelMsr { tj_max_c }
            }
        };

        Ok(Self { handle, state })
    }

    fn read(&mut self) -> (Option<f32>, Option<u16>) {
        match &mut self.state {
            BackendState::AmdFamily17 { rapl } => {
                let temp_c = {
                    let _guard = PciMutexGuard::acquire_best_effort();
                    self.handle
                        .execute("ioctl_read_smn", &[AMD_SMN_CUR_TEMP], 1)
                        .ok()
                        .and_then(|out| out.first().copied())
                        .map(decode_amd_tctl_c)
                };

                let power_w = rapl.as_mut().and_then(|state| {
                    let now_raw = self
                        .handle
                        .execute("ioctl_read_msr", &[AMD_MSR_PKG_ENERGY_STAT], 1)
                        .ok()?
                        .into_iter()
                        .next()?;
                    let now_at = Instant::now();
                    let elapsed = now_at.duration_since(state.prev_at).as_secs_f64();
                    let watts = decode_amd_rapl_watts(
                        state.prev_energy_raw,
                        now_raw,
                        state.energy_unit_j,
                        elapsed,
                    );
                    if elapsed >= 0.05 {
                        state.prev_energy_raw = now_raw;
                        state.prev_at = now_at;
                    }
                    watts
                });

                (temp_c, power_w)
            }
            BackendState::IntelMsr { tj_max_c } => {
                let raw = self
                    .handle
                    .execute("ioctl_read_msr", &[INTEL_MSR_PACKAGE_THERM_STATUS], 1)
                    .or_else(|_| {
                        self.handle
                            .execute("ioctl_read_msr", &[INTEL_MSR_IA32_THERM_STATUS], 1)
                    })
                    .ok()
                    .and_then(|out| out.into_iter().next());

                let temp_c = raw.and_then(|val| decode_intel_pkg_temp_c(*tj_max_c, val));
                (temp_c, None)
            }
        }
    }
}

fn init_amd_rapl(handle: &PawnIoHandle) -> Option<AmdRaplState> {
    let pwr_unit = handle
        .execute("ioctl_read_msr", &[AMD_MSR_PWR_UNIT], 1)
        .ok()?
        .into_iter()
        .next()?;
    let energy_unit_exp = ((pwr_unit >> 8) & 0x1F) as i32;
    let energy_unit_j = 0.5_f64.powi(energy_unit_exp);
    let prev_energy_raw = handle
        .execute("ioctl_read_msr", &[AMD_MSR_PKG_ENERGY_STAT], 1)
        .ok()?
        .into_iter()
        .next()?;
    Some(AmdRaplState {
        energy_unit_j,
        prev_energy_raw,
        prev_at: Instant::now(),
    })
}

fn load_matching_module(handle: &PawnIoHandle) -> std::result::Result<ModuleKind, String> {
    if let Ok(env_val) = std::env::var("SUDOKOOLER_PAWNIO_MODULE") {
        let env_path = PathBuf::from(env_val);
        if env_path.is_file() {
            let blob = fs::read(&env_path).map_err(|e| {
                format!(
                    "failed to read SUDOKOOLER_PAWNIO_MODULE '{}': {e}",
                    env_path.display()
                )
            })?;
            let kind = if env_path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().contains("intel"))
            {
                ModuleKind::IntelMsr
            } else {
                ModuleKind::AmdFamily17
            };
            handle
                .load_binary(&blob)
                .map_err(|e| format!("failed to load '{}': {e:#}", env_path.display()))?;
            return Ok(kind);
        }
    }

    let order = preferred_module_order();
    let mut last_load_err = None;

    for (stem, kind) in order {
        let file_name = format!("{stem}.bin");
        for candidate in module_candidate_paths(&file_name) {
            if !candidate.is_file() {
                continue;
            }
            let Ok(blob) = fs::read(&candidate) else {
                continue;
            };
            match handle.load_binary(&blob) {
                Ok(()) => return Ok(kind),
                Err(err) => {
                    last_load_err = Some(format!("{}: {err:#}", candidate.display()));
                }
            }
        }
    }

    if let Some(err) = last_load_err {
        Err(format!("PawnIO module load failed ({err})"))
    } else {
        let primary_stem = order[0].0;
        Err(format!(
            "no signed PawnIO module ({primary_stem}.bin) found in standard paths"
        ))
    }
}

fn preferred_module_order() -> [(&'static str, ModuleKind); 2] {
    if cpu_vendor_is_intel() {
        [
            ("IntelMSR", ModuleKind::IntelMsr),
            ("AMDFamily17", ModuleKind::AmdFamily17),
        ]
    } else {
        [
            ("AMDFamily17", ModuleKind::AmdFamily17),
            ("IntelMSR", ModuleKind::IntelMsr),
        ]
    }
}

fn cpu_vendor_is_intel() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        let res = std::arch::x86_64::__cpuid(0);
        let mut vendor = [0u8; 12];
        vendor[0..4].copy_from_slice(&res.ebx.to_le_bytes());
        vendor[4..8].copy_from_slice(&res.edx.to_le_bytes());
        vendor[8..12].copy_from_slice(&res.ecx.to_le_bytes());
        &vendor == b"GenuineIntel"
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

fn module_candidate_paths(file_name: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(env_val) = std::env::var("SUDOKOOLER_PAWNIO_MODULE") {
        let p = Path::new(&env_val);
        if p.is_dir() {
            paths.push(p.join(file_name));
        }
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        paths.push(exe_dir.join("pawnio").join(file_name));
        paths.push(exe_dir.join(file_name));
    }

    paths.push(PathBuf::from(r"C:\Program Files\PawnIO\modules").join(file_name));
    paths.push(PathBuf::from(r"C:\Program Files\PawnIO").join(file_name));
    paths.push(PathBuf::from("pawnio").join(file_name));

    paths
}

pub(crate) fn build_execute_input(fn_name: &str, args: &[u64]) -> Vec<u8> {
    let mut buf = vec![0u8; FN_NAME_LENGTH + args.len() * std::mem::size_of::<u64>()];
    let name_bytes = fn_name.as_bytes();
    let copy_len = name_bytes.len().min(FN_NAME_LENGTH - 1);
    buf[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
    for (idx, &arg) in args.iter().enumerate() {
        let offset = FN_NAME_LENGTH + idx * 8;
        buf[offset..offset + 8].copy_from_slice(&arg.to_le_bytes());
    }
    buf
}

/// Decodes `SMU::THM::THM_TCON_CUR_TMP` (`0x00059800`) into °C.
pub(crate) fn decode_amd_tctl_c(raw_smn: u64) -> f32 {
    let raw = raw_smn as u32;
    let cur_temp = (raw >> 21) & 0x7FF;
    let range_sel = (raw >> 19) & 0x1;
    let mut temp_c = (cur_temp as f32) * 0.125;
    if range_sel == 1 {
        temp_c -= 49.0;
    }
    temp_c
}

/// Computes package wattage from consecutive 32-bit `MSR_PKG_ENERGY_STAT` samples.
pub(crate) fn decode_amd_rapl_watts(
    prev_raw: u64,
    now_raw: u64,
    energy_unit_j: f64,
    elapsed_secs: f64,
) -> Option<u16> {
    if elapsed_secs < 0.05 || !energy_unit_j.is_finite() || energy_unit_j <= 0.0 {
        return None;
    }
    let delta_ticks = (now_raw as u32).wrapping_sub(prev_raw as u32);
    let watts = (f64::from(delta_ticks) * energy_unit_j) / elapsed_secs;
    watts
        .is_finite()
        .then_some(watts.round().clamp(0.0, 999.0) as u16)
}

/// Decodes Intel `IA32_PACKAGE_THERM_STATUS` / `IA32_THERM_STATUS` using `TjMax`.
pub(crate) fn decode_intel_pkg_temp_c(tj_max_c: Option<f32>, therm_status_raw: u64) -> Option<f32> {
    let valid = ((therm_status_raw >> 31) & 0x1) == 1;
    if !valid {
        return None;
    }
    let tj_max = tj_max_c.unwrap_or(100.0);
    let digital_readout = ((therm_status_raw >> 16) & 0x7F) as f32;
    Some(tj_max - digital_readout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_amd_tctl_with_and_without_offset() {
        // range_sel = 0, cur_temp = 440 (440 * 0.125 = 55.0 C)
        let raw_no_offset = 440u64 << 21;
        assert!((decode_amd_tctl_c(raw_no_offset) - 55.0).abs() < 1e-5);

        // range_sel = 1, cur_temp = 832 (832 * 0.125 - 49.0 = 55.0 C)
        let raw_with_offset = (832u64 << 21) | (1u64 << 19);
        assert!((decode_amd_tctl_c(raw_with_offset) - 55.0).abs() < 1e-5);
    }

    #[test]
    fn decodes_amd_rapl_watts_including_32bit_wrap() {
        // Standard AMD energy unit = 2^-16 J (~15.2588 uJ)
        let unit = 0.5_f64.powi(16);
        let ticks_for_65w = (65.0 / unit).round() as u64;
        assert_eq!(
            decode_amd_rapl_watts(1000, 1000 + ticks_for_65w, unit, 1.0),
            Some(65)
        );

        // Simulate 32-bit wraparound across u32::MAX
        let prev = u32::MAX as u64 - 500;
        let now = ticks_for_65w - 501;
        assert_eq!(decode_amd_rapl_watts(prev, now, unit, 1.0), Some(65));
    }

    #[test]
    fn decodes_intel_package_temperature() {
        // Bit 31 valid = 1, readout = 35 => 100 - 35 = 65.0 C
        let valid_raw = (1u64 << 31) | (35u64 << 16);
        assert_eq!(decode_intel_pkg_temp_c(Some(100.0), valid_raw), Some(65.0));

        // Bit 31 valid = 0 => None
        let invalid_raw = 35u64 << 16;
        assert_eq!(decode_intel_pkg_temp_c(Some(100.0), invalid_raw), None);
    }

    #[test]
    fn builds_execute_input_with_32_byte_padded_name_and_le_args() {
        let buf = build_execute_input("ioctl_read_smn", &[0x0005_9800]);
        assert_eq!(buf.len(), 40);
        assert_eq!(&buf[..14], b"ioctl_read_smn");
        assert!(buf[14..32].iter().all(|&b| b == 0));
        assert_eq!(
            u64::from_le_bytes(buf[32..40].try_into().unwrap()),
            0x0005_9800
        );
    }
}
