// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! Ring sizing driven by observed receiver lag: a [`LagObserver`] watches the
//! lag between a ring's write and read cursors and decides when the next ring
//! should grow or shrink, within [`AutoscaleConfig`]'s bounds, and a
//! [`ResizingRing`] applies the decision by replacing its ring with one of
//! the new size under the same name, which a [`FollowingReader`] follows.
//!
//! The swap rests on the ring's superseded mark ([`PulseRing::supersede`],
//! ADR-0015): the writer marks the old ring, removes its name and creates the
//! new ring under it. A reader still mapped to the old ring reads what it
//! holds, sees the mark, and reopens the name. Nothing the old ring held is
//! taken from a reader, so a reader that keeps up, or a writer that publishes
//! with [`PulseRing::publish_blocking`] to one reader, loses nothing across a
//! swap (`tests::a_reader_follows_the_ring_across_resizes_and_loses_nothing`).

use std::time::{Duration, Instant};

use core_ring::{PositionRead, PulseRing, RingError};
use pulse_canon::Pulse;
use thiserror::Error;

/// Errors of the autoscale stage.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum AutoscaleError {
    /// A ring could not be created, opened or unlinked.
    #[error("ring: {0}")]
    Ring(#[from] RingError),
    /// A resize would leave the configured `[min, max]` capacity range.
    #[error("invalid resize: requested {requested} outside [{min}, {max}]")]
    InvalidResize {
        /// The capacity the resize would give, in slots.
        requested: u64,
        /// Lower bound on ring capacity (`AutoscaleConfig::min_capacity`).
        min: u64,
        /// Upper bound on ring capacity (`AutoscaleConfig::max_capacity`).
        max: u64,
    },
}

/// What [`LagObserver::observe`] decides for the next ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeDecision {
    /// Keep the current ring size.
    Hold,
    /// Halve the next ring.
    Shrink,
    /// Double the next ring.
    Grow,
    /// Quadruple the next ring, without waiting for the window to close.
    EmergencyGrow,
}

/// Configuration for a [`LagObserver`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AutoscaleConfig {
    /// Minimum allowed ring capacity (slots). Default 1024.
    pub min_capacity: u64,
    /// Maximum allowed ring capacity (slots). Default 16M.
    pub max_capacity: u64,
    /// Lag fraction below which the ring shrinks (default 0.25).
    pub shrink_threshold: f64,
    /// Lag fraction at or above which the ring grows (default 0.75).
    pub grow_threshold: f64,
    /// How long each observation window lasts (default 10 s).
    pub observation_window: Duration,
}

impl Default for AutoscaleConfig {
    fn default() -> Self {
        Self {
            min_capacity: 1024,
            max_capacity: 16 * 1024 * 1024,
            shrink_threshold: 0.25,
            grow_threshold: 0.75,
            observation_window: Duration::from_secs(10),
        }
    }
}

/// Turns observations of a ring's lag, its write cursor less its read cursor,
/// into [`ResizeDecision`]s; poll it periodically with the ring's cursors.
pub struct LagObserver {
    cfg: AutoscaleConfig,
    capacity: u64,
    /// Low-lag windows since the last mid-range window or resize.
    low_lag_streak: u32,
    /// When the current window opened.
    last_decision: Instant,
    /// The peak lag fraction in the current window.
    peak_lag_pct: f64,
}

impl LagObserver {
    /// An observer of a ring of `current_capacity` slots; its first window
    /// opens now.
    pub fn new(cfg: AutoscaleConfig, current_capacity: u64) -> Self {
        Self {
            cfg,
            capacity: current_capacity,
            low_lag_streak: 0,
            last_decision: Instant::now(),
            peak_lag_pct: 0.0,
        }
    }

