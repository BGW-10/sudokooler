# sudokooler

A lightweight, open-source, cross-platform (Linux & Windows) daemon written in Rust to drive digital segmented displays on Sudokoo CPU coolers via USB HID, replacing the bloated MasterCraft suite.

## Supported Hardware

| Model | VID | PID | Protocol | Status |
| --- | --- | --- | --- | --- |
| Sudokoo SK700V | `0x381C` | `0x0003` | `sk700v` | Verified & Enabled |
| Other `0x381C` PIDs (`0x0001`, `0x0002`, `0x0004`, `0x0005`) | `0x381C` | `0x0001`–`0x0005` | `sk700v` (tentative) | Disabled / Unverified |

> **Safety guardrail:** `sudokooler` strictly filters USB HID devices by `VID == 0x381C` and `enabled = true` profiles in `profiles/default_devices.toml`. Unverified PIDs remain disabled until captured and tested on physical hardware.

## Building & Running

```bash
# Run unit tests (including byte-for-byte SK700V packet parity tests)
cargo test

# Build optimized release binary
cargo build --release
```

### CLI Usage

```bash
# List detected Sudokoo (VID 0x381C) USB HID interfaces
sudokooler --list-devices

# Sample sensors and print 64-byte hex packets without writing to USB
sudokooler --dry-run --count 3

# Run the 1 Hz polling daemon (Ctrl+C sends display-off packet and exits cleanly)
sudokooler

# Optional flags
sudokooler --config path/to/devices.toml --interval-ms 1000 --count 0
```

## Platform Sensor Backends

### Linux
- Reads CPU temperature and power directly from `/sys/class/hwmon` (`k10temp`, `zenpower`, `coretemp`, `fam15h_power`) with `/sys/class/powercap/intel-rapl:0/energy_uj` fallback for wattage, `/proc/stat` for CPU usage, and `cpufreq`/`/proc/cpuinfo` for frequency—zero external runtime dependencies.
- Install the udev rule and systemd service template from `packaging/`:
  ```bash
  sudo cp packaging/99-sudokooler.rules /etc/udev/rules.d/
  sudo udevadm control --reload-rules && sudo udevadm trigger
  sudo cp target/release/sudokooler /usr/local/bin/sudokooler
  sudo cp packaging/sudokooler.service /etc/systemd/system/
  sudo systemctl enable --now sudokooler
  ```

### Windows
- Uses native Win32 PDH (`Processor Information(_Total)`) for CPU usage and effective clock speed.
- Opportunistically reads `Global\HWiNFO_SENS_SM2` shared memory for CPU package temperature (`CPU (Tctl/Tdie)` / `CPU Package`) and package power (`CPU Package Power` / `CPU PPT`) when HWiNFO Sensors is running with Shared Memory Support enabled (otherwise reports `0.0°C` / `0W`).

## AI Assistance Disclosure

This repository was bootstrapped and developed with assistance from **Google Antigravity** (agentic AI coding assistant), based on reverse-engineered USB HID captures and Python prototypes (`python/`).

## License

MIT
