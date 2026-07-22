//! Append-only JSONL duration history + ETA estimates.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Mutex;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use super::bus::ActivitySink;
use super::event::{ActivityEvent, ActivityMergeRoot, PhaseTiming, PkgKind};

/// One finished package action (one JSONL line).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HistoryRecord {
    pub ts_end: f64,
    pub job_id: String,
    pub cpn: String,
    pub cpv: String,
    pub merge_root: ActivityMergeRoot,
    pub kind: PkgKind,
    pub ok: bool,
    pub seconds: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<PhaseTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Appends [`HistoryRecord`]s on [`ActivityEvent::PkgEnd`].
pub struct HistorySink {
    path: Utf8PathBuf,
    /// Serialize multi-job appends on the same path within one process.
    lock: Mutex<()>,
}

impl HistorySink {
    pub fn new(merge_root: impl Into<Utf8PathBuf>) -> Self {
        let merge_root = merge_root.into();
        Self {
            path: merge_root.join("var/cache/edb/em-activity/history/merges.jsonl"),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    fn append(&self, rec: &HistoryRecord) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(parent) = self.path.parent()
            && let Err(e) = std::fs::create_dir_all(parent.as_std_path())
        {
            eprintln!("warning: activity history mkdir {parent}: {e}");
            return;
        }
        let line = match serde_json::to_string(rec) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: activity history serialise: {e}");
                return;
            }
        };
        let res = (|| {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.path.as_std_path())?;
            writeln!(f, "{line}")?;
            Ok::<(), std::io::Error>(())
        })();
        if let Err(e) = res {
            eprintln!("warning: activity history append {}: {e}", self.path);
        }
    }
}

impl ActivitySink for HistorySink {
    fn on_event(&self, event: &ActivityEvent) {
        let ActivityEvent::PkgEnd {
            job_id,
            cpv,
            cpn,
            merge_root,
            kind,
            ok,
            at,
            seconds,
            phases,
            error,
            ..
        } = event
        else {
            return;
        };
        self.append(&HistoryRecord {
            ts_end: *at,
            job_id: job_id.clone(),
            cpn: cpn.clone(),
            cpv: cpv.clone(),
            merge_root: *merge_root,
            kind: *kind,
            ok: *ok,
            seconds: *seconds,
            phases: phases.clone(),
            error: error.clone(),
        });
    }
}

/// Read-only view of history for `em log list` / `time` / ETA.
pub struct DurationStore {
    records: Vec<HistoryRecord>,
}

impl DurationStore {
    pub fn load(merge_root: &Utf8Path) -> Self {
        let path = merge_root.join("var/cache/edb/em-activity/history/merges.jsonl");
        Self::load_path(path.as_std_path())
    }

    pub fn load_path(path: &Path) -> Self {
        let mut records = Vec::new();
        let Ok(f) = std::fs::File::open(path) else {
            return Self { records };
        };
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(rec) = serde_json::from_str::<HistoryRecord>(line) {
                records.push(rec);
            }
        }
        Self { records }
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Most recent records first, optional limit.
    pub fn recent(&self, limit: Option<usize>) -> Vec<&HistoryRecord> {
        let n = self.records.len();
        let take = limit.unwrap_or(n).min(n);
        self.records[n.saturating_sub(take)..]
            .iter()
            .rev()
            .collect()
    }

    /// Successful durations for `cpn` (or substring match on cpn/cpv), newest first.
    pub fn successes_for_atom(&self, atom: &str) -> Vec<&HistoryRecord> {
        let atom = atom.trim();
        self.records
            .iter()
            .rev()
            .filter(|r| {
                r.ok && (r.cpn == atom
                    || r.cpv == atom
                    || r.cpn.contains(atom)
                    || r.cpv.contains(atom)
                    || atom.strip_prefix('=') == Some(r.cpv.as_str()))
            })
            .collect()
    }

    /// Last `k` successful durations for exact cpn (seconds), oldest→newest in window.
    pub fn recent_success_seconds(&self, cpn: &str, k: usize) -> Vec<f64> {
        let mut v: Vec<f64> = self
            .records
            .iter()
            .rev()
            .filter(|r| r.ok && r.cpn == cpn)
            .take(k)
            .map(|r| r.seconds)
            .collect();
        v.reverse();
        v
    }

    pub fn median_seconds(&self, cpn: &str, k: usize) -> Option<f64> {
        let mut s = self.recent_success_seconds(cpn, k);
        if s.is_empty() {
            return None;
        }
        s.sort_by(|a, b| a.total_cmp(b));
        let mid = s.len() / 2;
        Some(if s.len() % 2 == 1 {
            s[mid]
        } else {
            (s[mid - 1] + s[mid]) / 2.0
        })
    }

    /// Fallback: median of all successful merges (any cpn), last `k` overall.
    pub fn global_median_seconds(&self, k: usize) -> Option<f64> {
        let mut s: Vec<f64> = self
            .records
            .iter()
            .rev()
            .filter(|r| r.ok)
            .take(k)
            .map(|r| r.seconds)
            .collect();
        if s.is_empty() {
            return None;
        }
        s.sort_by(|a, b| a.total_cmp(b));
        let mid = s.len() / 2;
        Some(if s.len() % 2 == 1 {
            s[mid]
        } else {
            (s[mid - 1] + s[mid]) / 2.0
        })
    }
}

/// One remaining package in a plan for ETA.
#[derive(Clone, Debug)]
pub struct EtaPkg {
    pub cpn: String,
    pub cpv: String,
}