    /// Record one observation of the ring's cursors and decide.
    ///
    /// `EmergencyGrow` comes at once when the lag reaches the capacity.
    /// Otherwise the decision is `Hold` until the window closes, and then
    /// `Grow` if the window's peak lag reached `grow_threshold`, or `Shrink`
    /// from the second window below `shrink_threshold` since the last
    /// mid-range window or resize. A resize that would leave
    /// `[min_capacity, max_capacity]` is not decided. The `observer_*` tests
    /// check each of the four decisions.
    pub fn observe(&mut self, write_cursor: u64, read_cursor: u64) -> ResizeDecision {
        let lag = write_cursor.saturating_sub(read_cursor);
        let lag_pct = lag as f64 / self.capacity as f64;
        if lag_pct > self.peak_lag_pct {
            self.peak_lag_pct = lag_pct;
        }

        if lag_pct >= 1.0 && self.capacity * 4 <= self.cfg.max_capacity {
            self.reset();
            return ResizeDecision::EmergencyGrow;
        }

        if self.last_decision.elapsed() < self.cfg.observation_window {
            return ResizeDecision::Hold;
        }

        let decision = if self.peak_lag_pct >= self.cfg.grow_threshold {
            if self.capacity * 2 <= self.cfg.max_capacity {
                ResizeDecision::Grow
            } else {
                ResizeDecision::Hold
            }
        } else if self.peak_lag_pct < self.cfg.shrink_threshold {
            self.low_lag_streak += 1;
            // Two low windows, not one, so that the size does not flap.
            if self.low_lag_streak >= 2 && self.capacity / 2 >= self.cfg.min_capacity {
                ResizeDecision::Shrink
            } else {
                ResizeDecision::Hold
            }
        } else {
            self.low_lag_streak = 0;
            ResizeDecision::Hold
        };

        self.reset();
        decision
    }

    fn reset(&mut self) {
        self.last_decision = Instant::now();
        self.peak_lag_pct = 0.0;
        // Not `low_lag_streak`, which counts across windows.
    }

    /// The ring's capacity in slots, as constructed or last applied.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Record that the ring was resized as `decision` says, once the swap has
    /// completed, and return the new capacity.
    ///
    /// # Errors
    ///
    /// [`AutoscaleError::InvalidResize`] when the new capacity would leave
    /// `[min_capacity, max_capacity]`; the capacity is then unchanged.
    pub fn apply(&mut self, decision: ResizeDecision) -> Result<u64, AutoscaleError> {
        let new_cap = self.resized(decision)?;
        self.capacity = new_cap;
        self.low_lag_streak = 0;
        Ok(new_cap)
    }

    /// The capacity `decision` gives, once it is within
    /// `[min_capacity, max_capacity]`.
    fn resized(&self, decision: ResizeDecision) -> Result<u64, AutoscaleError> {
        let new_cap = match decision {
            ResizeDecision::Hold => return Ok(self.capacity),
            ResizeDecision::Shrink => self.capacity / 2,
            ResizeDecision::Grow => self.capacity.saturating_mul(2),
            ResizeDecision::EmergencyGrow => self.capacity.saturating_mul(4),
        };
        self.check_bounds(new_cap)?;
        Ok(new_cap)
    }

    fn check_bounds(&self, capacity: u64) -> Result<(), AutoscaleError> {
        if capacity < self.cfg.min_capacity || capacity > self.cfg.max_capacity {
            return Err(AutoscaleError::InvalidResize {
                requested: capacity,
                min: self.cfg.min_capacity,
                max: self.cfg.max_capacity,
            });
        }
        Ok(())
    }
}

/// A ring written under one name and resized by replacing it: the writer's
/// side of the swap a [`LagObserver`] decides.
///
/// The writer publishes through [`Self::ring_mut`] and calls
/// [`Self::observe`] now and then, which resizes when the observer decides
/// to; [`Self::resize`] resizes at once. Readers follow with a
/// [`FollowingReader`], whose read cursor the observer reads as the lag.
/// One writer per name, as for any ring.
pub struct ResizingRing {
    ring: PulseRing,
    observer: LagObserver,
}

impl ResizingRing {
    /// Create a ring of `capacity` slots under `name`, which must be free,
    /// observed with `cfg`.
    ///
    /// # Errors
    ///
    /// [`AutoscaleError::InvalidResize`] when `capacity` is outside `cfg`'s
    /// bounds, before anything is created; [`AutoscaleError::Ring`] when the
    /// ring cannot be created ([`PulseRing::create_exclusive`]).
    pub fn create(name: &str, capacity: u64, cfg: AutoscaleConfig) -> Result<Self, AutoscaleError> {
        let observer = LagObserver::new(cfg, capacity);
        observer.check_bounds(capacity)?;
        Ok(Self {
            ring: PulseRing::create_exclusive(name, capacity)?,
            observer,
        })
    }

    /// The ring the writer publishes to now.
    pub fn ring(&self) -> &PulseRing {
        &self.ring
    }

    /// The ring the writer publishes to now, to publish through.
    pub fn ring_mut(&mut self) -> &mut PulseRing {
        &mut self.ring
    }

