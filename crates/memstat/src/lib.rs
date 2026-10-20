//! `memstat` — in-process memory snapshot for fleet Rust services.
//!
//! Reads `/proc/self/status` and `/proc/self/smaps_rollup` on each [`Registry::snapshot`]
//! call (no background thread, no other I/O) and evaluates a set of named gauges the
//! tool registered once at startup.
//!
//! Gauges must not panic: fleet release builds use `panic = "abort"`, so a panic cannot
//! be caught. A gauge returns `Result<u64, String>`; an `Err` is reported in the snapshot
//! as that gauge's `error` field.

use serde::Serialize;
use std::sync::RwLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// What a gauge measures; serialized as `"count"` / `"bytes"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GaugeKind {
    Count,
    Bytes,
}

/// Result a gauge closure returns. `Err` carries a human-readable reason.
pub type GaugeResult = Result<u64, String>;

type GaugeFn = Box<dyn Fn() -> GaugeResult + Send + Sync>;

struct Gauge {
    name: String,
    kind: GaugeKind,
    read: GaugeFn,
}

/// Fields from `/proc/self/status`, in bytes. Absent when the line is missing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusMem {
    pub vm_rss: Option<u64>,
    pub rss_anon: Option<u64>,
    pub rss_file: Option<u64>,
    pub rss_shmem: Option<u64>,
    pub vm_swap: Option<u64>,
    pub vm_hwm: Option<u64>,
}

/// Fields from `/proc/self/smaps_rollup`, in bytes. Absent when unreadable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollupMem {
    pub pss: Option<u64>,
    pub pss_anon: Option<u64>,
    pub pss_file: Option<u64>,
    pub pss_shmem: Option<u64>,
    pub swap: Option<u64>,
}

/// One gauge's reading: exactly one of `value` / `error` is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GaugeReading {
    pub name: String,
    pub kind: GaugeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The snapshot a tool serves (camelCase JSON).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// `None` when `/proc/self/status` was unreadable.
    pub status: Option<StatusMem>,
    /// `None` when `/proc/self/smaps_rollup` was unreadable.
    pub smaps_rollup: Option<RollupMem>,
    pub gauges: Vec<GaugeReading>,
    /// Seconds since the registry was created.
    pub uptime_secs: f64,
    /// Registry creation time, Unix seconds (wall clock). Registries are created at
    /// startup, so this stands in for process start time; true process start
    /// (`/proc/self/stat` starttime) needs `CLK_TCK` via libc and is not derived.
    pub registry_started_unix: u64,
    /// Set when the registry lock was poisoned (gauges then empty).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_error: Option<String>,
}

/// Thread-safe registry of named gauges. Create once at startup, share (e.g. in an `Arc`
/// or a `static OnceLock`), call [`Registry::snapshot`] from a route.
pub struct Registry {
    created: Instant,
    created_unix: u64,
    gauges: RwLock<Vec<Gauge>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        let created_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            created: Instant::now(),
            created_unix,
            gauges: RwLock::new(Vec::new()),
        }
    }

    /// Register a named gauge. The closure must not panic (see crate docs).
    /// Returns `Err` only if the registry lock was poisoned.
    pub fn register<F>(&self, name: &str, kind: GaugeKind, read: F) -> Result<(), String>
    where
        F: Fn() -> GaugeResult + Send + Sync + 'static,
    {
        let mut g = self
            .gauges
            .write()
            .map_err(|_| "memstat registry lock poisoned".to_string())?;
        g.push(Gauge {
            name: name.to_string(),
            kind,
            read: Box::new(read),
        });
        Ok(())
    }

    /// Read `/proc/self/{status,smaps_rollup}` and evaluate every gauge.
    pub fn snapshot(&self) -> Snapshot {
        let status = std::fs::read_to_string("/proc/self/status")
            .ok()
            .map(|s| parse_status(&s));
        let smaps_rollup = std::fs::read_to_string("/proc/self/smaps_rollup")
            .ok()
            .map(|s| parse_smaps_rollup(&s));
        let (gauges, registry_error) = match self.gauges.read() {
            Ok(g) => (g.iter().map(read_gauge).collect(), None),
            Err(_) => (
                Vec::new(),
                Some("memstat registry lock poisoned".to_string()),
            ),
        };
        Snapshot {
            status,
            smaps_rollup,
            gauges,
            uptime_secs: self.created.elapsed().as_secs_f64(),
            registry_started_unix: self.created_unix,
            registry_error,
        }
    }
}

fn read_gauge(g: &Gauge) -> GaugeReading {
    let (value, error) = match (g.read)() {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(e)),
    };
    GaugeReading {
        name: g.name.clone(),
        kind: g.kind,
        value,
        error,
    }
}

