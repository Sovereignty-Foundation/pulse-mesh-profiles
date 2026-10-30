// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! `hft_functions::wait`: pluggable wait strategies (module M03).
//!
//! A [`WaitStrategy`] waits for a slot's word to change: from [`BusySpin`]
//! (lowest latency, highest power) through hardware cache-line monitors to
//! [`Park`] (lowest power, highest jitter). The hardware strategies are the
//! opt-in `umwait`, `mwaitx` and `wfe` features, and
//! [`detect_best_for_host`] picks one the host supports.
//!
//! This trait is for hardware monitor waits on a raw slot address. OS-level
//! stalls use `core_backoff::Backoff`; bounded SeqLock retries stay bare
//! `spin_loop()` loops (see `core-backoff`'s "Which wait mechanism?").

use std::time::Duration;

/// Outcome of a single [`WaitStrategy::wait`] call, which cannot fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitResult {
    /// Monitored cache line changed (or strategy-specific equivalent).
    Changed,
    /// Strategy-specific timeout elapsed without observed change.
    Timeout,
    /// Strategy not supported on this CPU/kernel; caller should fall back.
    NotSupported,
}

/// How a receiver waits for its next slot to become readable. A strategy is
/// called once per slot, and its `wait` must not allocate.
pub trait WaitStrategy: Send + Sync {
    /// Wait for the `AtomicU64` at `monitor_addr` to differ from `expected`,
    /// or for the strategy's timeout.
    ///
    /// # Safety
    ///
    /// `monitor_addr` must point to an aligned, initialized `AtomicU64` that
    /// stays valid (not freed, unmapped or detached) for the whole call. A
    /// hardware feature the strategy needs is not the caller's to check: the
    /// strategy checked it when it was made.
    unsafe fn wait(&self, monitor_addr: *const u8, expected: u64) -> WaitResult;

    /// A cheap hint for one round of a polling loop: `std::hint::spin_loop()`
    /// by default. `UMWaitDeadline` (feature `umwait`) waits in the monitor
    /// instead, so the core stops loading the line; the other strategies use
    /// the default.
    #[inline(always)]
    fn poll_hint(&self, _line: &std::sync::atomic::AtomicU64, _expected: u64) {
        std::hint::spin_loop();
    }

    /// Human-readable name for telemetry / bench output.
    fn name(&self) -> &'static str;
}

/// Busy-spin: the lowest latency, at the cost of a core and memory
/// bandwidth, for a receiver with cores to spare. A wait returns
/// [`WaitResult::Timeout`] after a million polls without a change.
pub struct BusySpin;

impl WaitStrategy for BusySpin {
    #[inline(always)]
    unsafe fn wait(&self, monitor_addr: *const u8, expected: u64) -> WaitResult {
        let p = monitor_addr as *const std::sync::atomic::AtomicU64;
        for _ in 0..1_000_000u64 {
            // SAFETY: caller upholds the WaitStrategy::wait contract that
            // monitor_addr points at a properly-aligned, live AtomicU64.
            let v = unsafe { (*p).load(std::sync::atomic::Ordering::Acquire) };
            if v != expected {
                return WaitResult::Changed;
            }
            std::hint::spin_loop();
        }
        WaitResult::Timeout
    }
    fn name(&self) -> &'static str {
        "busy-spin"
    }
}

/// OS thread park: the lowest power and the highest jitter. A wait parks
/// once, for up to `quantum`, then returns [`WaitResult::Timeout`] if the
/// word has not changed.
pub struct Park {
    /// How long a wait parks: longer costs less CPU and more latency.
    pub quantum: Duration,
}