    /// The current ring's capacity in slots.
    pub fn capacity(&self) -> u64 {
        self.ring.capacity()
    }

    /// Observe the ring's write and read cursors and, when the observer
    /// decides to resize, resize ([`Self::resize`]); returns the decision.
    ///
    /// # Errors
    ///
    /// Those of [`Self::resize`]; the observer's window has closed either way.
    pub fn observe(&mut self) -> Result<ResizeDecision, AutoscaleError> {
        let decision = self.observer.observe(
            self.ring.cursor_position(),
            self.ring.read_cursor_position(),
        );
        if decision != ResizeDecision::Hold {
            let capacity = self.observer.resized(decision)?;
            self.resize(capacity)?;
        }
        Ok(decision)
    }

    /// Replace the ring with one of `capacity` slots under the same name:
    /// mark the old ring superseded, remove its name, create the new ring.
    /// Every handle on the old ring keeps its mapping, and the old ring's
    /// own drop leaves the name, now the new ring's, alone.
    ///
    /// If the new ring cannot be created, a ring of the old capacity is
    /// created under the name instead, so readers that follow find one, and
    /// the error is returned. If that fails too, its error is returned, and
    /// the writer is left with the old ring, superseded and nameless.
    ///
    /// # Errors
    ///
    /// [`AutoscaleError::InvalidResize`] when `capacity` is outside the
    /// configured bounds, before anything changes; [`AutoscaleError::Ring`]
    /// when the name cannot be removed or the new ring cannot be created.
    pub fn resize(&mut self, capacity: u64) -> Result<(), AutoscaleError> {
        self.observer.check_bounds(capacity)?;
        let name = self.ring.name().to_owned();
        let old_capacity = self.ring.capacity();
        // Release: a reader that sees the mark sees every publish before it.
        self.ring.supersede();
        PulseRing::unlink(&name)?;
        match PulseRing::create_exclusive(&name, capacity) {
            Ok(ring) => {
                self.ring = ring;
                self.observer.capacity = capacity;
                self.observer.low_lag_streak = 0;
                Ok(())
            }
            Err(e) => {
                self.ring = PulseRing::create_exclusive(&name, old_capacity)?;
                Err(e.into())
            }
        }
    }
}

/// A reader of the ring a [`ResizingRing`] writes under a name, which
/// follows it across resizes: it reads every pulse a ring holds, in order,
/// and once that ring is superseded and read to its end, reopens the name.
pub struct FollowingReader {
    ring: PulseRing,
    name: String,
    position: u64,
    lost: u64,
    swaps: u64,
}

impl FollowingReader {
    /// Attach to the ring under `name`, from its next write position.
    ///
    /// # Errors
    ///
    /// [`AutoscaleError::Ring`] when no ring can be opened under the name
    /// ([`PulseRing::open_existing`]), with `Open(ENOENT)` when the name
    /// holds none, as it does for the moment a writer is between rings.
    pub fn open(name: &str) -> Result<Self, AutoscaleError> {
        let ring = PulseRing::open_existing(name)?;
        let position = ring.cursor_position();
        Ok(Self {
            ring,
            name: name.to_owned(),
            position,
            lost: 0,
            swaps: 0,
        })
    }

