//! Opt-in Portage-compatible `emerge.log` dual-write

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};

use super::bus::ActivitySink;
use super::event::{ActivityEvent, ActivityMode};

/// Appends classic emerge.log lines for qlop/genlop/emlop
///
/// Opt-in only (product decision): not on the default CLI bus.
pub struct EmergeLogSink {
    path: Utf8PathBuf,
    lock: Mutex<()>,
    /// Display ROOT for "to {root}" lines (session merge root)
    merge_root_display: Mutex<Arc<str>>,
    /// Current session mode — regen skips per-item lines (thousands of ebuilds)
    mode: Mutex<ActivityMode>,
}

impl EmergeLogSink {
    /// `log_path` e.g. `/var/log/emerge.log` or `<root>/var/log/emerge.log`
    pub fn new(log_path: impl Into<Utf8PathBuf>) -> Self {
        Self {
            path: log_path.into(),
            lock: Mutex::new(()),
            merge_root_display: Mutex::new("/".into()),
            mode: Mutex::new(ActivityMode::Merge),
        }
    }

    /// Default path under a merge root: `<root>/var/log/emerge.log`, or
    /// `/var/log/emerge.log` when root is `/`.
    pub fn for_merge_root(merge_root: &Utf8Path) -> Self {
        let path = if merge_root.as_str() == "/" || merge_root.as_str().is_empty() {
            Utf8PathBuf::from("/var/log/emerge.log")
        } else {
            merge_root.join("var/log/emerge.log")
        };
        let s = Self::new(path);
        *s.merge_root_display
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = merge_root.as_str().into();
        s
    }

    fn append_line(&self, body: &str) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let ts = ActivityEvent::now() as i64;
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent.as_std_path());
        }
        let res = (|| {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.path.as_std_path())?;
            writeln!(f, "{ts}: {body}")?;
            Ok::<(), std::io::Error>(())
        })();
        if let Err(e) = res {
            // Host /var/log often needs root — soft-fail.
            tracing::warn!("emergelog {}: {e}", self.path);
        }
    }

    fn mode(&self) -> ActivityMode {
        *self.mode.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl ActivitySink for EmergeLogSink {
    fn on_event(&self, event: &ActivityEvent) {
        match event {
            ActivityEvent::SessionStart {
                started_at,
                argv,
                merge_root,
                mode,
                ..
            } => {
                *self
                    .merge_root_display
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = merge_root.clone();
                *self.mode.lock().unwrap_or_else(|e| e.into_inner()) = *mode;
                let when = chrono_like(*started_at);
                self.append_line(&format!("Started emerge on: {when}"));
                let cmd = if argv.is_empty() {
                    "em".to_string()
                } else {
                    argv.join(" ")
                };
                let tag = match mode {
                    ActivityMode::Merge | ActivityMode::FetchOnly | ActivityMode::BuildpkgOnly => {
                        " *** emerge "
                    }
                    ActivityMode::Unmerge => " === Unmerging... ",
                    ActivityMode::Depclean => " >>> depclean ",
                    ActivityMode::Regen => " >>> regen ",
                };
                self.append_line(&format!("{tag}{cmd}"));
            }
            ActivityEvent::PkgStart { cpv, index, of, .. } => {
                if self.mode() == ActivityMode::Regen {
                    return;
                }
                let root = self
                    .merge_root_display
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                self.append_line(&format!(" >>> emerge ({index} of {of}) {cpv} to {root}"));
            }
            ActivityEvent::PhaseEnter { cpv, phase, .. } => {
                if self.mode() == ActivityMode::Regen {
                    return;
                }
                // Approximate Portage phase banners for the common cases.
                let label = match phase.as_ref() {
                    "fetch" => "Fetching",
                    "unpack" | "prepare" | "configure" | "compile" | "test" => "Compiling/Merging",
                    "install" => "Merging",
                    "qmerge" | "merge" => "Merging",
                    other => other,
                };
                self.append_line(&format!(" === {label} ({cpv})"));
            }
            ActivityEvent::PkgEnd { cpv, ok, .. } => {
                if self.mode() == ActivityMode::Regen {
                    return;
                }
                let root = self
                    .merge_root_display
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if *ok {
                    self.append_line(&format!(" ::: completed emerge {cpv} to {root}"));
                } else {
                    self.append_line(&format!(" !!! emerge FAILURE: {cpv}"));
                }
            }
            ActivityEvent::SessionEnd { ok, .. } => {
                if *ok {
                    self.append_line(" *** exiting successfully.");
                } else {
                    self.append_line(" *** exiting unsuccessfully with status '1'.");
                }
            }
            _ => {}
        }
    }
}

/// Local time in the shape `qlop`/`genlop` parse out of `emerge.log`'s
/// `Started emerge on:` line — `Feb 23, 2026 16:05:01`, zero-padded day.
fn chrono_like(unix: f64) -> String {
    let secs = unix.max(0.0) as i64;
    let nanos = ((unix.max(0.0) - secs as f64) * 1e9) as u32;
    chrono::DateTime::from_timestamp(secs, nanos)
        .map(|utc| {
            chrono::DateTime::<chrono::Local>::from(utc)
                .format("%b %d, %Y %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| format!("unix {secs}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `qlop`/`genlop` parse the `Started emerge on:` line, so the shape is a
    // wire format, not cosmetics. Asserted by parsing back rather than against
    // a literal string, which would only hold in one timezone.
    #[test]
    fn started_emerge_timestamp_round_trips_through_portages_format() {
        const EPOCH: i64 = 1_771_862_701; // Feb 2026, clear of any DST fold
        let rendered = chrono_like(EPOCH as f64);
        let naive = chrono::NaiveDateTime::parse_from_str(&rendered, "%b %d, %Y %H:%M:%S")
            .unwrap_or_else(|e| panic!("{rendered:?} is not emerge.log's shape: {e}"));
        let local = naive
            .and_local_timezone(chrono::Local)
            .single()
            .expect("unambiguous local time");
        assert_eq!(local.timestamp(), EPOCH, "rendered {rendered:?}");
    }
}
