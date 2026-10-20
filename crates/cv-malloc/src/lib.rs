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
        libc::mallopt(libc::M_TRIM_THRESHOLD, 131_072);
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
}
