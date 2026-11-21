# Adding a New Cooler Protocol or Sensor Backend

This guide walks through adding support for a new Sudokoo USB HID cooler model (or adding a new CPU telemetry backend).

---

## Part 1: Supporting a New Sudokoo Cooler

### 1. Identify VID, PID, and HID Interface
Plug in the cooler and inspect its USB HID descriptors:
```bash
cargo run -- --list-devices
```
Confirm `VID == 0x381C` and note the `PID`, `iface`, `usage_page`, and `usage`.

### 2. Capture & Compare Packets
Capture USB traffic (for example with Wireshark + USBPcap on Windows or `usbmon` on Linux) while the vendor app turns the display ON, updates telemetry, and turns the display OFF.

- **If the cooler uses the exact SK700V packet format** (`10 68 01 09 ... 16`, 64 bytes):
  You do **not** need a new Rust protocol module. Simply add a `[[devices]]` entry in [profiles/default_devices.toml](../profiles/default_devices.toml) with `protocol = "sk700v"` and the new `pid`, `usage_page` / `interface_number` (if multi-interface), and `power_bar_max_w`.
- **If the cooler uses a different packet format**, follow Steps 3–6 below.

### 3. Add a Protocol Module (`src/hid/protocol/<model>.rs`)
Create a new file implementing [`DisplayProtocol`](../src/hid/protocol/mod.rs):

```rust
use anyhow::{Result, bail};

use crate::config::DeviceProfile;
use crate::sensors::Telemetry;

use super::DisplayProtocol;

pub struct MyCoolerProtocol;

impl DisplayProtocol for MyCoolerProtocol {
    fn name(&self) -> &'static str {
        "mycooler"
    }

    fn validate_profile(&self, profile: &DeviceProfile) -> Result<()> {
        if profile.report_length != 64 {
            bail!("invalid profile '{}': expected 64-byte reports", profile.name);
        }
        Ok(())
    }

    fn encode_telemetry(
        &self,
        telemetry: &Telemetry,
        profile: &DeviceProfile,
    ) -> Result<Vec<Vec<u8>>> {
        self.validate_profile(profile)?;
        let mut pkt = vec![0u8; profile.report_length];
        pkt[0] = profile.report_id;
        // Fill protocol-specific payload + checksum bytes...
        // Multi-packet protocols (e.g., LCD frames) can push multiple Vec<u8> chunks here.
        Ok(vec![pkt])
    }
}
```

### 4. Register the Variant in `ProtocolKind` and `for_protocol()`
1. Export the new module in [src/hid/protocol/mod.rs](../src/hid/protocol/mod.rs):
   ```rust
   pub mod mycooler;
   pub mod sk700v;
   ```
2. Add the enum variant in [src/config.rs](../src/config.rs):
   ```rust
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
   #[serde(rename_all = "lowercase")]
   pub enum ProtocolKind {
       Sk700v,
       Mycooler,
   }
   ```
3. Wire it into `for_protocol()` in [src/hid/protocol/mod.rs](../src/hid/protocol/mod.rs):
   ```rust
   pub fn for_protocol(kind: ProtocolKind) -> Box<dyn DisplayProtocol> {
       match kind {
           ProtocolKind::Sk700v => Box::new(sk700v::Sk700vProtocol),
           ProtocolKind::Mycooler => Box::new(mycooler::MyCoolerProtocol),
       }
   }
   ```
   Rust enforces exhaustive matching—once `ProtocolKind::Mycooler` is added, the compiler verifies both `for_protocol()` and every call site handle it. `DeviceRegistry::validate()`, `CoolerDevice`, and `--dry-run` all dispatch through `for_protocol()`, so no changes to `src/main.rs` or `src/hid/mod.rs` are needed.

### 5. Add the Device Profile & Unit Tests
1. Add a `[[devices]]` entry to [profiles/default_devices.toml](../profiles/default_devices.toml) (or test first with `--config path/to/test.toml`).
2. Add unit tests inside `src/hid/protocol/<model>.rs` asserting **byte-for-byte parity** against real captured packets (including init/shutdown packets and edge values).

### 6. Verify Before Enabling Globally
```bash
# 1. Run unit tests and doc checks
cargo test
cargo check --all-targets

# 2. Verify packet output safely without writing to USB
cargo run -- --config path/to/test.toml --dry-run --count 3

# 3. Test live on physical hardware
cargo run -- --config path/to/test.toml --count 5
```

---

## Part 2: Adding a New `SensorProvider` Backend

1. Create a module under `src/sensors/` implementing `SensorProvider`:
   ```rust
   impl SensorProvider for MySensors {
       fn read(&mut self) -> anyhow::Result<Telemetry> { ... }
       fn backend_name(&self) -> &'static str { "my-backend" }
   }
   ```
2. Declare the module (with appropriate `#[cfg(...)]` gating) in [src/sensors/mod.rs](../src/sensors/mod.rs).
3. Wire it into `default_provider()` in [src/sensors/mod.rs](../src/sensors/mod.rs).