impl WaitStrategy for Park {
    unsafe fn wait(&self, monitor_addr: *const u8, expected: u64) -> WaitResult {
        let p = monitor_addr as *const std::sync::atomic::AtomicU64;
        // SAFETY: the WaitStrategy::wait contract makes monitor_addr an
        // aligned, live AtomicU64 for the whole call.
        let v = unsafe { (*p).load(std::sync::atomic::Ordering::Acquire) };
        if v != expected {
            return WaitResult::Changed;
        }
        std::thread::park_timeout(self.quantum);
        // SAFETY: same contract as above; the parked-then-resumed sequence
        // does not invalidate the pointer.
        let v2 = unsafe { (*p).load(std::sync::atomic::Ordering::Acquire) };
        if v2 != expected {
            WaitResult::Changed
        } else {
            WaitResult::Timeout
        }
    }
    fn name(&self) -> &'static str {
        "park"
    }
}

/// Intel WAITPKG (UMONITOR/UMWAIT; Tremont, Ice Lake-X and later, Sapphire
/// Rapids): the core sleeps in a light C0 state watching the cache line, and
/// wakes within about 100-500 ns of a write to it.
///
/// Only [`UMWaitDeadline::new`] makes one, and only where [`umwait_available`]
/// finds WAITPKG: [`WaitStrategy::poll_hint`] runs the instructions from safe
/// code, and they fault on a CPU without it.
#[cfg(all(target_arch = "x86_64", feature = "umwait"))]
pub struct UMWaitDeadline {
    /// TSC cycles from the start of a wait to its deadline; 0 waits until the
    /// line changes. Private, so no struct literal skips the check in `new`.
    deadline_tsc_cycles: u64,
}

#[cfg(all(target_arch = "x86_64", feature = "umwait"))]
impl UMWaitDeadline {
    /// The UMWAIT strategy with a deadline of `deadline_tsc_cycles` TSC cycles
    /// after each wait begins (0 waits until the line changes), or `None` when
    /// [`umwait_available`] does not find WAITPKG on this host.
    #[must_use]
    pub fn new(deadline_tsc_cycles: u64) -> Option<Self> {
        umwait_available().then_some(Self {
            deadline_tsc_cycles,
        })
    }
}

#[cfg(all(target_arch = "x86_64", feature = "umwait"))]
impl WaitStrategy for UMWaitDeadline {
    #[inline]
    unsafe fn wait(&self, monitor_addr: *const u8, expected: u64) -> WaitResult {
        let p = monitor_addr as *const std::sync::atomic::AtomicU64;
        let deadline = if self.deadline_tsc_cycles == 0 {
            u64::MAX
        } else {
            // SAFETY: RDTSC has no preconditions; it only reads the counter.
            unsafe { std::arch::x86_64::_rdtsc() }.wrapping_add(self.deadline_tsc_cycles)
        };
        let lo = (deadline & 0xFFFF_FFFF) as u32;
        let hi = (deadline >> 32) as u32;
        loop {
            // Arm, compare, wait, in one asm block: `umonitor` arms the
            // monitor on the word's line first, so a store that lands
            // before the compare is seen by the compare, and one that lands
            // after it wakes `umwait`. Comparing before arming would miss a
            // store between the two and sleep to the deadline. Hint 1 asks
            // for C0.1, the lighter sleep that wakes sooner.
            //
            // SAFETY: `monitor_addr` is an aligned, initialised, live
            // `AtomicU64` for the call (the `WaitStrategy::wait` contract):
            // `umonitor` arms on it without touching memory and `cmp` reads
            // it; `umwait` blocks until the line is written, the TSC
            // deadline in EDX:EAX passes, the OS time limit ends the wait,
            // or an interrupt. Every input is declared, the stack is
            // untouched (`nostack`), and the flags are clobbered, which the
            // block does not claim to preserve. WAITPKG is present, since
            // `UMWaitDeadline::new` makes a strategy only where
            // `umwait_available()` finds it; without it the instructions
            // raise #UD rather than touch memory.
            unsafe {
                std::arch::asm!(
                    "umonitor {addr}",
                    "cmp qword ptr [{addr}], {expected}",
                    "jne 2f",
                    "umwait {hint:r}",
                    "2:",
                    addr = in(reg) monitor_addr,
                    expected = in(reg) expected,
                    hint = in(reg) 1u64,
                    in("eax") lo,
                    in("edx") hi,
                    options(nostack),
                );
            }
            // Decide by the word, not by CF: UMWAIT sets CF only when the
            // OS time limit ended the wait, so its own deadline, an
            // interrupt and the monitored write all leave CF clear.
            // SAFETY: as above.
            if unsafe { (*p).load(std::sync::atomic::Ordering::Acquire) } != expected {
                return WaitResult::Changed;
            }
            // SAFETY: RDTSC has no preconditions; it only reads the counter.
            if deadline != u64::MAX && unsafe { std::arch::x86_64::_rdtsc() } >= deadline {
                return WaitResult::Timeout;
            }
            // Woken early with the word unchanged (an interrupt, or the OS
            // time limit): wait again.
        }
    }