/// Result of [`estimate_remaining`].
#[derive(Clone, Debug)]
pub struct Eta {
    /// Estimated wall seconds (after / jobs).
    pub wall_seconds: f64,
    /// Sum of per-package serial estimates.
    pub serial_seconds: f64,
    pub jobs: u32,
    pub known: u32,
    pub unknown: u32,
    /// Per-package serial estimates (same order as input).
    pub per_pkg: Vec<(String, Option<f64>)>,
}

/// Median of last `k` successes per cpn; unknown use global median if any.
/// Wall time ≈ serial / max(jobs, 1).
pub fn estimate_remaining(store: &DurationStore, pkgs: &[EtaPkg], jobs: u32, k: usize) -> Eta {
    let global = store.global_median_seconds(k.max(20));
    let mut serial = 0.0;
    let mut known = 0u32;
    let mut unknown = 0u32;
    let mut per_pkg = Vec::with_capacity(pkgs.len());
    for p in pkgs {
        let est = store.median_seconds(&p.cpn, k).or(global);
        match est {
            Some(s) => {
                serial += s;
                known += 1;
                per_pkg.push((p.cpv.clone(), Some(s)));
            }
            None => {
                unknown += 1;
                per_pkg.push((p.cpv.clone(), None));
            }
        }
    }
    let j = jobs.max(1) as f64;
    Eta {
        wall_seconds: serial / j,
        serial_seconds: serial,
        jobs: jobs.max(1),
        known,
        unknown,
        per_pkg,
    }
}

pub fn format_seconds(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "?".into();
    }
    let s = secs.round() as u64;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// Format `em log list`.
pub fn format_list(store: &DurationStore, limit: Option<usize>) -> String {
    let rows = store.recent(limit.or(Some(20)));
    if rows.is_empty() {
        return "No activity history yet.\n".into();
    }
    let mut out = String::new();
    for r in rows {
        let status = if r.ok { "ok" } else { "FAIL" };
        out.push_str(&format!(
            "{:>10}  {status:<4}  {cpv}  ({kind:?})\n",
            format_seconds(r.seconds),
            cpv = r.cpv,
            kind = r.kind,
        ));
    }
    out
}

/// Format `em log time [atom]`.
pub fn format_time(store: &DurationStore, atom: Option<&str>) -> String {
    match atom {
        None => {
            let Some(m) = store.global_median_seconds(50) else {
                return "No successful merges recorded yet.\n".into();
            };
            format!(
                "global median (last successes): {}\n{} record(s) total\n",
                format_seconds(m),
                store.len()
            )
        }
        Some(a) => {
            let rows = store.successes_for_atom(a);
            if rows.is_empty() {
                return format!("No successful history matching '{a}'.\n");
            }
            let secs: Vec<f64> = rows.iter().map(|r| r.seconds).collect();
            let mut sorted = secs.clone();
            sorted.sort_by(|x, y| x.total_cmp(y));
            let mid = sorted.len() / 2;
            let median = if sorted.len() % 2 == 1 {
                sorted[mid]
            } else {
                (sorted[mid - 1] + sorted[mid]) / 2.0
            };
            let mean = secs.iter().sum::<f64>() / secs.len() as f64;
            let mut out = format!(
                "{a}: n={}  median={}  mean={}  last={}\n",
                secs.len(),
                format_seconds(median),
                format_seconds(mean),
                format_seconds(secs[0]),
            );
            for r in rows.iter().take(10) {
                out.push_str(&format!("  {:>10}  {}\n", format_seconds(r.seconds), r.cpv));
            }
            if rows.len() > 10 {
                out.push_str(&format!("  … {} more\n", rows.len() - 10));
            }
            out
        }
    }
}

/// Format ETA for human output.
pub fn format_eta(eta: &Eta) -> String {
    let mut out = format!(
        "ETA ~{} wall ({} serial / {} job{}) — {} known, {} unknown package time(s)\n",
        format_seconds(eta.wall_seconds),
        format_seconds(eta.serial_seconds),
        eta.jobs,
        if eta.jobs == 1 { "" } else { "s" },
        eta.known,
        eta.unknown,
    );
    if eta.unknown > 0 && eta.known == 0 {
        out.push_str("(no history yet — estimates unavailable)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::bus::ActivityBus;
    use crate::activity::event::ACTIVITY_EVENT_VERSION;
    use std::sync::Arc;

    #[test]
    fn history_sink_and_eta() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let bus = ActivityBus::new();
        bus.add_sink(Arc::new(HistorySink::new(root.clone())));

        for (i, secs) in [(1.0_f64, 10.0), (2.0, 20.0), (3.0, 30.0)] {
            bus.emit(ActivityEvent::PkgEnd {
                v: ACTIVITY_EVENT_VERSION,
                job_id: "j".into(),
                parent_job_id: None,
                cpv: format!("sys-apps/foo-{i}"),
                cpn: "sys-apps/foo".into(),
                merge_root: ActivityMergeRoot::Target,
                kind: PkgKind::Source,
                ok: true,
                at: 1000.0 + i,
                seconds: secs,
                phases: vec![],
                error: None,
            });
        }
        let store = DurationStore::load(&root);
        assert_eq!(store.len(), 3);
        assert_eq!(store.median_seconds("sys-apps/foo", 10), Some(20.0));

        let eta = estimate_remaining(
            &store,
            &[
                EtaPkg {
                    cpn: "sys-apps/foo".into(),
                    cpv: "sys-apps/foo-9".into(),
                },
                EtaPkg {
                    cpn: "sys-apps/foo".into(),
                    cpv: "sys-apps/foo-10".into(),
                },
            ],
            2,
            10,
        );
        assert_eq!(eta.known, 2);
        assert!((eta.serial_seconds - 40.0).abs() < 0.01);
        assert!((eta.wall_seconds - 20.0).abs() < 0.01);
    }
}
