//! Append-only JSONL duration history + ETA estimates.

use std::collections::{BinaryHeap, VecDeque};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Mutex;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use super::bus::ActivitySink;
use super::event::{ActivityEvent, ActivityMergeRoot, ActivityMode, PhaseTiming, PkgKind};

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
///
/// Skips [`ActivityMode::Regen`] sessions — regenerating thousands of ebuilds
/// must not pollute the merge-duration history used for ETA.
pub struct HistorySink {
    path: Utf8PathBuf,
    /// Serialize multi-job appends on the same path within one process.
    lock: Mutex<()>,
    /// job_id → mode from the most recent [`ActivityEvent::SessionStart`].
    modes: Mutex<std::collections::HashMap<String, ActivityMode>>,
}

impl HistorySink {
    pub fn new(merge_root: impl Into<Utf8PathBuf>) -> Self {
        let merge_root = merge_root.into();
        Self {
            path: merge_root.join("var/cache/edb/em-activity/history/merges.jsonl"),
            lock: Mutex::new(()),
            modes: Mutex::new(std::collections::HashMap::new()),
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
            crate::style::warn_line(&format!("activity history mkdir {parent}: {e}"));
            return;
        }
        let line = match serde_json::to_string(rec) {
            Ok(s) => s,
            Err(e) => {
                crate::style::warn_line(&format!("activity history serialise: {e}"));
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
            crate::style::warn_line(&format!("activity history append {}: {e}", self.path));
        }
    }
}

impl ActivitySink for HistorySink {
    fn on_event(&self, event: &ActivityEvent) {
        match event {
            ActivityEvent::SessionStart { job_id, mode, .. } => {
                self.modes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(job_id.clone(), *mode);
            }
            ActivityEvent::SessionEnd { job_id, .. } => {
                self.modes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(job_id);
            }
            ActivityEvent::PkgEnd {
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
            } => {
                let mode = self
                    .modes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(job_id)
                    .copied()
                    .unwrap_or(ActivityMode::Merge);
                if mode == ActivityMode::Regen {
                    return;
                }
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
            _ => {}
        }
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

/// Result of [`estimate_remaining`] / [`estimate_remaining_with_blockers`].
#[derive(Clone, Debug)]
pub struct Eta {
    /// Estimated wall-clock seconds (parallel schedule).
    pub wall_seconds: f64,
    /// Sum of per-package serial estimates.
    pub serial_seconds: f64,
    pub jobs: u32,
    pub known: u32,
    pub unknown: u32,
    /// True when wall used the build-graph critical-path scheduler.
    pub critical_path: bool,
    /// Per-package serial estimates (same order as input).
    pub per_pkg: Vec<(String, Option<f64>)>,
}

/// `(durations, known, unknown, per_pkg, serial_sum)`.
type PackageEstimates = (Vec<f64>, u32, u32, Vec<(String, Option<f64>)>, f64);

fn package_estimates(store: &DurationStore, pkgs: &[EtaPkg], k: usize) -> PackageEstimates {
    let global = store.global_median_seconds(k.max(20));
    let mut serial = 0.0;
    let mut known = 0u32;
    let mut unknown = 0u32;
    let mut per_pkg = Vec::with_capacity(pkgs.len());
    let mut durations = Vec::with_capacity(pkgs.len());
    for p in pkgs {
        let est = store.median_seconds(&p.cpn, k).or(global);
        match est {
            Some(s) => {
                serial += s;
                known += 1;
                per_pkg.push((p.cpv.clone(), Some(s)));
                durations.push(s);
            }
            None => {
                unknown += 1;
                per_pkg.push((p.cpv.clone(), None));
                // Unknown packages contribute 0 to wall (same as serial pad);
                // critical-path still places them so dependents wait.
                durations.push(0.0);
            }
        }
    }
    (durations, known, unknown, per_pkg, serial)
}

/// Median of last `k` successes per cpn; unknown use global median if any.
/// Wall time ≈ serial / max(jobs, 1) (no build-order constraints).
pub fn estimate_remaining(store: &DurationStore, pkgs: &[EtaPkg], jobs: u32, k: usize) -> Eta {
    let (durations, known, unknown, per_pkg, serial) = package_estimates(store, pkgs, k);
    let j = jobs.max(1) as f64;
    let _ = durations;
    Eta {
        wall_seconds: serial / j,
        serial_seconds: serial,
        jobs: jobs.max(1),
        known,
        unknown,
        critical_path: false,
        per_pkg,
    }
}

/// Like [`estimate_remaining`], but wall time is a **list-schedule simulation**
/// of `jobs` workers over the build-order graph.
///
/// `blockers[i]` lists earlier plan indices that must finish before `i` can
/// start — the same shape `run_merge_plan` uses for `--jobs`.
pub fn estimate_remaining_with_blockers(
    store: &DurationStore,
    pkgs: &[EtaPkg],
    blockers: &[Vec<usize>],
    jobs: u32,
    k: usize,
) -> Eta {
    let (durations, known, unknown, per_pkg, serial) = package_estimates(store, pkgs, k);
    let n = pkgs.len();
    let wall = if n == 0 {
        0.0
    } else if blockers.len() != n {
        // Graph missing/mismatched — fall back to naive.
        serial / jobs.max(1) as f64
    } else {
        critical_path_wall(&durations, blockers, jobs.max(1) as usize)
    };
    Eta {
        wall_seconds: wall,
        serial_seconds: serial,
        jobs: jobs.max(1),
        known,
        unknown,
        critical_path: blockers.len() == n && n > 0,
        per_pkg,
    }
}

/// Discrete-event simulation: up to `jobs` concurrent builds; a node starts
/// only after all `blockers[i]` predecessors have finished.
fn critical_path_wall(durations: &[f64], blockers: &[Vec<usize>], jobs: usize) -> f64 {
    let n = durations.len();
    if n == 0 {
        return 0.0;
    }
    let mut remaining_preds: Vec<usize> = blockers.iter().map(Vec::len).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, preds) in blockers.iter().enumerate() {
        for &p in preds {
            if p < n {
                dependents[p].push(i);
            }
        }
    }
    let mut ready: VecDeque<usize> = (0..n).filter(|&i| remaining_preds[i] == 0).collect();
    // (finish_time, node)
    let mut running: BinaryHeap<std::cmp::Reverse<(OrderedF64, usize)>> = BinaryHeap::new();
    let mut done = 0usize;
    let mut time = 0.0_f64;

    while done < n {
        while running.len() < jobs {
            let Some(i) = ready.pop_front() else {
                break;
            };
            let finish = time + durations[i].max(0.0);
            running.push(std::cmp::Reverse((OrderedF64(finish), i)));
        }
        let Some(std::cmp::Reverse((OrderedF64(finish), i))) = running.pop() else {
            // Deadlock (cycle) — should not happen with merge blockers; bail serial.
            return durations.iter().sum();
        };
        time = finish;
        done += 1;
        for &d in &dependents[i] {
            remaining_preds[d] = remaining_preds[d].saturating_sub(1);
            if remaining_preds[d] == 0 {
                ready.push_back(d);
            }
        }
    }
    time
}

/// Total-order wrapper so finish times can sit in a heap.
#[derive(Clone, Copy, Debug)]
struct OrderedF64(f64);

impl PartialEq for OrderedF64 {
    fn eq(&self, other: &Self) -> bool {
        self.0.total_cmp(&other.0) == std::cmp::Ordering::Equal
    }
}
impl Eq for OrderedF64 {}
impl PartialOrd for OrderedF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for OrderedF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
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

/// Format ETA for human output. Only the headline value (`unknown`, or the
/// `~time` estimate) is bold, matching how `Total:`/`Size of downloads:`
/// bold their own headline numbers — everything else on this line, and the
/// whole detail line below it, stays plain.
pub fn format_eta(eta: &Eta) -> String {
    use crate::style::C_BOLD;
    use std::fmt::Write as _;

    // No build history for any planned package: a soft warning, not a fake
    // ~0s estimate. `em -p --eta` on a never-built package must not read as
    // "instant".
    if eta.known == 0 && eta.unknown > 0 {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "Expected time of completion: {C_BOLD}unknown{C_BOLD:#} — no build history for {} package{} yet",
            eta.unknown,
            if eta.unknown == 1 { "" } else { "s" },
        );
        return out;
    }

    let mode = if eta.critical_path {
        "critical-path"
    } else {
        "naive serial/jobs"
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Expected time of completion: ~{C_BOLD}{}{C_BOLD:#} wall ({mode}, {} job{})",
        format_seconds(eta.wall_seconds),
        eta.jobs,
        if eta.jobs == 1 { "" } else { "s" },
    );
    let unknown_suffix = if eta.unknown > 0 {
        format!(", {} unknown package time(s)", eta.unknown)
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        "  {} serial, {} known{unknown_suffix}",
        format_seconds(eta.serial_seconds),
        eta.known,
    );
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

        let pkgs = [
            EtaPkg {
                cpn: "sys-apps/foo".into(),
                cpv: "sys-apps/foo-9".into(),
            },
            EtaPkg {
                cpn: "sys-apps/foo".into(),
                cpv: "sys-apps/foo-10".into(),
            },
        ];
        let eta = estimate_remaining(&store, &pkgs, 2, 10);
        assert_eq!(eta.known, 2);
        assert!((eta.serial_seconds - 40.0).abs() < 0.01);
        assert!((eta.wall_seconds - 20.0).abs() < 0.01);
        assert!(!eta.critical_path);

        // Chain 0→1 with 2 jobs: still serial wall = 40.
        let chain = [vec![], vec![0]];
        let eta_cp = estimate_remaining_with_blockers(&store, &pkgs, &chain, 2, 10);
        assert!(eta_cp.critical_path);
        assert!((eta_cp.wall_seconds - 40.0).abs() < 0.01);

        // Independent packages with 2 jobs: wall ≈ 20.
        let indep = [vec![], vec![]];
        let eta_par = estimate_remaining_with_blockers(&store, &pkgs, &indep, 2, 10);
        assert!((eta_par.wall_seconds - 20.0).abs() < 0.01);
    }

    #[test]
    fn format_eta_says_unknown_when_no_history() {
        // A plan with no recorded build times must not claim a ~0s wall.
        let eta = Eta {
            wall_seconds: 0.0,
            serial_seconds: 0.0,
            jobs: 1,
            known: 0,
            unknown: 2,
            critical_path: true,
            per_pkg: vec![],
        };
        let s = format_eta(&eta);
        assert!(s.contains("unknown"), "got: {s}");
        assert!(
            !s.contains('~'),
            "no-history ETA must not show a numeric wall estimate: {s}"
        );
    }

    #[test]
    fn format_eta_shows_estimate_when_some_known() {
        let eta = Eta {
            wall_seconds: 40.0,
            serial_seconds: 40.0,
            jobs: 1,
            known: 2,
            unknown: 0,
            critical_path: true,
            per_pkg: vec![],
        };
        let s = format_eta(&eta);
        assert!(s.contains('~'), "got: {s}");
        assert!(s.contains("critical-path"));
    }
}
