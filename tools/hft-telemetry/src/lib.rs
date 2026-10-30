// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! `pms-telemetry`: scheduling-pressure probes for adaptive wait strategies.
//!
//! A `RunqueueProbe` observes host-level scheduling pressure directly rather
//! than inferring it from elapsed time. `PsiProbe` reads `/proc/pressure/cpu`
//! on Linux 4.20 and later, cheap and coarse, and `NoopProbe` reports none
//! where PSI is missing. An `AdaptiveWaitStrategy` runs its base strategy
//! while pressure is below a threshold and falls through to `Park` to
//! release the core when it is not.
//!
//! In the book: <https://pulse-mesh.net/docs/daemon/architecture/peripheral.html>.

use hft_hub::functions::wait::{Park, WaitResult, WaitStrategy};
#[cfg(feature = "psi")]
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(feature = "psi")]
use std::sync::{mpsc, Arc};
#[cfg(feature = "psi")]
use std::time::Duration;
use thiserror::Error;

/// Errors raised by [`PsiProbe`] construction.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// `/proc/pressure/cpu` not available on this system.
    /// Either the kernel is older than 4.20, or PSI was disabled at boot.
    #[error("/proc/pressure/cpu unavailable — kernel < 4.20 or PSI disabled at boot")]
    PsiUnavailable,
    /// Underlying I/O failure (typically while reading `/proc/pressure/cpu`).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Source of host-level scheduling pressure information.
pub trait RunqueueProbe: Send + Sync {
    /// Pressure as a percentage 0.0 - 100.0. Higher = more contention.
    fn pressure_percent(&self) -> f32;

    /// Optional per-CPU runqueue depth. Default = uniform pressure_percent across CPUs.
    fn current_depth(&self, _cpu: u32) -> u32 {
        (self.pressure_percent() / 10.0) as u32
    }

    /// Short identifier for the probe backend (`"psi"`, `"noop"`). Used for logging and metric labeling.
    fn name(&self) -> &'static str;
}

/// The PSI file the probe reads.
#[cfg(feature = "psi")]
const PSI_CPU: &str = "/proc/pressure/cpu";

/// The shortest refresh interval [`PsiProbe::new`] keeps; the kernel updates
/// PSI every two seconds, so reading faster only wastes work.
#[cfg(feature = "psi")]
pub const MIN_PSI_REFRESH: Duration = Duration::from_millis(10);

/// PSI-backed probe. A thread of the probe's own reads `/proc/pressure/cpu`
/// every refresh interval (100 ms from [`detect_best_probe`]) and keeps the
/// latest `avg10`, so [`RunqueueProbe::pressure_percent`] is one atomic load
/// and a wait never touches the file system. Dropping the probe stops and
/// joins the thread.
#[cfg(feature = "psi")]
pub struct PsiProbe {
    /// The latest `avg10`, as `f32` bits.
    pressure: Arc<AtomicU32>,
    /// Dropped to stop the refresher.
    stop: Option<mpsc::Sender<()>>,
    refresher: Option<std::thread::JoinHandle<()>>,
}

#[cfg(feature = "psi")]
impl PsiProbe {
    /// A PSI probe whose thread refreshes it every `refresh_interval`, at
    /// least [`MIN_PSI_REFRESH`].
    ///
    /// # Errors
    ///
    /// Returns [`TelemetryError::PsiUnavailable`] if `/proc/pressure/cpu`
    /// is not present (kernel < 4.20 or PSI disabled at boot), and
    /// [`TelemetryError::Io`] if the refresher thread cannot be started.
    pub fn new(refresh_interval: Duration) -> Result<Self, TelemetryError> {
        if !std::path::Path::new(PSI_CPU).exists() {
            return Err(TelemetryError::PsiUnavailable);
        }
        let pressure = Arc::new(AtomicU32::new(read_avg10().unwrap_or(0.0).to_bits()));
        let (stop, stopped) = mpsc::channel::<()>();
        let latest = Arc::clone(&pressure);
        let interval = refresh_interval.max(MIN_PSI_REFRESH);
        let refresher = std::thread::Builder::new()
            .name("pms-psi".into())
            .spawn(move || {
                while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(interval) {
                    if let Some(avg10) = read_avg10() {
                        latest.store(avg10.to_bits(), Ordering::Relaxed);
                    }
                }
            })?;
        Ok(Self {
            pressure,
            stop: Some(stop),
            refresher: Some(refresher),
        })
    }
}

#[cfg(feature = "psi")]
impl Drop for PsiProbe {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(refresher) = self.refresher.take() {
            let _ = refresher.join();
        }
    }
}

/// The `avg10` of the `some` line of `/proc/pressure/cpu`, `None` if the
/// file cannot be read or holds no such value.
#[cfg(feature = "psi")]
fn read_avg10() -> Option<f32> {
    parse_avg10(&std::fs::read_to_string(PSI_CPU).ok()?)
}

