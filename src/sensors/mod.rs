use anyhow::Result;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows_hwinfo;
#[cfg(windows)]
mod windows_pawnio;
#[cfg(windows)]
mod windows_pdh;

/// Cross-platform CPU telemetry snapshot passed to protocol encoders.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Telemetry {
    pub cpu_temp_c: f32,
    pub cpu_usage_pct: u8,
    pub cpu_power_w: u16,
    pub cpu_freq_mhz: u16,
}

/// Hardware telemetry backend interface.
///
/// Go note: Rust traits work like Go interfaces, except types explicitly declare
/// `impl SensorProvider for MyType` rather than satisfying interfaces structurally.
pub trait SensorProvider {
    fn read(&mut self) -> Result<Telemetry>;
    fn backend_name(&self) -> &'static str;
}

/// Factory returning the platform-native sensor provider (`Box<dyn SensorProvider>` is a trait
/// object, analogous to returning a Go interface value backed by a concrete struct).
///
/// On Windows, prefers standalone PawnIO (`WindowsPawnIoSensors`) and automatically falls
/// back to `WindowsHwinfoSensors` if the PawnIO driver or module is unavailable.
pub fn default_provider() -> Result<Box<dyn SensorProvider>> {
    #[cfg(windows)]
    {
        match windows_pawnio::WindowsPawnIoSensors::try_new() {
            Ok(pawnio) => Ok(Box::new(pawnio)),
            Err(reason) => Ok(Box::new(windows_hwinfo::WindowsHwinfoSensors::new(Some(
                reason,
            ))?)),
        }
    }

    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::LinuxSensors::new()?))
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    {
        anyhow::bail!("no sensor backend implemented for this operating system")
    }
}