    /// One [`WaitStrategy::wait`] with this strategy's deadline, in place of a
    /// spin: the core sleeps in the monitor rather than loading the line.
    #[inline(always)]
    fn poll_hint(&self, line: &std::sync::atomic::AtomicU64, expected: u64) {
        let monitor_addr = line as *const std::sync::atomic::AtomicU64 as *const u8;
        // SAFETY: `line` is a live `&AtomicU64` for the whole call, so
        // `monitor_addr` meets the `wait` contract; WAITPKG was checked by
        // `UMWaitDeadline::new`, the only way to make `self`.
        unsafe {
            let _ = self.wait(monitor_addr, expected);
        }
    }
    fn name(&self) -> &'static str {
        "umwait"
    }
}

/// Whether WAITPKG can be used here: CPUID reports it (`CPUID.7.0:ECX[5]`)
/// and, on Linux, `/proc/cpuinfo` lists `waitpkg`, which the kernel does only
/// once it has enabled the feature. False on every other OS.
/// `UMWaitDeadline::new` (feature `umwait`) makes a strategy exactly when this
/// is true.
#[cfg(target_arch = "x86_64")]
pub fn umwait_available() -> bool {
    let cpuid = raw_cpuid::CpuId::new();
    if let Some(ext) = cpuid.get_extended_feature_info() {
        if !ext.has_waitpkg() {
            return false;
        }
    } else {
        return false;
    }
    // Elsewhere CPUID alone cannot tell whether the OS enabled the feature.
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/cpuinfo") {
            return s
                .lines()
                .any(|l| l.starts_with("flags") && l.contains(" waitpkg"));
        }
    }
    false
}

/// Always `false`: WAITPKG is x86_64-only. ARM's counterpart is `WfeArm`
/// (feature `wfe`).
#[cfg(not(target_arch = "x86_64"))]
pub fn umwait_available() -> bool {
    false
}

/// AMD MONITORX/MWAITX (Zen 2 and later): the core waits in C0 watching the
/// cache line, as `UMWaitDeadline` does on Intel, and wakes on a write to
/// it, its timer, or an interrupt.
///
/// Only [`MWaitX::new`] makes one, and only where [`mwaitx_available`] finds
/// MONITORX: [`WaitStrategy::poll_hint`] runs the instructions from safe
/// code, and they fault on a CPU without it.
#[cfg(all(target_arch = "x86_64", feature = "mwaitx"))]
pub struct MWaitX {
    /// TSC cycles from the start of a wait to its deadline; 0 waits until the
    /// line changes. Private, so no struct literal skips the check in `new`.
    deadline_tsc_cycles: u64,
}

/// MWAITX's EAX hint: stay in C0, the state that wakes soonest.
#[cfg(all(target_arch = "x86_64", feature = "mwaitx"))]
const MWAITX_C0: u32 = 0xF0;
/// MWAITX's ECX bit 1: end the wait when the EBX timer runs out.
#[cfg(all(target_arch = "x86_64", feature = "mwaitx"))]
const MWAITX_TIMER_ENABLE: u32 = 1 << 1;

