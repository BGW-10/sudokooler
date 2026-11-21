//! Shared Win32 Performance Data Helper (PDH) counter wrapper.
//!
//! Collects `\Processor Information(_Total)\% Processor Utility` (with a legacy
//! `% Processor Time` fallback) and computes effective CPU frequency from
//! `Processor Frequency * (% Processor Performance / 100)`.

use std::time::Duration;

use anyhow::{Result, anyhow};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW,
    PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows::core::PCWSTR;

/// RAII wrapper around a Win32 PDH query handle so it always closes on drop.
struct PdhQuery {
    handle: PDH_HQUERY,
}

impl Drop for PdhQuery {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: `self.handle` was opened by `PdhOpenQueryW` and is closed once here on drop.
            unsafe {
                let _ = PdhCloseQuery(self.handle);
            }
        }
    }
}

/// Shared Win32 Performance Data Helper (PDH) sampler for total CPU usage and effective clock speed.
pub(super) struct PdhCpuCounters {
    query: PdhQuery,
    usage_counter: PDH_HCOUNTER,
    base_freq_counter: Option<PDH_HCOUNTER>,
    perf_pct_counter: Option<PDH_HCOUNTER>,
}

impl PdhCpuCounters {
    /// Opens a PDH query, registers CPU counters, and sleeps briefly for the initial rate baseline.
    pub fn new() -> Result<Self> {
        let mut query_handle = PDH_HQUERY::default();
        // SAFETY: `query_handle` is a valid mutable out-pointer; passing NULL selects live data.
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
        // SAFETY: `query.handle` is an open PDH query handle owned by `query`.
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
        })
    }

    /// Samples `(cpu_usage_pct, cpu_freq_mhz)`.
    pub fn sample(&self) -> Result<(u8, u16)> {
        // SAFETY: `self.query.handle` remains valid for the lifetime of `self.query`.
        let status = unsafe { PdhCollectQueryData(self.query.handle) };
        if status != ERROR_SUCCESS.0 {
            return Err(anyhow!("PdhCollectQueryData failed with status 0x{status:08X}"));
        }

        let cpu_usage_pct = read_pdh_double(self.usage_counter)
            .unwrap_or(0.0)
            .round()
            .clamp(0.0, 100.0) as u8;

        let cpu_freq_mhz = match (self.base_freq_counter, self.perf_pct_counter) {
            (Some(base_c), Some(perf_c)) => {
                let base = read_pdh_double(base_c).unwrap_or(0.0);
                let perf = read_pdh_double(perf_c).unwrap_or(100.0);
                ((base * perf) / 100.0)
                    .round()
                    .clamp(0.0, f64::from(u16::MAX)) as u16
            }
            (Some(base_c), None) => read_pdh_double(base_c)
                .unwrap_or(0.0)
                .round()
                .clamp(0.0, f64::from(u16::MAX)) as u16,
            _ => 0,
        };

        Ok((cpu_usage_pct, cpu_freq_mhz))
    }
}

/// Encodes a Rust `&str` as a NUL-terminated UTF-16 buffer for Win32 `*W` APIs.
pub(super) fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn add_english_counter(query: PDH_HQUERY, path: &str) -> Result<PDH_HCOUNTER> {
    let wide = to_wide_null(path);
    let mut counter = PDH_HCOUNTER::default();
    // SAFETY: `wide` stays alive for the duration of `PdhAddEnglishCounterW` and is NUL-terminated.
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
    // SAFETY: `counter` belongs to `self.query`, and `PDH_FMT_DOUBLE` populates `Anonymous.doubleValue`.
    let status = unsafe { PdhGetFormattedCounterValue(counter, PDH_FMT_DOUBLE, None, &mut value) };
    if status != ERROR_SUCCESS.0 {
        return None;
    }
    // SAFETY: `status == ERROR_SUCCESS` with `PDH_FMT_DOUBLE` guarantees `doubleValue` is active.
    let v = unsafe { value.Anonymous.doubleValue };
    v.is_finite().then_some(v)
}
