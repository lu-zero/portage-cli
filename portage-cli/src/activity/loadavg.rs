//! System load average — Portage-compatible read + formatting.
//!
//! Real emerge uses `_emerge/getloadavg.py` (`os.getloadavg()`, falling back
//! to `/proc/loadavg`) for the `Jobs:` status line (`JobStatusDisplay`) and
//! for `--load-average` job-start throttling (`PollScheduler._can_add_job`).
//! Both call sites need the same three-tuple; formatting is display-only.
//!
//! We read `/proc/loadavg` directly (Portage's own fallback). That is the
//! path that matters on Gentoo Linux; other platforms without `/proc` get
//! `None` ("unknown" on the status line; throttle treats that as "do not
//! start more jobs" when a limit is set — same as Portage's `OSError` path).

use std::fs;

/// 1-, 5-, and 15-minute load averages, or `None` if unobtainable.
pub fn get_loadavg() -> Option<(f64, f64, f64)> {
    let line = fs::read_to_string("/proc/loadavg").ok()?;
    let mut parts = line.split_whitespace();
    let a = parts.next()?.parse().ok()?;
    let b = parts.next()?.parse().ok()?;
    let c = parts.next()?.parse().ok()?;
    Some((a, b, c))
}

/// Portage `JobStatusDisplay._load_avg_str`: precision depends on the max of
/// the three samples (`<10` → 2 decimals, `<100` → 1, else 0).
pub fn format_load_avg(avg: (f64, f64, f64)) -> String {
    let max = avg.0.max(avg.1).max(avg.2);
    let fmt = |x: f64| {
        if max < 10.0 {
            format!("{x:.2}")
        } else if max < 100.0 {
            format!("{x:.1}")
        } else {
            format!("{x:.0}")
        }
    };
    format!("{}, {}, {}", fmt(avg.0), fmt(avg.1), fmt(avg.2))
}

/// `"Load avg: …"` or `"Load avg: unknown"` when unreadable.
pub fn load_avg_display() -> String {
    match get_loadavg() {
        Some(avg) => format!("Load avg: {}", format_load_avg(avg)),
        None => "Load avg: unknown".into(),
    }
}

/// Whether a new parallel job may start under `--load-average LOAD`.
///
/// Portage `PollScheduler._can_add_job`: when `max_load` is set and at least
/// one job is already running, refuse to start another if the 1-minute
/// average is ≥ `max_load`. The first concurrent job is always allowed (so a
/// quiet machine can start work). Unreadable loadavg refuses further starts
/// (same as Portage's `OSError` path).
pub fn can_start_under_load(max_load: Option<f64>, running: usize) -> bool {
    let Some(limit) = max_load else {
        return true;
    };
    if running < 1 {
        return true;
    }
    match get_loadavg() {
        Some((avg1, _, _)) => avg1 < limit,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_uses_two_decimals_when_max_under_10() {
        assert_eq!(format_load_avg((1.23, 1.1, 0.98)), "1.23, 1.10, 0.98");
    }

    #[test]
    fn format_uses_one_decimal_when_max_under_100() {
        assert_eq!(format_load_avg((12.34, 5.0, 1.0)), "12.3, 5.0, 1.0");
    }

    #[test]
    fn format_uses_zero_decimals_when_max_at_least_100() {
        assert_eq!(format_load_avg((100.4, 50.0, 1.0)), "100, 50, 1");
    }

    #[test]
    fn can_start_with_no_limit_or_no_running() {
        assert!(can_start_under_load(None, 0));
        assert!(can_start_under_load(None, 4));
        assert!(can_start_under_load(Some(0.01), 0));
    }

    #[test]
    fn get_loadavg_readable_on_linux() {
        // Gentoo / CI Linux hosts expose /proc/loadavg.
        if cfg!(target_os = "linux") {
            let avg = get_loadavg();
            assert!(avg.is_some(), "expected /proc/loadavg on Linux");
            let (a, b, c) = avg.unwrap();
            assert!(a >= 0.0 && b >= 0.0 && c >= 0.0);
        }
    }
}