#[cfg(all(target_arch = "x86_64", feature = "mwaitx"))]
impl MWaitX {
    /// The MWAITX strategy with a deadline of `deadline_tsc_cycles` TSC cycles
    /// after each wait begins (0 waits until the line changes), or `None` when
    /// [`mwaitx_available`] does not find MONITORX on this host.
    #[must_use]
    pub fn new(deadline_tsc_cycles: u64) -> Option<Self> {
        mwaitx_available().then_some(Self {
            deadline_tsc_cycles,
        })
    }
}

#[cfg(all(target_arch = "x86_64", feature = "mwaitx"))]
impl WaitStrategy for MWaitX {
    #[inline]
    unsafe fn wait(&self, monitor_addr: *const u8, expected: u64) -> WaitResult {
        let p = monitor_addr as *const std::sync::atomic::AtomicU64;
        let deadline = if self.deadline_tsc_cycles == 0 {
            u64::MAX
        } else {
            // SAFETY: RDTSC has no preconditions; it only reads the counter.
            unsafe { std::arch::x86_64::_rdtsc() }.wrapping_add(self.deadline_tsc_cycles)
        };
        loop {
            // MWAITX's timer is relative and 32 bits wide, counted at the
            // TSC rate (about 1.4 s at 3 GHz), so a longer deadline, or none,
            // takes more than one round.
            let timer = if deadline == u64::MAX {
                u32::MAX
            } else {
                // SAFETY: RDTSC has no preconditions; it only reads the counter.
                let now = unsafe { std::arch::x86_64::_rdtsc() };
                if now >= deadline {
                    return WaitResult::Timeout;
                }
                u32::try_from(deadline - now).unwrap_or(u32::MAX)
            };
            // Arm, compare, wait, in one asm block, as `UMWaitDeadline`
            // does: `monitorx` arms the monitor on the word's line first, so
            // a store that lands before the compare is seen by the compare,
            // and one that lands after it wakes `mwaitx`. MONITORX takes the
            // address in RAX and zero extensions (ECX) and hints (EDX);
            // MWAITX takes the C-state hint in EAX, the timer enable in ECX
            // and the timer in EBX. LLVM reserves RBX, so the timer goes
            // through a scratch register swapped in and back around the
            // wait, on the path that waits only.
            //
            // SAFETY: `monitor_addr` is an aligned, initialised, live
            // `AtomicU64` for the call (the `WaitStrategy::wait` contract):
            // `monitorx` arms on it without touching memory and `cmp` reads
            // it; `mwaitx` blocks until the line is written, the timer runs
            // out, or an interrupt, and stores nothing. RBX holds the timer
            // only between the two `xchg`s and leaves the block as it came
            // in. Every other register the block writes is declared (RAX and
            // RCX as clobbers, the scratch as an inout), the stack is
            // untouched (`nostack`), and the flags are clobbered, which the
            // block does not claim to preserve. MONITORX is present, since
            // `MWaitX::new` makes a strategy only where `mwaitx_available()`
            // finds it; without it the instructions raise #UD rather than
            // touch memory.
            unsafe {
                std::arch::asm!(
                    "mov rax, {addr}",
                    "xor ecx, ecx",
                    "monitorx",
                    "cmp qword ptr [{addr}], {expected}",
                    "jne 2f",
                    "xchg {timer:r}, rbx",
                    "mov eax, {hint:e}",
                    "mov ecx, {enable:e}",
                    "mwaitx",
                    "xchg {timer:r}, rbx",
                    "2:",
                    addr = in(reg) monitor_addr,
                    expected = in(reg) expected,
                    timer = inout(reg) u64::from(timer) => _,
                    hint = in(reg) MWAITX_C0,
                    enable = in(reg) MWAITX_TIMER_ENABLE,
                    out("rax") _,
                    out("rcx") _,
                    in("edx") 0u32,
                    options(nostack),
                );
            }
            // Decide by the word: MWAITX reports nothing about why it woke.
            // SAFETY: as above.
            if unsafe { (*p).load(std::sync::atomic::Ordering::Acquire) } != expected {
                return WaitResult::Changed;
            }
            // Woken with the word unchanged (an interrupt, a store elsewhere
            // in the line, or the timer): the deadline is checked on the way
            // round.
        }
    }

