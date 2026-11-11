//! glibc allocator controls and statistics (plan memory-instrumentation-and-reduction WP-M1):
//! the arena cap, `malloc_trim` and `mallinfo2`, plus process-start environment defaults
//! (`set_env_default`). This crate holds the workspace's only `unsafe`;
//! on non-Linux/non-glibc targets every function is a no-op or returns zeros.
#![allow(unsafe_code)]

/// Allocator totals in bytes, summed over every arena.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MallocInfo {
    /// Bytes handed out to the program (`uordblks + hblkhd`).
    pub in_use: u64,
    /// Bytes the allocator holds from the OS (`arena + hblkhd`).
    pub system: u64,
}

impl MallocInfo {
    /// Bytes held from the OS but not in use (`system - in_use`, saturating).
    #[must_use]
    pub fn retained(&self) -> u64 {
        self.system.saturating_sub(self.in_use)
    }
}

/// `M_TRIM_THRESHOLD` shared by [`cap_arenas`] and [`tune_daemon`] (128 KiB).
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const TRIM_THRESHOLD_BYTES: i32 = 131_072;

/// Free-but-resident heap: process anonymous RSS minus live allocator bytes (saturating). Unlike
/// [`MallocInfo::retained`] (`system - in_use`, which never shrinks because `malloc_trim`
/// madvises pages away without lowering `arena`), this falls after a trim.
#[must_use]
pub fn free_resident(anon: u64, in_use: u64) -> u64 {
    anon.saturating_sub(in_use)
}

/// Caps glibc malloc arenas (plan editor-cutover-blockers WP-2): off-thread image decodes each
/// grow their own arena and glibc keeps the freed pages, measured at 100–130 MiB of RSS.
/// Equivalent to `MALLOC_ARENA_MAX=2 MALLOC_TRIM_THRESHOLD_=131072`. Linux/glibc only. Call
/// first thing in `main`, before any thread exists.
pub fn cap_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `mallopt` only sets allocator tunables and is called first thing in `main`,
    // before any other thread exists, so no allocation races the change; both parameters are
    // valid glibc options with in-range values. A zero return (rejected) is harmless.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
        libc::mallopt(libc::M_TRIM_THRESHOLD, TRIM_THRESHOLD_BYTES);
    }
}

/// Daemon allocator tuning (plan round-23 WP-3, pinned by `docs/spikes/daemon-threads/FINDINGS.md`
/// "Verdict"): only `M_TRIM_THRESHOLD=131072`, the arena count untouched (an arena cap of 2
/// cost +11-33% run p95 in the spike). Linux/glibc only. Call first thing, before any thread
/// exists.
pub fn tune_daemon() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `mallopt` only sets an allocator tunable and is called before any other thread
    // exists, so no allocation races the change; `M_TRIM_THRESHOLD` is a valid glibc option with
    // an in-range value. A zero return (rejected) is harmless.
    unsafe {
        libc::mallopt(libc::M_TRIM_THRESHOLD, TRIM_THRESHOLD_BYTES);
    }
}

/// Sets the environment variable `key` to `value` only when it is unset (plan
/// memory-instrumentation-and-reduction WP-M5: the editor's GPU driver defaults, which the Vulkan
/// loader and wgpu read from the environment). Call first thing in `main`, before any thread exists.
pub fn set_env_default(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        // SAFETY: `set_var` is unsound only if another thread reads or writes the environment
        // concurrently; this is called first in `main`, before any thread exists.
        unsafe { std::env::set_var(key, value) };
    }
}

/// Returns free heap memory to the OS (`malloc_trim(0)`); `true` if any was released.
#[must_use = "reports whether memory was released"]
pub fn trim() -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: `malloc_trim` takes no pointers and is thread-safe in glibc (it locks each
        // arena); pad 0 is valid.
        let released = unsafe { libc::malloc_trim(0) };
        released != 0
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        false
    }
}

/// Current allocator totals from `mallinfo2` (zeros off glibc).
#[must_use]
pub fn info() -> MallocInfo {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: `mallinfo2` (glibc >= 2.33) takes no arguments, returns a plain struct by
        // value and is thread-safe (it locks each arena while summing).
        let m = unsafe { libc::mallinfo2() };
        let b = |v: libc::size_t| u64::try_from(v).unwrap_or(u64::MAX);
        MallocInfo {
            in_use: b(m.uordblks).saturating_add(b(m.hblkhd)),
            system: b(m.arena).saturating_add(b(m.hblkhd)),
        }
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        MallocInfo::default()
    }
}

/// Allocator bytes retained above live data beyond which a trim is worthwhile (32 MiB); small
/// workloads never cross it, so the threshold is the rate limiter.
pub const TRIM_RETAINED_THRESHOLD: u64 = 32 * 1024 * 1024;

/// This process's anonymous RSS from `/proc/self/status` (`RssAnon`); `None` when unreadable.
#[must_use]
pub fn anon_rss() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    memstat::parse_status(&text).rss_anon
}

/// Current [`free_resident`] figure (`None` when `/proc/self/status` is unreadable).
#[must_use]
pub fn current_free_resident() -> Option<u64> {
    anon_rss().map(|a| free_resident(a, info().in_use))
}