/// Parse a `Key:   1234 kB` line value into bytes.
fn kb_value(rest: &str) -> Option<u64> {
    let mut it = rest.split_whitespace();
    let n: u64 = it.next()?.parse().ok()?;
    match it.next() {
        Some("kB") | Some("KB") => n.checked_mul(1024),
        None => Some(n),
        Some(_) => None,
    }
}

fn fields(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|l| l.split_once(':'))
}

/// Pure parser over `/proc/self/status` text.
pub fn parse_status(text: &str) -> StatusMem {
    let mut m = StatusMem::default();
    for (k, v) in fields(text) {
        let slot = match k.trim() {
            "VmRSS" => &mut m.vm_rss,
            "RssAnon" => &mut m.rss_anon,
            "RssFile" => &mut m.rss_file,
            "RssShmem" => &mut m.rss_shmem,
            "VmSwap" => &mut m.vm_swap,
            "VmHWM" => &mut m.vm_hwm,
            _ => continue,
        };
        *slot = kb_value(v);
    }
    m
}

/// Pure parser over `/proc/self/smaps_rollup` text (the header line has no `:` value and is skipped).
pub fn parse_smaps_rollup(text: &str) -> RollupMem {
    let mut m = RollupMem::default();
    for (k, v) in fields(text) {
        let slot = match k.trim() {
            "Pss" => &mut m.pss,
            "Pss_Anon" => &mut m.pss_anon,
            "Pss_File" => &mut m.pss_file,
            "Pss_Shmem" => &mut m.pss_shmem,
            "Swap" => &mut m.swap,
            _ => continue,
        };
        *slot = kb_value(v);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "Name:\tserver\nVmPeak:\t  20000 kB\nVmHWM:\t    9000 kB\nVmRSS:\t    8000 kB\nRssAnon:\t    5000 kB\nRssFile:\t    2900 kB\nRssShmem:\t     100 kB\nVmSwap:\t       0 kB\nThreads:\t4\n";

    const ROLLUP: &str = "55d0c0000000-7ffd00000000 ---p 00000000 00:00 0                          [rollup]\nRss:                8000 kB\nPss:                7000 kB\nPss_Anon:           5000 kB\nPss_File:           1900 kB\nPss_Shmem:           100 kB\nSwap:                 12 kB\nSwapPss:              12 kB\n";

    #[test]
    fn parses_status_fixture() {
        let m = parse_status(STATUS);
        assert_eq!(m.vm_rss, Some(8000 * 1024));
        assert_eq!(m.rss_anon, Some(5000 * 1024));
        assert_eq!(m.rss_file, Some(2900 * 1024));
        assert_eq!(m.rss_shmem, Some(100 * 1024));
        assert_eq!(m.vm_swap, Some(0));
        assert_eq!(m.vm_hwm, Some(9000 * 1024));
    }

    #[test]
    fn missing_and_garbled_fields_are_absent() {
        let m = parse_status("VmRSS:\tgarbage kB\nName:\tx\n");
        assert_eq!(m, StatusMem::default());
    }

    #[test]
    fn parses_smaps_rollup_fixture() {
        let m = parse_smaps_rollup(ROLLUP);
        assert_eq!(m.pss, Some(7000 * 1024));
        assert_eq!(m.pss_anon, Some(5000 * 1024));
        assert_eq!(m.pss_file, Some(1900 * 1024));
        assert_eq!(m.pss_shmem, Some(100 * 1024));
        assert_eq!(m.swap, Some(12 * 1024));
    }

    #[test]
    fn snapshot_reports_gauge_values_and_errors() {
        let r = Registry::new();
        r.register("cache", GaugeKind::Count, || Ok(3)).unwrap();
        r.register("broken", GaugeKind::Bytes, || Err("lock poisoned".into()))
            .unwrap();
        let s = r.snapshot();
        assert_eq!(s.gauges[0].value, Some(3));
        assert_eq!(s.gauges[1].error.as_deref(), Some("lock poisoned"));
        assert!(s.registry_error.is_none());
        if cfg!(target_os = "linux") {
            assert!(s.status.as_ref().and_then(|m| m.vm_rss).is_some());
        }
    }

    #[test]
    fn snapshot_serializes_camel_case() {
        let r = Registry::new();
        r.register("entries", GaugeKind::Count, || Ok(1)).unwrap();
        let v = serde_json::to_value(r.snapshot()).unwrap();
        assert!(v.get("uptimeSecs").is_some());
        assert!(v.get("registryStartedUnix").is_some());
        assert_eq!(v["gauges"][0]["kind"], "count");
        assert!(v["gauges"][0].get("error").is_none());
        if cfg!(target_os = "linux") {
            assert!(v["status"].get("rssAnon").is_some());
        }
    }
}
