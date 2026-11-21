use std::ffi::c_void;
use std::ptr;
use std::time::Duration;

use anyhow::{Result, anyhow};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
use windows::Win32::System::Memory::{
    FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
};
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW,
    PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows::core::PCWSTR;

use super::{SensorProvider, Telemetry};

const HWINFO_SHMEM_NAMES: &[&str] = &["Global\\HWiNFO_SENS_SM2", "Local\\HWiNFO_SENS_SM2"];
const HWINFO_HEADER_SIZE: usize = 44;
const HWINFO_MIN_READING_SIZE: usize = 292;
const HWINFO_MAX_MAP_BYTES: usize = 16 * 1024 * 1024;

const HWINFO_TYPE_TEMP: u32 = 1;
const HWINFO_TYPE_POWER: u32 = 5;
const HWINFO_TYPE_CLOCK: u32 = 6;
const HWINFO_TYPE_USAGE: u32 = 7;

/// RAII wrapper around a Win32 PDH query handle so it always closes on drop.
struct PdhQuery {
    handle: PDH_HQUERY,
}

impl Drop for PdhQuery {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            unsafe {
                let _ = PdhCloseQuery(self.handle);
            }
        }
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct MappedView(MEMORY_MAPPED_VIEW_ADDRESS);

impl Drop for MappedView {
    fn drop(&mut self) {
        if !self.0.Value.is_null() {
            unsafe {
                let _ = UnmapViewOfFile(self.0);
            }
        }
    }
}

pub struct WindowsSensors {
    query: PdhQuery,
    usage_counter: PDH_HCOUNTER,
    base_freq_counter: Option<PDH_HCOUNTER>,
    perf_pct_counter: Option<PDH_HCOUNTER>,
    warned_no_hwinfo: bool,
}

impl WindowsSensors {
    pub fn new() -> Result<Self> {
        let mut query_handle = PDH_HQUERY::default();
        let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query_handle) };
        if status != ERROR_SUCCESS.0 {
            return Err(anyhow!("PdhOpenQueryW failed with status 0x{status:08X}"));
        }
        let query = PdhQuery {
            handle: query_handle,
        };

        let usage_counter = add_english_counter(
            query.handle,
            "\\Processor Information(_Total)\\% Processor Utility",
        )
        .or_else(|_| {
            add_english_counter(query.handle, "\\Processor(_Total)\\% Processor Time")
        })?;

        let base_freq_counter = add_english_counter(
            query.handle,
            "\\Processor Information(_Total)\\Processor Frequency",
        )
        .ok();

        let perf_pct_counter = add_english_counter(
            query.handle,
            "\\Processor Information(_Total)\\% Processor Performance",
        )
        .ok();

        // PDH rate counters require two collections separated by a short interval before
        // `PdhGetFormattedCounterValue` returns a valid sample.
        let status = unsafe { PdhCollectQueryData(query.handle) };
        if status != ERROR_SUCCESS.0 {
            return Err(anyhow!(
                "initial PdhCollectQueryData failed with status 0x{status:08X}"
            ));
        }
        std::thread::sleep(Duration::from_millis(120));

        Ok(Self {
            query,
            usage_counter,
            base_freq_counter,
            perf_pct_counter,
            warned_no_hwinfo: false,
        })
    }
}