/// Which figure [`release`] judges by. The daemon uses [`Figure::FreeResident`], which falls after
/// a trim; the editor uses [`Figure::ArenaRetained`] ([`MallocInfo::retained`]), which never falls
/// after a trim, so it relies on the editor's once-per-quiet-period rule instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Figure {
    /// [`current_free_resident`]: anonymous RSS minus live allocator bytes.
    FreeResident,
    /// [`MallocInfo::retained`]: bytes held from the OS but not in use.
    ArenaRetained,
}

/// What [`release`] did: the figure it read and whether `malloc_trim` released memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    /// The figure read, in bytes.
    pub figure: u64,
    /// `true` if the trim released any memory.
    pub trimmed: bool,
}

impl Figure {
    /// Reads this figure now (`None` when it is unreadable).
    #[must_use]
    pub fn read(self) -> Option<u64> {
        match self {
            Self::FreeResident => current_free_resident(),
            Self::ArenaRetained => Some(info().retained()),
        }
    }
}

/// Whether a figure warrants a trim: strictly above [`TRIM_RETAINED_THRESHOLD`].
#[must_use]
pub fn over_threshold(figure: u64) -> bool {
    figure > TRIM_RETAINED_THRESHOLD
}

/// The one release entry: reads `figure` and trims when it exceeds [`TRIM_RETAINED_THRESHOLD`].
/// Returns `None` when the figure is unreadable or below the threshold (nothing was trimmed);
/// the caller owns any logging and any trigger policy.
pub fn release(figure: Figure) -> Option<Release> {
    let figure = figure.read()?;
    over_threshold(figure).then(|| Release {
        figure,
        trimmed: trim(),
    })
}

/// The one mapping of a `memstat` snapshot (Serialize-only) plus `mallinfo2` onto the wire type (shared by the daemon and the editor; `cv-protocol` must not depend on
/// `memstat`).
#[must_use]
pub fn to_report(snap: &memstat::Snapshot, malloc: MallocInfo) -> cv_protocol::MemoryReport {
    let status = snap.status.as_ref();
    let mut gauges: Vec<cv_protocol::GaugeValue> = snap
        .gauges
        .iter()
        .map(|g| cv_protocol::GaugeValue {
            name: g.name.clone(),
            value: g.value,
            error: g.error.clone(),
        })
        .collect();
    if let Some(e) = &snap.registry_error {
        gauges.push(cv_protocol::GaugeValue {
            name: "registry".into(),
            value: None,
            error: Some(e.clone()),
        });
    }
    let nonzero = |v: u64| (v > 0).then_some(v);
    cv_protocol::MemoryReport {
        uptime_secs: if snap.uptime_secs.is_finite() && snap.uptime_secs > 0.0 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                snap.uptime_secs as u64
            }
        } else {
            0
        },
        rss: status.and_then(|s| s.vm_rss),
        anon: snap.smaps_rollup.as_ref().and_then(|r| r.pss_anon),
        swap: status.and_then(|s| s.vm_swap),
        hwm: status.and_then(|s| s.vm_hwm),
        malloc_in_use: nonzero(malloc.in_use),
        malloc_system: nonzero(malloc.system),
        gauges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn info_nonzero_and_trim_safe() {
        let buf = std::hint::black_box(vec![7_u8; 1 << 20]);
        let i = info();
        assert!(i.in_use > 0 && i.system > 0, "{i:?}");
        drop(buf);
        let _ = trim();
        assert!(info().system > 0);
    }

    #[test]
    fn free_resident_is_anon_minus_in_use_saturating() {
        assert_eq!(free_resident(100, 30), 70);
        assert_eq!(free_resident(30, 100), 0);
    }

    #[test]
    fn over_threshold_is_strict() {
        assert!(!over_threshold(0));
        assert!(!over_threshold(TRIM_RETAINED_THRESHOLD));
        assert!(over_threshold(TRIM_RETAINED_THRESHOLD + 1));
    }

    #[test]
    fn figure_selection_reads_the_named_figure() {
        // Arena-retained is always readable; other test threads allocate, so only presence is
        // stable. Free-resident is `None` exactly when `/proc/self/status` is unreadable.
        assert!(Figure::ArenaRetained.read().is_some());
        assert_eq!(Figure::FreeResident.read().is_some(), anon_rss().is_some());
    }

    #[test]
    fn report_maps_snapshot_and_survives_unreadable_proc() {
        let reg = memstat::Registry::new();
        reg.register("ok", memstat::GaugeKind::Count, || Ok(3))
            .expect("register");
        reg.register(
            "bad",
            memstat::GaugeKind::Count,
            || Err("unreadable".into()),
        )
        .expect("register");
        let mut snap = reg.snapshot();
        // `/proc/self/smaps_rollup` (and status) unreadable: absent fields, no panic.
        snap.status = None;
        snap.smaps_rollup = None;
        let r = to_report(&snap, MallocInfo::default());
        assert_eq!((r.rss, r.anon, r.swap, r.hwm), (None, None, None, None));
        assert_eq!(r.malloc_in_use, None);
        assert_eq!(r.gauges[0].value, Some(3));
        assert_eq!(r.gauges[1].error.as_deref(), Some("unreadable"));
    }
}