/// The `avg10` of the `some` line of a PSI file's text, whose lines read
/// `some avg10=0.00 avg60=0.00 avg300=0.00 total=N`: the most responsive of
/// the smoothed values.
#[cfg(feature = "psi")]
fn parse_avg10(text: &str) -> Option<f32> {
    text.lines()
        .find(|line| line.starts_with("some"))?
        .split_whitespace()
        .find_map(|token| token.strip_prefix("avg10="))?
        .parse()
        .ok()
}

#[cfg(feature = "psi")]
impl RunqueueProbe for PsiProbe {
    fn pressure_percent(&self) -> f32 {
        f32::from_bits(self.pressure.load(Ordering::Relaxed))
    }
    fn name(&self) -> &'static str {
        "psi"
    }
}

/// The best probe this host allows: [`PsiProbe`] where `/proc/pressure/cpu`
/// exists, otherwise [`NoopProbe`].
pub fn detect_best_probe() -> Box<dyn RunqueueProbe> {
    #[cfg(feature = "psi")]
    {
        if let Ok(p) = PsiProbe::new(Duration::from_millis(100)) {
            return Box::new(p);
        }
    }
    Box::new(NoopProbe)
}

/// Always reports 0% pressure. Useful when telemetry isn't available — the adaptive
/// strategy degrades to its base strategy with no behavioural change.
pub struct NoopProbe;
impl RunqueueProbe for NoopProbe {
    fn pressure_percent(&self) -> f32 {
        0.0
    }
    fn name(&self) -> &'static str {
        "noop"
    }
}

/// Wraps a base WaitStrategy with a runqueue probe. When pressure exceeds
/// `pressure_threshold`, falls through to `fallback` (typically `Park`) to release
/// the core for genuinely contending work.
pub struct AdaptiveWaitStrategy<W: WaitStrategy, P: RunqueueProbe> {
    /// Strategy to use under normal conditions (typically `BusySpin` or `UMWAIT`).
    pub base: W,
    /// Run-queue pressure probe: [`PsiProbe`], or [`NoopProbe`] where PSI
    /// is missing.
    pub probe: P,
    /// Pressure percent (0–100) above which the strategy falls through
    /// to the lower-power `fallback` instead of `base`. 80.0 is a
    /// reasonable starting point.
    pub pressure_threshold: f32,
    /// Lower-power fallback strategy (typically `Park` to release the core).
    pub fallback: Park,
}

impl<W: WaitStrategy, P: RunqueueProbe> WaitStrategy for AdaptiveWaitStrategy<W, P> {
    unsafe fn wait(&self, monitor_addr: *const u8, expected: u64) -> WaitResult {
        if self.probe.pressure_percent() >= self.pressure_threshold {
            // SAFETY: caller of the outer `wait` upholds the WaitStrategy::wait
            // contract — `monitor_addr` is a valid 8-byte-aligned pointer that
            // remains live for the duration of the call. We forward both
            // pointer and `expected` value unchanged to the fallback strategy,
            // whose `wait` carries the identical contract.
            unsafe { self.fallback.wait(monitor_addr, expected) }
        } else {
            // SAFETY: same invariant as the fallback branch above —
            // `monitor_addr` / `expected` forwarded unchanged to a sibling
            // WaitStrategy whose `wait` carries the identical contract.
            unsafe { self.base.wait(monitor_addr, expected) }
        }
    }
    fn name(&self) -> &'static str {
        "adaptive"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_probe_reports_zero() {
        let p = NoopProbe;
        assert_eq!(p.pressure_percent(), 0.0);
    }

    #[cfg(feature = "psi")]
    #[test]
    fn psi_probe_returns_finite_pressure_when_available() {
        if let Ok(probe) = PsiProbe::new(Duration::from_millis(100)) {
            let p = probe.pressure_percent();
            assert!(p.is_finite());
            assert!(p >= 0.0);
        }
    }

    #[cfg(feature = "psi")]
    #[test]
    fn avg10_is_read_from_the_some_line() {
        let text = "some avg10=1.50 avg60=0.25 avg300=0.00 total=42\nfull avg10=9.00 avg60=0.00 avg300=0.00 total=7\n";
        assert_eq!(parse_avg10(text), Some(1.5));
        assert_eq!(parse_avg10("full avg10=9.00\n"), None);
        assert_eq!(parse_avg10("some avg10=x\n"), None);
    }

    /// Dropping a probe stops its refresher at once, not after an interval.
    #[cfg(feature = "psi")]
    #[test]
    fn dropping_a_probe_stops_its_refresher() {
        if let Ok(probe) = PsiProbe::new(Duration::from_secs(3600)) {
            let started = std::time::Instant::now();
            drop(probe);
            assert!(started.elapsed() < Duration::from_secs(5));
        }
    }
}