impl SensorProvider for WindowsSensors {
    fn read(&mut self) -> Result<Telemetry> {
        let status = unsafe { PdhCollectQueryData(self.query.handle) };
        if status != ERROR_SUCCESS.0 {
            return Err(anyhow!("PdhCollectQueryData failed with status 0x{status:08X}"));
        }

        let mut cpu_usage_pct = read_pdh_double(self.usage_counter)
            .unwrap_or(0.0)
            .round()
            .clamp(0.0, 100.0) as u8;

        let mut cpu_freq_mhz = match (self.base_freq_counter, self.perf_pct_counter) {
            (Some(base_c), Some(perf_c)) => {
                let base = read_pdh_double(base_c).unwrap_or(0.0);
                let perf = read_pdh_double(perf_c).unwrap_or(100.0);
                ((base * perf) / 100.0).round().clamp(0.0, f64::from(u16::MAX)) as u16
            }
            (Some(base_c), None) => read_pdh_double(base_c)
                .unwrap_or(0.0)
                .round()
                .clamp(0.0, f64::from(u16::MAX)) as u16,
            _ => 0,
        };

        let mut cpu_temp_c = 0.0f32;
        let mut cpu_power_w = 0u16;

        match read_hwinfo_cpu_sensors() {
            Some(hw) => {
                if let Some(temp) = hw.temp_c {
                    cpu_temp_c = temp;
                }
                if let Some(power) = hw.power_w {
                    cpu_power_w = power;
                }
                if cpu_usage_pct == 0
                    && let Some(usage) = hw.usage_pct
                {
                    cpu_usage_pct = usage;
                }
                if cpu_freq_mhz == 0
                    && let Some(freq) = hw.freq_mhz
                {
                    cpu_freq_mhz = freq;
                }
            }
            None if !self.warned_no_hwinfo => {
                self.warned_no_hwinfo = true;
                eprintln!(
                    "[sensors] HWiNFO64 shared memory not detected; CPU temp/power will report 0 unless HWiNFO Sensors is running with Shared Memory Support enabled."
                );
            }
            None => {}
        }

        Ok(Telemetry {
            cpu_temp_c,
            cpu_usage_pct,
            cpu_power_w,
            cpu_freq_mhz,
        })
    }

    fn backend_name(&self) -> &'static str {
        "windows (Win32 PDH + optional HWiNFO64 shared memory)"
    }
}

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn add_english_counter(query: PDH_HQUERY, path: &str) -> Result<PDH_HCOUNTER> {
    let wide = to_wide_null(path);
    let mut counter = PDH_HCOUNTER::default();
    let status = unsafe { PdhAddEnglishCounterW(query, PCWSTR(wide.as_ptr()), 0, &mut counter) };
    if status != ERROR_SUCCESS.0 {
        return Err(anyhow!(
            "PdhAddEnglishCounterW('{path}') failed with status 0x{status:08X}"
        ));
    }
    Ok(counter)
}

fn read_pdh_double(counter: PDH_HCOUNTER) -> Option<f64> {
    let mut value = PDH_FMT_COUNTERVALUE::default();
    let status = unsafe { PdhGetFormattedCounterValue(counter, PDH_FMT_DOUBLE, None, &mut value) };
    if status != ERROR_SUCCESS.0 {
        return None;
    }
    let v = unsafe { value.Anonymous.doubleValue };
    v.is_finite().then_some(v)
}

#[derive(Default)]
struct HwinfoSnapshot {
    temp_c: Option<f32>,
    power_w: Option<u16>,
    usage_pct: Option<u8>,
    freq_mhz: Option<u16>,
}

