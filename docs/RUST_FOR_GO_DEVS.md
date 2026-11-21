# Reading `sudokooler` as a Go & Python Developer

> **Project Context & AI Disclaimer:** `sudokooler` is my first Rust project and was built with significant assistance from AI (Google Antigravity). I primarily guided the architecture, hardware reverse-engineering, module splits, and project direction. This guide documents how the Rust patterns used across `sudokooler` map back to Go and Python so the codebase is easy to read, review, and maintain.

---

## 1. `Option<T>`, `Result<T>`, and `?` (No `nil`, No Exceptions)

Rust has no `nil` pointers and no `try`/`except`. Instead, optional values and fallible calls are normal **enums** (tagged unions that can hold data inside a variant) automatically imported everywhere via Rust's standard prelude:

- **`Option<T>`** = `Some(value)` or `None` (replaces nullable pointers / Go `nil` / Python `None`).
- **`Result<T, E>`** = `Ok(value)` or `Err(error)` (replaces Go's `(T, error)` tuple). In `sudokooler`, we use `anyhow::Result<T>`, which boxes any error behind a single `Result<T>` type.

### Side-by-Side: Error Propagation

**Go**
```go
func loadRegistry(path string) (*DeviceRegistry, error) {
    raw, err := os.ReadFile(path)
    if err != nil {
        return nil, fmt.Errorf("failed to read config: %w", err)
    }
    var reg DeviceRegistry
    if err := toml.Unmarshal(raw, &reg); err != nil {
        return nil, err
    }
    if err := reg.Validate(); err != nil {
        return nil, err
    }
    return &reg, nil
}
```

**Rust** ([`src/config.rs`](../src/config.rs))
```rust
pub fn load_registry(path: &Path) -> Result<DeviceRegistry> {
    let raw = fs::read_to_string(path).context("failed to read config")?;
    let reg: DeviceRegistry = toml::from_str(&raw)?;
    reg.validate()?;
    Ok(reg) // No semicolon on the final expression = return value
}
```

- The `?` operator works like Go's `if err != nil { return ..., err }`.
- You'll also see `Result<Option<CoolerDevice>>` in [`src/hid/mod.rs`](../src/hid/mod.rs):
  - `Ok(Some(dev))` → USB scan succeeded and found a cooler.
  - `Ok(None)` → USB scan succeeded, no cooler currently plugged in (wait & retry).
  - `Err(err)` → OS/USB error.

---

## 2. Ownership, Borrowing (`&` / `&mut`), and `mut`

Variables in Rust are **immutable by default** (`let x = 5;` cannot be reassigned; `let mut x = 5;` opts into mutability).

Because Rust has no garbage collector, every value has a **single owner** and is freed when its owner goes out of scope:
- Passing a value directly **moves** ownership.
- **`&T`** borrows read-only (like passing a read-only pointer/slice in Go): e.g., `profile: &DeviceProfile` in [`src/hid/protocol/sk700v.rs`](../src/hid/protocol/sk700v.rs).
- **`&mut T`** borrows mutably: e.g., `fn read(&mut self)` in [`src/sensors/mod.rs`](../src/sensors/mod.rs) lets a sensor provider update its previous RAPL/CPU tick baselines.
- **`&[u8]` vs. `Vec<u8>`**: `&[u8]` is a borrowed byte slice (like Go's `[]byte` when read-only); `Vec<u8>` is an owned, heap-allocated growable byte buffer (like Python's `bytearray`).
- **`&str` vs. `String`**: `&str` is a borrowed string slice (string literals like `"sk700v"` have type `&'static str`, meaning they live for the entire program); `String` is an owned heap-allocated string.
- **`.clone()`**: Explicitly duplicates a value when a long-lived struct needs to own its own copy (such as `CoolerDevice` cloning `DeviceProfile`).

---

## 3. Structs, `impl`, Traits, and Factories (`Box<dyn Trait>`)

Rust separates data (`struct`) from methods (`impl`), and uses **`trait`** instead of Go's **`interface`** (implementations must be declared explicitly):

**Go**
```go
type DisplayProtocol interface {
    Name() string
    EncodeTelemetry(t Telemetry, p *DeviceProfile) ([][]byte, error)
}

type Sk700vProtocol struct{}

func (Sk700vProtocol) Name() string { return "sk700v" }

func ForProtocol(kind ProtocolKind) DisplayProtocol {
    switch kind {
    case ProtocolSk700v:
        return Sk700vProtocol{}
    }
    panic("unreachable")
}
```

**Rust** ([`src/hid/protocol/mod.rs`](../src/hid/protocol/mod.rs))
```rust
pub trait DisplayProtocol {
    fn name(&self) -> &'static str;
    fn encode_telemetry(&self, t: &Telemetry, p: &DeviceProfile) -> Result<Vec<Vec<u8>>>;
}

pub struct Sk700vProtocol;

impl DisplayProtocol for Sk700vProtocol {
    fn name(&self) -> &'static str { "sk700v" }
    // ...
}

pub fn for_protocol(kind: ProtocolKind) -> Box<dyn DisplayProtocol> {
    match kind {
        ProtocolKind::Sk700v => Box::new(Sk700vProtocol),
    }
}
```

- **`Box<dyn DisplayProtocol>`** is a heap-allocated **trait object**—conceptually the same as a Go interface value at runtime.
- Unlike Go's `switch`, Rust's `match` is **exhaustiveness-checked** at compile time.

---

## 4. Pattern Matching: `match`, `if let`, and `let ... else`

When unpacking an `Option` or `Result` without `?`:

- **`let ... else`** (early return guard, like `if x == nil { return ... }`):
  ```rust
  // src/hid/mod.rs
  let Some((info, profile)) = candidates.into_iter().next() else {
      return Ok(None);
  };
  ```
- **`if let`** (run a block only when a pattern matches):
  ```rust
  // src/main.rs
  if let Some(dev) = active_device.as_ref()
      && let Err(err) = dev.send_telemetry(&telemetry)
  {
      // drop stale handle and reconnect...
  }
  ```

---

## 5. Deterministic Cleanup (`Drop` vs. `defer` / `with`)

Go's `defer` only cleans up when a *function* returns; Python's `with` cleans up at the end of a *block*. In Rust, implementing `Drop` cleans up an OS resource automatically whenever the struct leaves scope or is replaced/dropped—even if held inside a long-lived struct across many function calls:

**Go**
```go
handle, err := openPawnIO()
if err != nil { return err }
defer windows.CloseHandle(handle)
```

**Rust** ([`src/sensors/windows_pdh.rs`](../src/sensors/windows_pdh.rs))
```rust
pub(super) struct OwnedHandle(pub HANDLE); // Tuple struct: inner field accessed as self.0

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe { let _ = CloseHandle(self.0); }
        }
    }
}
```

This RAII pattern ensures `OwnedHandle`, `PdhQuery`, `MappedView`, `PciMutexGuard`, and `hidapi::HidDevice` never leak handles on early returns, errors, or USB disconnects.

---

## 6. Attributes (`#[...]`) and Macros (`name!`)

- **Macros** always end with `!` and expand at compile time:
  - `println!(...)`, `format!(...)`, `vec![...]`
  - `bail!("...")` (from `anyhow`, equivalent to `return Err(anyhow!("..."))`)
  - `include_str!("../profiles/default_devices.toml")` (embeds the TOML file into the binary at compile time)
- **Attributes (`#[...]`)** attach metadata (like Python decorators or Go build tags):
  - `#[derive(Debug, Clone, Serialize, Deserialize)]` — auto-generates debug printing, cloning, and TOML parsing.
  - `#[cfg(windows)]` / `#[cfg(target_os = "linux")]` — conditional compilation (like `//go:build windows`).
  - `#[cfg(test)]` / `#[test]` — unit test blocks compiled only during `cargo test`.

---

## 7. Modules, Visibility, and Paths

- `mod foo;` in a parent file compiles `foo.rs` (or `foo/mod.rs`) as a child module.
- Everything is **private by default** (unlike Go's uppercase export rule):
  - `pub` — visible outside the module.
  - `pub(super)` — visible only to the parent module (e.g., `OwnedHandle` is shared across `src/sensors/` but hidden from the rest of the crate).
  - `pub(crate)` — visible anywhere inside `sudokooler`.
- Path prefixes: `crate::` starts from `src/main.rs`, `super::` is the parent module (`..`), `self::` is the current module.

---

## 8. Iterators, Closures, `Arc<AtomicBool>`, and `unsafe`

- **Iterators & Closures (`|arg| body` ≈ Python `lambda arg: body`):**
  ```rust
  // src/hid/protocol/sk700v.rs
  frame.get(1..18)
      .unwrap_or_default()
      .iter()
      .fold(0u8, |acc, &b| acc.wrapping_add(b))
  ```
  Debug Rust builds panic on integer overflow, so `.wrapping_add` explicitly opts into Go-style modulo-256 checksum math.
- **`Arc<AtomicBool>` (`src/main.rs`):** `Arc` (*Atomically Reference Counted*) allows the main loop and `Ctrl+C` OS signal callback thread to share one atomic boolean flag without a mutex. `move || { ... }` transfers ownership of the cloned `Arc` into the callback.
- **`unsafe { ... }`:** Required only for operations the compiler can't verify statically (calling C/Win32 APIs like `CreateFileW` or `DeviceIoControl`, or creating a slice from a mapped shared-memory pointer). Every `unsafe` block in `sudokooler` is annotated with a `// SAFETY:` comment explaining why the call is sound.

---

## Quick Reference Cheat Sheet

| Go / Python Concept | Rust Equivalent in `sudokooler` |
| --- | --- |
| Go `var x` / Python variable | `let mut x` (`let x` is immutable by default) |
| Go `nil` / Python `None` | `Option<T>` (`Some(v)` / `None`) |
| Go `(T, error)` / `if err != nil` | `anyhow::Result<T>` (`Ok(v)` / `Err(e)`) + `?` |
| Go `[]byte` / Python `bytearray` | `&[u8]` (borrowed) / `Vec<u8>` (owned) |
| Go `string` (read-only vs. owned) | `&str` (borrowed) / `String` (owned) |
| Go `interface` + constructor | `trait` + `Box<dyn Trait>` factory |
| Go `defer` / Python `with` | `impl Drop for MyType` (RAII) |
| Go `//go:build windows` | `#[cfg(windows)]` |
| Go `fmt.Errorf("...: %w", err)` | `.context("...")?` |
| Go `atomic.Bool` across goroutines | `Arc<AtomicBool>` across threads |