    /// One [`WaitStrategy::wait`] with this strategy's deadline, in place of a
    /// spin: the core sleeps in the monitor rather than loading the line.
    #[inline(always)]
    fn poll_hint(&self, line: &std::sync::atomic::AtomicU64, expected: u64) {
        let monitor_addr = line as *const std::sync::atomic::AtomicU64 as *const u8;
        // SAFETY: `line` is a live `&AtomicU64` for the whole call, so
        // `monitor_addr` meets the `wait` contract; MONITORX was checked by
        // `MWaitX::new`, the only way to make `self`.
        unsafe {
            let _ = self.wait(monitor_addr, expected);
        }
    }
    fn name(&self) -> &'static str {
        "mwaitx"
    }
}

/// Whether MONITORX/MWAITX can be used here: CPUID reports it on an AMD CPU
/// (`CPUID.80000001h:ECX[29]`) and, on Linux, `/proc/cpuinfo` lists
/// `mwaitx`, which a guest kernel does not when its hypervisor hides the
/// feature. False on every other OS. `MWaitX::new` (feature `mwaitx`) makes a
/// strategy exactly when this is true.
#[cfg(target_arch = "x86_64")]
pub fn mwaitx_available() -> bool {
    let cpuid = raw_cpuid::CpuId::new();
    if !cpuid
        .get_extended_processor_and_feature_identifiers()
        .is_some_and(|f| f.has_monitorx_mwaitx())
    {
        return false;
    }
    // Elsewhere CPUID alone cannot tell whether the OS enabled the feature.
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/cpuinfo") {
            return s
                .lines()
                .any(|l| l.starts_with("flags") && l.split_whitespace().any(|f| f == "mwaitx"));
        }
    }
    false
}

/// Always `false`: MONITORX is x86_64-only. ARM's counterpart is `WfeArm`
/// (feature `wfe`).
#[cfg(not(target_arch = "x86_64"))]
pub fn mwaitx_available() -> bool {
    false
}

/// ARM AArch64 WFE (Wait For Event), for a waiter that watches one 64-bit
/// word.
///
/// [`WaitStrategy::wait`] returns [`WaitResult::Changed`] once the word
/// differs from `expected`. Each round loads it with `LDXR`, which arms this
/// core's exclusive monitor on the word's granule, and sleeps in `WFE` if it
/// is unchanged. No store is missed: one before the `LDXR` is seen by it,
/// and one after clears the monitor, which is a wake-up event, as is the
/// exception return of a thread preempted between the two. Any other event
/// (an interrupt, an `SEV`, a store elsewhere in the granule, the timer's
/// event stream) costs one round.
///
/// The wait returns [`WaitResult::Timeout`] after a million wakes without a
/// change, which bounds wakes, not time: about 1.3 s on Apple silicon, where
/// `WFE` returned about every 1.3 µs with nothing to wake it; about 100 s on
/// a Linux host whose only events are its default 10 kHz timer event
/// stream; unbounded with no event stream. A caller that needs a deadline
/// checks its own between calls.
#[cfg(all(target_arch = "aarch64", feature = "wfe"))]
pub struct WfeArm;

