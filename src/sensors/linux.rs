use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::{SensorProvider, Telemetry};

const PREFERRED_TEMP_DRIVERS: &[&str] = &["k10temp", "zenpower", "coretemp", "fam15h_power"];

pub struct LinuxSensors {
    temp_input: Option<PathBuf>,
    power_input: Option<PathBuf>,
    rapl_energy_input: Option<PathBuf>,
    rapl_max_energy_uj: u64,
    prev_energy_uj: Option<u64>,
    prev_energy_at: Option<Instant>,
    prev_idle_ticks: u64,
    prev_total_ticks: u64,
}

impl LinuxSensors {
    pub fn new() -> Result<Self> {
        let (temp_input, power_input) = discover_hwmon_paths();
        let (rapl_energy_input, rapl_max_energy_uj) = discover_rapl_path();

        let (prev_idle_ticks, prev_total_ticks) =
            read_proc_stat_totals().context("failed to read initial /proc/stat CPU ticks")?;

        let prev_energy_uj = rapl_energy_input
            .as_deref()
            .and_then(|p| read_u64_file(p).ok());
        let prev_energy_at = prev_energy_uj.map(|_| Instant::now());

        // Brief baseline sleep so the very first `read()` yields non-zero CPU % and RAPL deltas.
        std::thread::sleep(Duration::from_millis(100));

        Ok(Self {
            temp_input,
            power_input,
            rapl_energy_input,
            rapl_max_energy_uj,
            prev_energy_uj,
            prev_energy_at,
            prev_idle_ticks,
            prev_total_ticks,
        })
    }

    fn read_temp_c(&self) -> f32 {
        let Some(path) = self.temp_input.as_deref() else {
            return 0.0;
        };
        read_u64_file(path)
            .map(|milli_c| (milli_c as f32) / 1000.0)
            .unwrap_or(0.0)
    }

    fn read_usage_pct(&mut self) -> u8 {
        let Ok((idle, total)) = read_proc_stat_totals() else {
            return 0;
        };

        let delta_total = total.saturating_sub(self.prev_total_ticks);
        let delta_idle = idle.saturating_sub(self.prev_idle_ticks);

        self.prev_idle_ticks = idle;
        self.prev_total_ticks = total;

        if delta_total == 0 {
            return 0;
        }
        let busy = delta_total.saturating_sub(delta_idle);
        ((busy as f64 / delta_total as f64) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8
    }

    fn read_power_w(&mut self) -> u16 {
        if let Some(path) = self.power_input.as_deref()
            && let Ok(microwatts) = read_u64_file(path)
        {
            return ((microwatts as f64) / 1_000_000.0)
                .round()
                .clamp(0.0, 999.0) as u16;
        }

        let Some(rapl_path) = self.rapl_energy_input.as_deref() else {
            return 0;
        };
        let Ok(now_uj) = read_u64_file(rapl_path) else {
            return 0;
        };
        let now = Instant::now();

        let watts = match (self.prev_energy_uj, self.prev_energy_at) {
            (Some(prev_uj), Some(prev_at)) => {
                let elapsed = now.duration_since(prev_at).as_secs_f64();
                let delta_uj = if now_uj >= prev_uj {
                    now_uj - prev_uj
                } else if self.rapl_max_energy_uj > prev_uj {
                    (self.rapl_max_energy_uj - prev_uj) + now_uj
                } else {
                    0
                };
                if elapsed > 0.001 {
                    ((delta_uj as f64) / (elapsed * 1_000_000.0))
                        .round()
                        .clamp(0.0, 999.0) as u16
                } else {
                    0
                }
            }
            _ => 0,
        };

        self.prev_energy_uj = Some(now_uj);
        self.prev_energy_at = Some(now);
        watts
    }
}

impl SensorProvider for LinuxSensors {
    fn read(&mut self) -> Result<Telemetry> {
        Ok(Telemetry {
            cpu_temp_c: self.read_temp_c(),
            cpu_usage_pct: self.read_usage_pct(),
            cpu_power_w: self.read_power_w(),
            cpu_freq_mhz: read_cpu_freq_mhz(),
        })
    }

    fn backend_name(&self) -> &'static str {
        "linux (/sys/class/hwmon + /proc/stat)"
    }
}