fn read_hwinfo_cpu_sensors() -> Option<HwinfoSnapshot> {
    let mapping = HWINFO_SHMEM_NAMES.iter().find_map(|name| {
        let wide = to_wide_null(name);
        unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(wide.as_ptr())) }.ok()
    })?;
    let mapping = OwnedHandle(mapping);

    // Map only the 44-byte header first so we can verify the signature and bounds-check
    // the full reading table length before touching any element offsets.
    let header_view = unsafe { MapViewOfFile(mapping.0, FILE_MAP_READ, 0, 0, HWINFO_HEADER_SIZE) };
    if header_view.Value.is_null() {
        return None;
    }
    let header_guard = MappedView(header_view);
    let header = unsafe {
        std::slice::from_raw_parts(header_guard.0.Value as *const u8, HWINFO_HEADER_SIZE)
    };
    if &header[0..4] != b"HWiS" {
        return None;
    }

    let reading_offset = u32::from_le_bytes(header[32..36].try_into().ok()?) as usize;
    let reading_size = u32::from_le_bytes(header[36..40].try_into().ok()?) as usize;
    let reading_count = u32::from_le_bytes(header[40..44].try_into().ok()?) as usize;
    drop(header_guard);

    if reading_size < HWINFO_MIN_READING_SIZE || reading_count == 0 {
        return None;
    }

    let total_bytes = reading_offset.checked_add(reading_size.checked_mul(reading_count)?)?;
    if total_bytes > HWINFO_MAX_MAP_BYTES {
        return None;
    }

    let full_view = unsafe { MapViewOfFile(mapping.0, FILE_MAP_READ, 0, 0, total_bytes) };
    if full_view.Value.is_null() {
        return None;
    }
    let full_guard = MappedView(full_view);
    let bytes =
        unsafe { std::slice::from_raw_parts(full_guard.0.Value as *const u8, total_bytes) };

    let mut snap = HwinfoSnapshot::default();
    let mut temp_priority = usize::MAX;
    let mut power_priority = usize::MAX;
    let mut clock_sum = 0.0f64;
    let mut clock_count = 0usize;

    for idx in 0..reading_count {
        let start = reading_offset + idx * reading_size;
        let elem = &bytes[start..start + reading_size];

        let reading_type = u32::from_le_bytes(elem[0..4].try_into().ok()?);
        let label_orig = read_cstr(&elem[12..140]);
        let label_user = read_cstr(&elem[140..268]);
        let label = if label_user.is_empty() {
            label_orig
        } else {
            label_user
        };
        let label_lc = label.to_ascii_lowercase();
        let unit = read_cstr(&elem[268..284]);
        let value = f64::from_le_bytes(elem[284..292].try_into().ok()?);

        if !value.is_finite() {
            continue;
        }

        match reading_type {
            HWINFO_TYPE_TEMP => {
                let prio = if label_lc.contains("cpu (tctl/tdie)") {
                    Some(0)
                } else if label_lc.contains("core (tctl/tdie)") {
                    Some(1)
                } else if label_lc == "cpu package" || label_lc.contains("cpu die (average)") {
                    Some(2)
                } else if label_lc.contains("cpu") && !label_lc.contains("gpu") {
                    Some(3)
                } else {
                    None
                };

                if let Some(p) = prio
                    && p < temp_priority
                {
                    let temp_c = if unit.eq_ignore_ascii_case("°F") || unit == "F" {
                        (value - 32.0) * 5.0 / 9.0
                    } else {
                        value
                    };
                    temp_priority = p;
                    snap.temp_c = Some(temp_c as f32);
                }
            }
            HWINFO_TYPE_POWER => {
                let prio = if label_lc.contains("cpu package power") {
                    Some(0)
                } else if label_lc.contains("cpu ppt") {
                    Some(1)
                } else if label_lc.contains("package power") && !label_lc.contains("gpu") {
                    Some(2)
                } else {
                    None
                };

                if let Some(p) = prio
                    && p < power_priority
                {
                    power_priority = p;
                    snap.power_w = Some(value.round().clamp(0.0, 999.0) as u16);
                }
            }
            HWINFO_TYPE_USAGE if snap.usage_pct.is_none() && label_lc == "total cpu usage" => {
                snap.usage_pct = Some(value.round().clamp(0.0, 100.0) as u8);
            }
            HWINFO_TYPE_CLOCK
                if label_lc.starts_with("core ") && label_lc.contains("clock") && value > 100.0 =>
            {
                clock_sum += value;
                clock_count += 1;
            }
            _ => {}
        }
    }

    if clock_count > 0 {
        snap.freq_mhz = Some(
            (clock_sum / clock_count as f64)
                .round()
                .clamp(0.0, f64::from(u16::MAX)) as u16,
        );
    }

    let _ = ptr::NonNull::<c_void>::dangling();
    Some(snap)
}

fn read_cstr(buf: &[u8]) -> &str {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..end]).unwrap_or("").trim()
}