#[cfg(all(target_arch = "aarch64", feature = "wfe"))]
impl WaitStrategy for WfeArm {
    #[inline(always)]
    unsafe fn wait(&self, monitor_addr: *const u8, expected: u64) -> WaitResult {
        let p = monitor_addr as *const std::sync::atomic::AtomicU64;
        for _ in 0..1_000_000u64 {
            // SAFETY: the `WaitStrategy::wait` contract makes `monitor_addr`
            // an aligned, initialised, live `AtomicU64` for the whole call.
            let v = unsafe { (*p).load(std::sync::atomic::Ordering::Acquire) };
            if v != expected {
                return WaitResult::Changed;
            }
            // SAFETY: `LDXR` loads the word at `monitor_addr`, which the
            // `WaitStrategy::wait` contract makes an aligned (an exclusive load
            // faults otherwise), initialised `AtomicU64` mapped for the call,
            // as one single-copy-atomic load, and arms this core's exclusive
            // monitor on its granule. Nothing is stored: no `STXR` follows,
            // and an armed monitor changes nothing later code observes, since
            // every exclusive sequence starts with an `LDXR` that re-arms it.
            // `CMP` and `B.NE` skip the `WFE` once the word has changed. `WFE`
            // only waits for an event: Linux and macOS let user code run it,
            // and a hypervisor that traps it treats it as a wait. The block
            // uses no stack and writes only its declared scratch register and
            // the flags, which it does not claim to preserve.
            unsafe {
                std::arch::asm!(
                    "ldxr {value}, [{addr}]",
                    "cmp {value}, {expected}",
                    "b.ne 2f",
                    "wfe",
                    "2:",
                    addr = in(reg) monitor_addr,
                    expected = in(reg) expected,
                    value = out(reg) _,
                    options(nostack),
                );
            }
        }
        WaitResult::Timeout
    }
    fn name(&self) -> &'static str {
        "wfe"
    }
}

