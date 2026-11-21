# Architecture Guide

`sudokooler` is a synchronous, cross-platform Rust daemon that samples host CPU telemetry and streams USB HID display reports to Sudokoo CPU coolers.

## Data Flow

```text
+-----------------------+     Telemetry     +-----------------------+     Vec<Vec<u8>>     +----------------+
|  SensorProvider       | ----------------> |  DisplayProtocol      | -----------------> |  CoolerDevice  |
|  (src/sensors/)       |                   |  (src/hid/protocol/)  |                    |  (src/hid/)    |
+-----------------------+                   +-----------------------+                    +----------------+
  Windows:                                    Factory: for_protocol()                      hidapi write()
   1. WindowsPawnIoSensors (primary)           - Sk700vProtocol                             VID == 0x381C
   2. WindowsHwinfoSensors (fallback)          - <future models>                            enabled PID only
  Linux:
   - LinuxSensors (/sys + /proc)
```

## Module Map

| Path | Responsibility |
| --- | --- |
| `src/main.rs` | `clap` CLI (`--list-devices`, `--dry-run`, `--config`, `--interval-ms`, `--count`), interruptible 1 Hz polling loop, `Ctrl+C` shutdown |
| `src/config.rs` | TOML `DeviceRegistry` / `DeviceProfile` loader (embedded `profiles/default_devices.toml` + `--config` override) and USB safety validator |
| `src/hid/mod.rs` | `hidapi` interface enumeration (`list_sudokoo_devices`) and `CoolerDevice` lifecycle (`open_first`, `send_init`, `send_telemetry`, `send_shutdown`) |
| `src/hid/protocol/mod.rs` | `DisplayProtocol` trait, `for_protocol()` factory, and shared framing helpers (`pad_report`, `decode_hex_packet`, `format_hex`) |
| `src/hid/protocol/sk700v.rs` | `Sk700vProtocol` encoder, checksum calculator (`compute_checksum`), profile validation, and byte-for-byte capture unit tests |
| `src/sensors/mod.rs` | `Telemetry` data contract, `SensorProvider` trait, and `default_provider()` OS factory |
| `src/sensors/windows_pdh.rs` | Shared Win32 PDH counter wrapper (`PdhCpuCounters`) and RAII `OwnedHandle` |
| `src/sensors/windows_pawnio.rs` | Standalone Windows AMD Ryzen provider (`WindowsPawnIoSensors`) via `\\?\GLOBALROOT\Device\PawnIO` + `AMDFamily17.bin` |
| `src/sensors/windows_hwinfo.rs` | Fallback Windows provider (`WindowsHwinfoSensors`) reading `Global\HWiNFO_SENS_SM2` shared memory |
| `src/sensors/linux.rs` | Zero-dependency Linux provider (`LinuxSensors`) reading `/sys/class/hwmon`, `powercap`, `/proc/stat`, and `cpufreq` |
| `profiles/default_devices.toml` | Verified allowlisted cooler profiles |
| `python/sk700v_prototype.py` | Standalone PEP 723 (`uv run`) SK700V reference implementation and diagnostic/decoder CLI |

## Core Design Principles

1. **Dual Trait + Factory Abstractions:**
   - **Sensors (`SensorProvider` + `default_provider()`):** Each platform backend owns OS handles and polling state (`PDH_HQUERY`, PawnIO `HANDLE`, `/proc/stat` tick counters). On Windows, `default_provider()` tries `WindowsPawnIoSensors::try_new()` and falls back automatically to `WindowsHwinfoSensors`.
   - **Protocols (`DisplayProtocol` + `for_protocol()`):** Each cooler protocol validates its `DeviceProfile` and encodes a `Telemetry` sample into `Vec<Vec<u8>>` HID reports. Returning a `Vec` of reports allows single-frame segmented displays (`SK700V`) and future multi-packet LCD transfers to share one interface.
2. **Hardware Safety Allowlisting:**
   - `DeviceRegistry::validate()` rejects any profile whose `vid != 0x381C` or whose hex control frames fail validation.
   - `CoolerDevice::open_first()` only opens devices matching `VID == 0x381C` and an `enabled = true` profile.
3. **Synchronous & Reconnect-Safe:**
   - No `tokio` or async runtime—just a 1 Hz blocking loop with `sleep_interruptible` (100 ms granularity for fast `Ctrl+C` exit).
   - USB write failures drop `CoolerDevice` (closing the OS handle via RAII `Drop`) and retry every 2 seconds without panicking.

## Go-to-Rust Quick Reference

| Go Concept | Rust Equivalent in `sudokooler` |
| --- | --- |
| Interface + constructor | `trait SensorProvider` / `trait DisplayProtocol` + `Box<dyn ...>` factory |
| `(T, error)` + `if err != nil` | `anyhow::Result<T>` + `?` propagation |
| `defer handle.Close()` | `impl Drop for OwnedHandle` / `PdhQuery` / `HidDevice` (RAII) |
| Read-only pointer / slice arg | Borrowed reference (`&DeviceProfile`, `&[u8]`) |
| `atomic.Bool` + signal channel | `Arc<AtomicBool>` shared with `ctrlc::set_handler` |
