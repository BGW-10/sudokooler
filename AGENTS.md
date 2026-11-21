# `sudokooler` — Engineering & Architecture Guide

`sudokooler` is a lightweight, cross-platform (Linux & Windows) synchronous Rust daemon that drives digital segmented displays on Sudokoo CPU coolers via USB HID, replacing the Electron/HWiNFO-based MasterCraft suite.

## Module Architecture

- `src/main.rs` — CLI entrypoint (`clap`) supporting `--list-devices`, `--dry-run`, `--config <PATH>`, `--interval-ms`, `--count`, and the default 1 Hz polling loop with `Ctrl+C` shutdown.
- `src/config.rs` — Declarative TOML loader and validator (`profiles/default_devices.toml` embedded via `include_str!`, with optional `--config` file override).
- `src/hid/protocol.rs` — Pure, hardware-independent packet encoding, checksum calculation, and 64-byte zero-padding (`encode_sk700v_packet`, `encode_telemetry`, `compute_checksum`). Covered by unit tests asserting byte-for-byte parity with captured SK700V USB HID frames.
- `src/hid/mod.rs` — Cross-platform `hidapi` enumerator and `CoolerDevice` handle wrapper with safe disconnect/reconnect handling.
- `src/sensors/mod.rs` — Cross-platform `Telemetry` struct and `SensorProvider` trait.
- `src/sensors/windows_pdh.rs` — Shared Win32 PDH (`Processor Information(_Total)`) helper for CPU usage and effective clock speed.
- `src/sensors/windows_pawnio.rs` — Standalone Windows sensor backend (`WindowsPawnIoSensors`) using PawnIO (`\\?\GLOBALROOT\Device\PawnIO` + signed `AMDFamily17.bin` / `IntelMSR.bin` modules) and Win32 PDH.
- `src/sensors/windows_hwinfo.rs` — Fallback Windows sensor backend (`WindowsHwinfoSensors`) using Win32 PDH and opportunistic `Global\HWiNFO_SENS_SM2` shared-memory lookup.
- `src/sensors/linux.rs` — Linux sensor backend: zero-dependency `/sys/class/hwmon` (`k10temp`, `zenpower`, `coretemp`), `/sys/class/powercap` RAPL fallback, `/proc/stat`, and `cpufreq`/`/proc/cpuinfo`.
- `python/sk700v_prototype.py` — Standalone PEP 723 (`uv run`) SK700V reference implementation and USB HID diagnostic/decoder tool.

## Data Contracts

### `Telemetry` (`src/sensors/mod.rs`)
```rust
pub struct Telemetry {
    pub cpu_temp_c: f32,
    pub cpu_usage_pct: u8,
    pub cpu_power_w: u16,
    pub cpu_freq_mhz: u16,
}
```

### `DeviceProfile` (`src/config.rs` & `profiles/default_devices.toml`)
- `name`: Human-readable model name (e.g., `"Sudokoo SK700V"`).
- `vid`: Must equal `0x381C`.
- `pid`: USB Product ID (e.g., `0x0003` for SK700V).
- `protocol`: Wire protocol variant (`"sk700v"`).
- `enabled` / `verified`: Only `enabled = true` profiles are eligible for USB `open()` and writes.
- `usage_page` / `interface_number`: Optional filters when a USB device exposes multiple HID collections.
- `report_id`, `report_length` (`64`), `power_bar_max_w`, `temp_unit` (`"C"` or `"F"`), `init_packets`, `shutdown_packets`.

### SK700V USB HID Wire Protocol (`VID = 0x381C`, `PID = 0x0003`)
All reports are **64 bytes** zero-padded, starting with HID Report ID `0x10`:
- **Init / Display ON** (9 bytes + zero pad): `10 68 01 09 02 03 01 78 16`
- **Display OFF / Shutdown** (9 bytes + zero pad): `10 68 01 09 02 03 00 77 16`
- **Telemetry Report** (20 bytes + 44 zero bytes):
  - `[0]` = `0x10` (Report ID)
  - `[1..=3]` = `[0x68, 0x01, 0x09]` (frame header)
  - `[4]` = `0x0D` (payload length = 13 bytes, covering `[5..=17]`)
  - `[5]` = `0x01` (telemetry command)
  - `[6]` = `0x02` (display active; `0x00` = instant off)
  - `[7..=8]` = `cpu_power_w` as big-endian `u16` (clamped `0..=999`)
  - `[9]` = `power_bar_pct` as `u8` (`0..=100`, scaled against `power_bar_max_w`)
  - `[10]` = temperature unit (`0x00` = Celsius, `0x01` = Fahrenheit)
  - `[11..=14]` = temperature as IEEE-754 big-endian `f32`
  - `[15]` = `cpu_usage_pct` as `u8` (clamped `0..=100`)
  - `[16..=17]` = `cpu_freq_mhz` as big-endian `u16`
  - `[18]` = checksum = 8-bit wrapping sum of bytes `[1..18]` (excludes Report ID byte `0`)
  - `[19]` = `0x16` (frame terminator)
  - `[20..=63]` = `0x00`

## Hardware Safety Guardrails

1. **Strict VID/PID Allowlisting:** Never open or write to any USB HID device unless `vendor_id == 0x381C` AND `product_id` matches an **enabled** `DeviceProfile` in the validated registry.
2. **Unverified PIDs Disabled:** MasterCraft's `index.jsc` also lists `0x381C` PIDs `0x0001`, `0x0002`, `0x0004`, and `0x0005` alongside models (`SK700V MACH`, `SK620V`, `PROTEUS`, `ENERGLO`). They are documented in `profiles/default_devices.toml` with `enabled = false` and `verified = false`—do not enable them until captured and verified on physical hardware.
3. **No Panics on I/O Errors:** USB unplugs, sleep/wake cycles, or transient HID write failures must return `Err(...)`, drop the stale `HidDevice`, log cleanly, and retry with backoff.
4. **No Proprietary Assets in Git:** Never copy or commit MasterCraft `.exe`, `.dll`, `.asar`, `.jsc`, `.node`, or raw captures into this repository.

## Go-to-Rust Conventions for Contributors

- **Synchronous by design:** Standard blocking threads + `std::thread::sleep` — no `tokio` or async runtimes.
- **Errors (`Result<T, E>` and `?`):** Functions return `Result` instead of Go's `(T, error)` tuple; `?` propagates errors upward like `if err != nil { return err }`. Use `thiserror` for structured domain errors (`config`, `protocol`) and `anyhow::Result` at application/driver boundaries.
- **Ownership & Borrowing:** Pass `&DeviceProfile` / `&[u8]` (read-only borrows) rather than cloning unless ownership needs to live inside a long-lived struct (such as `CoolerDevice`).
- **Traits vs Go Interfaces:** `SensorProvider` is implemented explicitly (`impl SensorProvider for WindowsPawnIoSensors`, `WindowsHwinfoSensors`, `LinuxSensors`), and the factory returns `Box<dyn SensorProvider>`.
- **RAII Cleanup (`Drop`):** OS handles (`PDH_HQUERY`, `HANDLE`, mapped views, `HidDevice`) clean up automatically when their owning struct goes out of scope (replacing Go's `defer`).
- **Keep It Simple:** Avoid unnecessary macros, lifetime annotations, or over-engineered abstractions.