/// The best strategy for this host: on x86_64, `UMWaitDeadline` when the
/// `umwait` feature is on and [`umwait_available`] finds WAITPKG, then
/// `MWaitX` when the `mwaitx` feature is on and [`mwaitx_available`] finds
/// MONITORX; `WfeArm` on aarch64 when the `wfe` feature is on; and
/// [`BusySpin`] otherwise. Each waits until the line changes, with no
/// deadline.
pub fn detect_best_for_host() -> Box<dyn WaitStrategy> {
    #[cfg(all(target_arch = "x86_64", feature = "umwait"))]
    {
        if let Some(umwait) = UMWaitDeadline::new(0) {
            return Box::new(umwait);
        }
    }
    #[cfg(all(target_arch = "x86_64", feature = "mwaitx"))]
    {
        if let Some(mwaitx) = MWaitX::new(0) {
            return Box::new(mwaitx);
        }
    }
    // Exactly one of the two blocks below survives cfg: the tail expression.
    #[cfg(all(target_arch = "aarch64", feature = "wfe"))]
    {
        Box::new(WfeArm)
    }
    #[cfg(not(all(target_arch = "aarch64", feature = "wfe")))]
    {
        Box::new(BusySpin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(all(target_arch = "aarch64", feature = "wfe"))]
    #[test]
    fn wfe_observes_change() {
        let line = AtomicU64::new(1);
        let addr = &line as *const AtomicU64 as *const u8;
        // SAFETY: `line` outlives the call and is a valid AtomicU64; the
        // value already differs, so the fast path returns before any `wfe`.
        let r = unsafe { WfeArm.wait(addr, 0) };
        assert_eq!(r, WaitResult::Changed);
        assert_eq!(WfeArm.name(), "wfe");
    }

    /// A waiter in `WfeArm::wait` wakes when another thread stores to the
    /// word. Each trial parks a waiter, stores, and times its return; the
    /// fastest must be under a millisecond, which a waiter left to the
    /// scheduler's tick would miss where the timer event stream is slow or off
    /// (on Apple silicon `WFE` returns every 1.3 µs anyway). The best of many
    /// trials is bounded, since a loaded host may deschedule any one waiter.
    #[cfg(all(target_arch = "aarch64", feature = "wfe"))]
    #[test]
    #[allow(clippy::expect_used, clippy::panic)]
    fn wfe_wakes_when_another_thread_stores_to_the_word() {
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        const TRIALS: usize = 50;
        let mut fastest = Duration::MAX;
        for _ in 0..TRIALS {
            let line = AtomicU64::new(0);
            let waiting = AtomicBool::new(false);
            let latency = std::thread::scope(|s| {
                let waiter = s.spawn(|| {
                    let addr = &line as *const AtomicU64 as *const u8;
                    let give_up = Instant::now() + Duration::from_secs(10);
                    waiting.store(true, Ordering::Release);
                    loop {
                        // SAFETY: `line` is an aligned, initialised AtomicU64
                        // that outlives the scope, and so this call.
                        match unsafe { WfeArm.wait(addr, 0) } {
                            WaitResult::Changed => return Instant::now(),
                            WaitResult::Timeout => {
                                assert!(Instant::now() < give_up, "the store never woke the waiter")
                            }
                            WaitResult::NotSupported => panic!("WFE reported NotSupported"),
                        }
                    }
                });
                while !waiting.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
                std::thread::sleep(Duration::from_millis(2));
                let stored = Instant::now();
                line.store(1, Ordering::Release);
                let woke = waiter.join().expect("the waiter");
                woke.saturating_duration_since(stored)
            });
            fastest = fastest.min(latency);
        }
        assert!(
            fastest < Duration::from_millis(1),
            "the fastest of {TRIALS} waiters woke {fastest:?} after the store"
        );
    }

    #[cfg(all(target_arch = "x86_64", feature = "umwait"))]
    #[test]
    fn umwait_is_made_only_where_the_probe_finds_waitpkg() {
        assert_eq!(UMWaitDeadline::new(0).is_some(), umwait_available());
        assert_eq!(UMWaitDeadline::new(10_000).is_some(), umwait_available());
    }

    #[cfg(all(target_arch = "x86_64", feature = "umwait"))]
    #[test]
    fn umwait_observes_change() {
        // Without WAITPKG there is no strategy to test.
        let Some(s) = UMWaitDeadline::new(0) else {
            return;
        };
        let line = AtomicU64::new(1);
        let addr = &line as *const AtomicU64 as *const u8;
        // SAFETY: `line` outlives the call; the value already differs, so
        // the fast path returns before any WAITPKG instruction executes.
        let r = unsafe { s.wait(addr, 0) };
        assert_eq!(r, WaitResult::Changed);
    }

    #[cfg(all(target_arch = "x86_64", feature = "mwaitx"))]
    #[test]
    fn mwaitx_is_made_only_where_the_probe_finds_monitorx() {
        assert_eq!(MWaitX::new(0).is_some(), mwaitx_available());
        assert_eq!(MWaitX::new(10_000).is_some(), mwaitx_available());
    }

    #[test]
    fn mwaitx_is_never_found_off_amd() {
        #[cfg(target_arch = "x86_64")]
        {
            let amd = raw_cpuid::CpuId::new()
                .get_vendor_info()
                .is_some_and(|v| v.as_str() == "AuthenticAMD");
            if !amd {
                assert!(!mwaitx_available());
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        assert!(!mwaitx_available());
    }

    #[cfg(all(target_arch = "x86_64", feature = "mwaitx"))]
    #[test]
    fn mwaitx_observes_change() {
        // Without MONITORX there is no strategy to test.
        let Some(s) = MWaitX::new(0) else {
            return;
        };
        let line = AtomicU64::new(1);
        let addr = &line as *const AtomicU64 as *const u8;
        // SAFETY: `line` outlives the call and is a valid AtomicU64; the
        // monitor arms on it and the compare finds it already changed.
        let r = unsafe { s.wait(addr, 0) };
        assert_eq!(r, WaitResult::Changed);
        assert_eq!(s.name(), "mwaitx");
    }

    /// An `MWaitX` with a deadline returns `Timeout` once it passes, with the
    /// word unchanged: a few million TSC cycles, a millisecond or so.
    #[cfg(all(target_arch = "x86_64", feature = "mwaitx"))]
    #[test]
    fn mwaitx_times_out_at_its_deadline() {
        use std::time::{Duration, Instant};
        let Some(s) = MWaitX::new(3_000_000) else {
            return;
        };
        let line = AtomicU64::new(0);
        let addr = &line as *const AtomicU64 as *const u8;
        let started = Instant::now();
        // SAFETY: `line` is an aligned, initialised AtomicU64 that outlives
        // the call.
        let r = unsafe { s.wait(addr, 0) };
        assert_eq!(r, WaitResult::Timeout);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    /// A waiter in `MWaitX::wait` wakes when another thread stores to the
    /// word, as `wfe_wakes_when_another_thread_stores_to_the_word` checks for
    /// WFE: the fastest of many trials must wake under a millisecond after
    /// the store, which a waiter left to its 32-bit timer or the scheduler's
    /// tick would miss.
    #[cfg(all(target_arch = "x86_64", feature = "mwaitx"))]
    #[test]
    #[allow(clippy::expect_used, clippy::panic)]
    fn mwaitx_wakes_when_another_thread_stores_to_the_word() {
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        const TRIALS: usize = 50;
        let Some(s) = MWaitX::new(0) else {
            return;
        };
        let mut fastest = Duration::MAX;
        for _ in 0..TRIALS {
            let line = AtomicU64::new(0);
            let waiting = AtomicBool::new(false);
            let latency = std::thread::scope(|scope| {
                let waiter = scope.spawn(|| {
                    let addr = &line as *const AtomicU64 as *const u8;
                    waiting.store(true, Ordering::Release);
                    // SAFETY: `line` is an aligned, initialised AtomicU64
                    // that outlives the scope, and so this call.
                    match unsafe { s.wait(addr, 0) } {
                        WaitResult::Changed => Instant::now(),
                        other => panic!("MWAITX with no deadline returned {other:?}"),
                    }
                });
                while !waiting.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
                std::thread::sleep(Duration::from_millis(2));
                let stored = Instant::now();
                line.store(1, Ordering::Release);
                let woke = waiter.join().expect("the waiter");
                woke.saturating_duration_since(stored)
            });
            fastest = fastest.min(latency);
        }
        assert!(
            fastest < Duration::from_millis(1),
            "the fastest of {TRIALS} waiters woke {fastest:?} after the store"
        );
    }

    /// Where MONITORX is found and WAITPKG is not (an AMD host),
    /// `detect_best_for_host` picks MWAITX.
    #[cfg(all(target_arch = "x86_64", feature = "mwaitx"))]
    #[test]
    fn detect_picks_mwaitx_on_amd() {
        if mwaitx_available() && !umwait_available() {
            assert_eq!(detect_best_for_host().name(), "mwaitx");
        }
    }

    #[test]
    fn busy_spin_observes_change() {
        let v = AtomicU64::new(0);
        let strat = BusySpin;
        v.store(1, Ordering::Release);
        // SAFETY: `v` is a live, aligned local `AtomicU64` for the call.
        let result = unsafe { strat.wait(&v as *const _ as *const u8, 0) };
        assert_eq!(result, WaitResult::Changed);
    }

    #[test]
    fn busy_spin_times_out_when_unchanged() {
        let v = AtomicU64::new(0);
        let strat = BusySpin;
        // SAFETY: `v` is a live, aligned local `AtomicU64` for the call.
        let result = unsafe { strat.wait(&v as *const _ as *const u8, 0) };
        assert_eq!(result, WaitResult::Timeout);
    }

    #[test]
    fn detect_returns_some_strategy() {
        let s = detect_best_for_host();
        assert!(!s.name().is_empty());
    }
}
