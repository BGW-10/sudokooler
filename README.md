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
On Windows, `sudokooler` reads CPU usage and effective clock speed natively via **Win32 PDH** (`Processor Information(_Total)`). Because Windows provides no built-in user-mode API for CPU die temperatures or package wattage on desktop CPUs, `sudokooler` supports **two options** for temperature and power telemetry (automatically choosing PawnIO if available, or HWiNFO64 otherwise):

#### Option 1: PawnIO (Recommended — Standalone, No Background App)
Ideal if you want `sudokooler` to run completely standalone (~2–4 MB RAM, zero background tray apps, and no 12-hour shared-memory timeout).

**Self-Contained Bundle & GitHub Releases (`packaging/bundle-windows.ps1`):**
You can download `sudokooler-windows-x86_64.zip` from GitHub Releases (built automatically on `v*` tags via [.github/workflows/release.yml](.github/workflows/release.yml)) or build the exact same offline bundle locally:
```powershell
# Build release binary + bundle PawnIO_setup.exe & AMDFamily17.bin into dist\sudokooler-windows-x86_64.zip
powershell -ExecutionPolicy Bypass -File .\packaging\bundle-windows.ps1
```
Then extract `sudokooler-windows-x86_64.zip` and run from an **elevated (Administrator)** PowerShell prompt:
```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1
```
This installs PawnIO (if missing), copies `sudokooler.exe` + `pawnio\AMDFamily17.bin` into `C:\Program Files\sudokooler`, and registers/starts a silent background Scheduled Task (`SYSTEM` at startup, no terminal window; remove anytime with `powershell -ExecutionPolicy Bypass -File .\uninstall.ps1`).

**Manual Setup (AMD Ryzen):**
1. Install the signed **PawnIO** kernel driver from [pawnio.eu](https://pawnio.eu/).
2. Download `AMDFamily17.bin` from [namazso/PawnIO.Modules releases](https://github.com/namazso/PawnIO.Modules/releases) (reads Ryzen Zen 1–5 `Tctl/Tdie` temp and RAPL package wattage).
3. Place `AMDFamily17.bin` in one of the checked locations:
   - `C:\Program Files\PawnIO\modules\AMDFamily17.bin`
   - `<exe_dir>\pawnio\AMDFamily17.bin`
   - Or set `SUDOKOOLER_PAWNIO_MODULE` to the `.bin` path or directory.
4. Run `sudokooler.exe` as Administrator (or via a Task Scheduler logon task with **Run with highest privileges** enabled), since Windows requires elevation to open `\\?\GLOBALROOT\Device\PawnIO`.

#### Option 2: HWiNFO64 Shared Memory (No PawnIO or Elevation Required)
Fully supported if you already run **HWiNFO64** or prefer not to install PawnIO or run `sudokooler` elevated:
1. Install and launch **HWiNFO64** in **Sensors-only** mode.
2. Open **Settings** (gear icon) → **General / User Interface**, and enable **Shared Memory Support**.
   - *(Optional)* Enable **Auto Start** and **Minimize Sensors on Startup** (or **Minimize Main Window**) so HWiNFO stays in the system tray.
3. Run `sudokooler.exe` normally (no Administrator elevation required):
   ```powershell
   .\target\release\sudokooler.exe --dry-run --count 3
   .\target\release\sudokooler.exe
   ```
   When PawnIO is absent, `sudokooler` automatically connects to `Global\HWiNFO_SENS_SM2` for CPU temperature (`CPU (Tctl/Tdie)` / `CPU Package`) and package wattage (`CPU Package Power` / `CPU PPT`).
4. **Note:** HWiNFO64 **Free** automatically disables Shared Memory Support after **12 hours** per session (HWiNFO Pro removes this limit).

> **Without PawnIO or HWiNFO64:** `sudokooler` still runs cleanly using Win32 PDH for live CPU usage and frequency, reporting `0.0°C` / `0W` for temperature and power.

## Python Protocol Prototype

A cleaned-up, standalone PEP 723 script ([python/sk700v_prototype.py](python/sk700v_prototype.py)) is included as a self-contained SK700V protocol reference and USB diagnostic tool. It requires no manual virtualenv setup when run with [`uv`](https://docs.astral.sh/uv/):

```bash
# Verify byte-for-byte parity with captured SK700V USB HID frames (also works with plain python3)
uv run python/sk700v_prototype.py --self-test

# Enumerate connected 0x381C HID interfaces
uv run python/sk700v_prototype.py --list

# Print encoded 64-byte sample packets without writing to USB
uv run python/sk700v_prototype.py --dry-run --count 3

# Decode a captured SK700V telemetry frame
uv run python/sk700v_prototype.py --decode 106801090d010200281900423c00001013a10516

# Send live sample frames to the SK700V (Ctrl+C sends Display OFF on exit)
uv run python/sk700v_prototype.py --send
uv run python/sk700v_prototype.py --off
```

## Documentation

- [Architecture Guide](docs/ARCHITECTURE.md) — data flow, module map, safety guardrails, and Go-to-Rust quick reference
- [Adding a Protocol or Sensor Backend](docs/ADDING_A_PROTOCOL.md) — step-by-step expansion guide
- [Rust for Go & Python Developers](docs/RUST_FOR_GO_DEVS.md) — walkthrough of the Rust syntax and idioms used in this repo

## AI Assistance Disclosure

This repository was bootstrapped and developed with assistance from **Google Antigravity** (agentic AI coding assistant), based on reverse-engineered SK700V USB HID captures.

## License

MIT