fn discover_hwmon_paths() -> (Option<PathBuf>, Option<PathBuf>) {
    let Ok(entries) = fs::read_dir("/sys/class/hwmon") else {
        return (None, None);
    };

    let mut best_temp: Option<(usize, PathBuf)> = None;
    let mut best_power: Option<(usize, PathBuf)> = None;

    for entry in entries.flatten() {
        let dir = entry.path();
        let Ok(name_raw) = fs::read_to_string(dir.join("name")) else {
            continue;
        };
        let name = name_raw.trim();
        let Some(priority) = PREFERRED_TEMP_DRIVERS.iter().position(|&d| d == name) else {
            continue;
        };

        if let Some(temp_path) = pick_hwmon_temp_file(&dir)
            && best_temp.as_ref().is_none_or(|(p, _)| priority < *p)
        {
            best_temp = Some((priority, temp_path));
        }

        if let Some(power_path) = pick_hwmon_power_file(&dir)
            && best_power.as_ref().is_none_or(|(p, _)| priority < *p)
        {
            best_power = Some((priority, power_path));
        }
    }

    (best_temp.map(|(_, p)| p), best_power.map(|(_, p)| p))
}

fn pick_hwmon_temp_file(hwmon_dir: &Path) -> Option<PathBuf> {
    // Prefer labeled Tctl/Tdie/Package sensors when multiple tempN_input files exist.
    for idx in 1..=8 {
        let input = hwmon_dir.join(format!("temp{idx}_input"));
        if !input.exists() {
            continue;
        }
        let label_path = hwmon_dir.join(format!("temp{idx}_label"));
        if let Ok(label) = fs::read_to_string(label_path) {
            let l = label.trim().to_ascii_lowercase();
            if l.contains("tctl") || l.contains("tdie") || l.contains("package") {
                return Some(input);
            }
        }
    }

    let fallback = hwmon_dir.join("temp1_input");
    fallback.exists().then_some(fallback)
}

fn pick_hwmon_power_file(hwmon_dir: &Path) -> Option<PathBuf> {
    for candidate in ["power1_input", "power1_average"] {
        let path = hwmon_dir.join(candidate);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

fn discover_rapl_path() -> (Option<PathBuf>, u64) {
    let energy = PathBuf::from("/sys/class/powercap/intel-rapl:0/energy_uj");
    if !energy.exists() {
        return (None, 0);
    }
    let max_uj =
        read_u64_file(Path::new("/sys/class/powercap/intel-rapl:0/max_energy_range_uj"))
            .unwrap_or(0);
    (Some(energy), max_uj)
}

fn read_proc_stat_totals() -> Result<(u64, u64)> {
    let content = fs::read_to_string("/proc/stat").context("failed to read /proc/stat")?;
    let first_line = content
        .lines()
        .next()
        .context("/proc/stat is unexpectedly empty")?;

    let mut parts = first_line.split_whitespace();
    let _label = parts.next();
    let fields: Vec<u64> = parts.filter_map(|s| s.parse::<u64>().ok()).collect();
    if fields.len() < 4 {
        anyhow::bail!("unexpected /proc/stat cpu line format");
    }

    let idle = fields[3].saturating_add(*fields.get(4).unwrap_or(&0));
    let total: u64 = fields.iter().copied().sum();
    Ok((idle, total))
}

fn read_cpu_freq_mhz() -> u16 {
    if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") {
        let mut sum_khz: u64 = 0;
        let mut count: u64 = 0;

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !name_str.starts_with("cpu")
                || name_str.len() <= 3
                || !name_str[3..].chars().all(|c| c.is_ascii_digit())
            {
                continue;
            }
            let freq_path = entry.path().join("cpufreq/scaling_cur_freq");
            if let Ok(khz) = read_u64_file(&freq_path) {
                sum_khz = sum_khz.saturating_add(khz);
                count += 1;
            }
        }

        if count > 0 {
            return ((sum_khz / count) / 1000).min(u16::MAX as u64) as u16;
        }
    }

    if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo") {
        let mut sum_mhz = 0.0f64;
        let mut count = 0u64;
        for line in cpuinfo.lines() {
            if let Some(rest) = line.strip_prefix("cpu MHz")
                && let Some((_, val)) = rest.split_once(':')
                && let Ok(mhz) = val.trim().parse::<f64>()
            {
                sum_mhz += mhz;
                count += 1;
            }
        }
        if count > 0 {
            return (sum_mhz / count as f64)
                .round()
                .clamp(0.0, u16::MAX as f64) as u16;
        }
    }

    0
}

fn read_u64_file(path: &Path) -> Result<u64> {
    let raw = fs::read_to_string(path)?;
    Ok(raw.trim().parse::<u64>()?)
}