    /// The next pulse in order, or `Ok(None)` when none is written yet,
    /// which includes the moment the writer is between rings: the name
    /// holds nothing, or a ring its creator has not finished. Each pulse
    /// read is reported to the writer ([`PulseRing::advance_read_cursor`]),
    /// for blocking publish and for the lag the observer sees. Positions
    /// overwritten before they were read are counted in [`Self::lost`].
    ///
    /// # Errors
    ///
    /// [`AutoscaleError::Ring`] when the name holds an object that cannot be
    /// opened as a ring; a name that holds nothing is `Ok(None)`.
    pub fn try_next(&mut self) -> Result<Option<Pulse>, AutoscaleError> {
        loop {
            match self.ring.read_position(self.position) {
                PositionRead::Pulse(pulse) => {
                    self.position += 1;
                    self.ring.advance_read_cursor(self.position);
                    return Ok(Some(pulse));
                }
                PositionRead::Lost { next } => {
                    self.lost += next - self.position;
                    self.position = next;
                    self.ring.advance_read_cursor(next);
                }
                PositionRead::NotWritten => {
                    if !self.ring.is_superseded() {
                        return Ok(None);
                    }
                    // The mark was set after the ring's last publish, and
                    // its acquire load orders the cursor read after it: a
                    // position below the cursor is still to read.
                    if self.position < self.ring.cursor_position() {
                        continue;
                    }
                    match PulseRing::open_existing(&self.name) {
                        Ok(ring) if !ring.is_superseded() => {
                            self.ring = ring;
                            self.position = 0;
                            self.swaps += 1;
                        }
                        // The old ring, its name not yet removed.
                        Ok(_) => return Ok(None),
                        // No ring under the name yet, or the new ring's
                        // creator has not sized it or published its header.
                        Err(
                            RingError::Open(libc::ENOENT)
                            | RingError::RegionTooSmall { have: 0, .. }
                            | RingError::LayoutMismatch {
                                field: "magic",
                                have: 0,
                                ..
                            },
                        ) => return Ok(None),
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }
    }

    /// Positions overwritten before this reader read them.
    pub fn lost(&self) -> u64 {
        self.lost
    }

    /// Rings this reader has moved to since it opened.
    pub fn swaps(&self) -> u64 {
        self.swaps
    }

    /// The ring this reader reads now.
    pub fn ring(&self) -> &PulseRing {
        &self.ring
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn observer_holds_when_lag_in_normal_range() {
        let cfg = AutoscaleConfig {
            observation_window: Duration::from_millis(0), // immediate decision
            ..Default::default()
        };
        let mut obs = LagObserver::new(cfg, 8192);
        // 50% lag (in the [25%, 75%] range)
        let decision = obs.observe(8192, 4096);
        assert_eq!(decision, ResizeDecision::Hold);
    }

    #[test]
    fn observer_grows_when_lag_above_threshold() {
        let cfg = AutoscaleConfig {
            observation_window: Duration::from_millis(0),
            ..Default::default()
        };
        let mut obs = LagObserver::new(cfg, 8192);
        // 90% lag → Grow
        let decision = obs.observe(8192, 819);
        assert_eq!(decision, ResizeDecision::Grow);
    }

    #[test]
    fn observer_shrinks_after_two_low_windows() {
        let cfg = AutoscaleConfig {
            observation_window: Duration::from_millis(0),
            ..Default::default()
        };
        let mut obs = LagObserver::new(cfg, 8192);
        // First low window → Hold (need 2 consecutive)
        let d1 = obs.observe(8192, 7372); // 10% lag
        assert_eq!(d1, ResizeDecision::Hold);
        // Second low window → Shrink
        let d2 = obs.observe(8192, 7372);
        assert_eq!(d2, ResizeDecision::Shrink);
    }

    #[test]
    fn observer_emergency_grows_on_full_lag() {
        let cfg = AutoscaleConfig {
            observation_window: Duration::from_secs(60),
            ..Default::default()
        };
        let mut obs = LagObserver::new(cfg, 8192);
        // 110% lag (consumer has fallen MORE than capacity behind) → emergency
        let decision = obs.observe(9000, 0);
        assert_eq!(decision, ResizeDecision::EmergencyGrow);
    }

    #[test]
    fn apply_grow_doubles_capacity() {
        let cfg = AutoscaleConfig::default();
        let mut obs = LagObserver::new(cfg, 8192);
        let new_cap = obs.apply(ResizeDecision::Grow).unwrap();
        assert_eq!(new_cap, 16384);
        assert_eq!(obs.capacity(), 16384);
    }

    #[test]
    fn apply_shrink_halves_capacity() {
        let cfg = AutoscaleConfig::default();
        let mut obs = LagObserver::new(cfg, 8192);
        let new_cap = obs.apply(ResizeDecision::Shrink).unwrap();
        assert_eq!(new_cap, 4096);
    }

    #[test]
    fn apply_respects_min_max_bounds() {
        let cfg = AutoscaleConfig {
            min_capacity: 1024,
            max_capacity: 4096,
            ..Default::default()
        };
        let mut obs = LagObserver::new(cfg, 4096);
        // Already at max; attempting Grow should error
        let result = obs.apply(ResizeDecision::Grow);
        assert!(matches!(result, Err(AutoscaleError::InvalidResize { .. })));
    }

    /// A ring name private to this process and test.
    fn ring_name(tag: &str) -> String {
        format!("/as-{tag}-{:x}", std::process::id())
    }

    fn step(n: u64) -> Pulse {
        let mut pulse = Pulse::zeroed();
        pulse.chain_step = n;
        pulse
    }

    fn bounds(min_capacity: u64, max_capacity: u64) -> AutoscaleConfig {
        AutoscaleConfig {
            min_capacity,
            max_capacity,
            observation_window: Duration::from_millis(0),
            ..Default::default()
        }
    }

    /// A writer publishing with backpressure grows, shrinks and grows its
    /// ring three times while a reader on another thread reads; the reader
    /// sees every pulse once, in order, and follows each swap.
    #[test]
    fn a_reader_follows_the_ring_across_resizes_and_loses_nothing() {
        const PULSES: u64 = 20_000;
        let name = ring_name("follow");
        let mut writer = ResizingRing::create(&name, 64, bounds(16, 4096)).unwrap();
        let mut reader = FollowingReader::open(&name).unwrap();
        let (read, lost, swaps) = std::thread::scope(|s| {
            s.spawn(|| {
                for n in 0..PULSES {
                    match n {
                        4_000 => writer.resize(1024).unwrap(),
                        9_000 => writer.resize(16).unwrap(),
                        15_000 => writer.resize(256).unwrap(),
                        _ => {}
                    }
                    writer
                        .ring_mut()
                        .publish_blocking(&step(n), Duration::from_secs(10))
                        .unwrap();
                }
            });
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut read = Vec::with_capacity(PULSES as usize);
            while (read.len() as u64) < PULSES {
                assert!(
                    Instant::now() < deadline,
                    "the reader stalled at {}",
                    read.len()
                );
                match reader.try_next().unwrap() {
                    Some(pulse) => read.push(pulse.chain_step),
                    None => std::hint::spin_loop(),
                }
            }
            (read, reader.lost(), reader.swaps())
        });
        assert_eq!(lost, 0);
        assert_eq!(swaps, 3);
        assert!(
            read.iter().copied().eq(0..PULSES),
            "the pulses came back out of order or repeated"
        );
        assert_eq!(writer.capacity(), 256);
    }

    /// Unread pulses filling the ring are an emergency: `observe` quadruples
    /// it at once, and a handle still on the old ring sees it superseded.
    #[test]
    fn observe_grows_a_ring_its_reader_has_fallen_behind_on() {
        let name = ring_name("grow");
        let mut writer = ResizingRing::create(&name, 64, bounds(16, 4096)).unwrap();
        let old = PulseRing::open_existing(&name).unwrap();
        for n in 0..64 {
            writer.ring_mut().publish(&step(n));
        }
        assert_eq!(writer.observe().unwrap(), ResizeDecision::EmergencyGrow);
        assert_eq!(writer.capacity(), 256);
        assert!(old.is_superseded());
        let reopened = PulseRing::open_existing(&name).unwrap();
        assert_eq!(reopened.capacity(), 256);
        assert!(!reopened.is_superseded());
    }

    #[test]
    fn a_resize_outside_the_bounds_changes_nothing() {
        let name = ring_name("bounds");
        assert!(matches!(
            ResizingRing::create(&name, 8, bounds(16, 4096)),
            Err(AutoscaleError::InvalidResize { requested: 8, .. })
        ));
        let mut writer = ResizingRing::create(&name, 64, bounds(16, 4096)).unwrap();
        assert!(matches!(
            writer.resize(8192),
            Err(AutoscaleError::InvalidResize {
                requested: 8192,
                ..
            })
        ));
        assert_eq!(writer.capacity(), 64);
        assert!(!writer.ring().is_superseded());
    }

    /// A new ring that cannot be made leaves a ring of the old size under
    /// the name, which a following reader moves to.
    #[test]
    fn a_swap_that_cannot_make_its_ring_backs_out_to_the_old_size() {
        let name = ring_name("backout");
        let mut writer = ResizingRing::create(&name, 64, bounds(16, u64::MAX)).unwrap();
        let mut reader = FollowingReader::open(&name).unwrap();
        writer.ring_mut().publish(&step(1));
        assert!(matches!(
            writer.resize(u64::MAX / 2),
            Err(AutoscaleError::Ring(RingError::InvalidCapacity { .. }))
        ));
        assert_eq!(writer.capacity(), 64);
        writer.ring_mut().publish(&step(2));
        let mut steps = Vec::new();
        while steps.len() < 2 {
            if let Some(pulse) = reader.try_next().unwrap() {
                steps.push(pulse.chain_step);
            }
        }
        assert_eq!(steps, [1, 2]);
        assert_eq!(reader.swaps(), 1);
    }
}
