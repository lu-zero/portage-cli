//! On-disk live status sink — concurrent-friendly per-package files

use std::path::Path;
use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::{Cpn, Cpv};
use serde::Serialize;

use super::bus::ActivitySink;
use super::event::{
    ActivityEvent, ActivityMergeRoot, ActivityMode, ActivityPlanPkg, PkgKind, SessionFlags,
};
use super::projection::LiveProjection;
use crate::util::write_atomic;

/// Writes `var/cache/edb/em-activity/live/<job_id>/…` under a merge root
pub struct LiveFsSink {
    /// Absolute path of the merge root that owns this activity tree
    activity_root: Utf8PathBuf,
    /// Keep a projection so phase updates can rewrite inflight JSON without
    /// re-reading every field from the event alone.
    state: Mutex<LiveProjection>,
    /// Set on the first write failure (e.g. an unwritable `EROOT` when not
    /// running as root) so the rest of the session neither repeats the same
    /// warning nor keeps paying for doomed writes — a single merge emits a
    /// `SessionStart`/heartbeats/`PkgStart`/`PhaseEnter`/`PhaseLeave` per
    /// phase/`PkgEnd`, and a broken activity root would otherwise print one
    /// identical line per event, sometimes hundreds of them.
    disabled: std::sync::atomic::AtomicBool,
}

impl LiveFsSink {
    /// `merge_root` is the EROOT that owns `var/cache/edb/em-activity/`
    pub fn new(merge_root: impl Into<Utf8PathBuf>) -> Self {
        Self {
            activity_root: merge_root.into(),
            state: Mutex::new(LiveProjection::new()),
            disabled: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn live_base(&self) -> Utf8PathBuf {
        self.activity_root.join("var/cache/edb/em-activity/live")
    }

    fn session_dir(&self, job_id: &str) -> Utf8PathBuf {
        self.live_base().join(job_id)
    }

    fn session_path(&self, job_id: &str) -> Utf8PathBuf {
        self.session_dir(job_id).join("session.json")
    }

    fn progress_path(&self, job_id: &str) -> Utf8PathBuf {
        self.session_dir(job_id).join("progress.json")
    }

    fn inflight_path(&self, job_id: &str, merge_root: ActivityMergeRoot, cpv: &Cpv) -> Utf8PathBuf {
        self.session_dir(job_id)
            .join("inflight")
            .join(merge_root.as_str())
            .join(cpv.cpn.category.as_str())
            .join(format!("{}-{}.json", cpv.cpn.package, cpv.version))
    }

    fn write_json(&self, path: &Utf8Path, value: &impl Serialize) {
        use std::sync::atomic::Ordering;
        if self.disabled.load(Ordering::Relaxed) {
            return;
        }
        match serde_json::to_string(value) {
            Ok(s) => {
                if let Err(e) = write_atomic(path, s) {
                    self.disabled.store(true, Ordering::Relaxed);
                    tracing::warn!(
                        "activity live fs write {path}: {e:#} — disabling live activity tracking for this session"
                    );
                }
            }
            Err(e) => tracing::warn!("activity live fs serialise: {e}"),
        }
    }

    /// Static job metadata — written once, at `SessionStart` only
    ///
    /// `plan` and `blockers` can be large, so this must never be rewritten per-package: that
    /// O(N) reserialize-and-rewrite, once per `PkgStart`/`PkgEnd`, made a 32,637-ebuild
    /// `em regen` hang indefinitely.
    fn write_session_file(&self, job_id: &str) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = state.get(job_id) else {
            return;
        };
        #[derive(Serialize)]
        struct SessionFile<'a> {
            job_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            parent_job_id: &'a Option<Arc<str>>,
            pid: u32,
            started_at: f64,
            argv: &'a [String],
            merge_root: &'a str,
            host_root: &'a str,
            #[serde(skip_serializing_if = "str::is_empty")]
            base_root: &'a str,
            mode: ActivityMode,
            plan_total: u32,
            flags: &'a SessionFlags,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            plan: &'a Vec<ActivityPlanPkg>,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            blockers: &'a Vec<Vec<usize>>,
        }
        let path = self.session_path(job_id);
        self.write_json(
            &path,
            &SessionFile {
                job_id: &s.job_id,
                parent_job_id: &s.parent_job_id,
                pid: s.pid,
                started_at: s.started_at,
                argv: &s.argv,
                merge_root: &s.merge_root,
                host_root: &s.host_root,
                base_root: &s.base_root,
                mode: s.mode,
                plan_total: s.plan_total,
                flags: &s.flags,
                plan: &s.plan,
                blockers: &s.blockers,
            },
        );
    }

    /// Dynamic progress — rewritten on every `SessionHeartbeat`/`PkgStart`/ `PkgEnd`
    ///
    /// Genuinely O(1) per write: none of these three fields grow.
    fn write_progress_file(&self, job_id: &str) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = state.get(job_id) else {
            return;
        };
        #[derive(Serialize)]
        struct ProgressFile {
            completed: u32,
            failed: u32,
            heartbeat_at: f64,
        }
        let path = self.progress_path(job_id);
        self.write_json(
            &path,
            &ProgressFile {
                completed: s.completed,
                failed: s.failed,
                heartbeat_at: ActivityEvent::now(),
            },
        );
    }

    fn write_inflight(&self, job_id: &str, merge_root: ActivityMergeRoot, cpv: &Arc<Cpv>) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = state.get(job_id) else {
            return;
        };
        let Some(p) = s.inflight.get(&(merge_root, cpv.clone())) else {
            return;
        };
        #[derive(Serialize)]
        struct InflightFile<'a> {
            cpv: &'a Cpv,
            cpn: Cpn,
            merge_root: ActivityMergeRoot,
            index: u32,
            of: u32,
            kind: PkgKind,
            pkg_started_at: f64,
            phase: &'a str,
            phase_started_at: f64,
            phases_done: &'a [(Arc<str>, f64)],
        }
        let path = self.inflight_path(job_id, merge_root, cpv);
        self.write_json(
            &path,
            &InflightFile {
                cpv: &p.cpv,
                cpn: p.cpn,
                merge_root: p.merge_root,
                index: p.index,
                of: p.of,
                kind: p.kind,
                pkg_started_at: p.pkg_started_at,
                phase: &p.phase,
                phase_started_at: p.phase_started_at,
                phases_done: &p.phases_done,
            },
        );
    }

    fn remove_inflight(&self, job_id: &str, merge_root: ActivityMergeRoot, cpv: &Cpv) {
        let path = self.inflight_path(job_id, merge_root, cpv);
        let _ = std::fs::remove_file(path.as_std_path());
    }

    fn remove_session(&self, job_id: &str) {
        let dir = self.session_dir(job_id);
        let _ = std::fs::remove_dir_all(dir.as_std_path());
    }
}

impl ActivitySink for LiveFsSink {
    fn on_event(&self, event: &ActivityEvent) {
        // Look up inflight presence *before* state.apply() removes it, so
        // remove_inflight can skip the unlink when there was never a file
        // (e.g. every PkgEnd `em regen` fires — regen never emits PkgStart).
        let had_inflight = if let ActivityEvent::PkgEnd {
            job_id,
            cpv,
            merge_root,
            ..
        } = event
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .get(job_id)
                .is_some_and(|s| s.inflight.contains_key(&(*merge_root, cpv.clone())))
        } else {
            false
        };
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.apply(event);
        }
        match event {
            ActivityEvent::SessionStart { job_id, .. } => {
                self.write_session_file(job_id);
                self.write_progress_file(job_id);
            }
            ActivityEvent::SessionHeartbeat { job_id, .. } => {
                self.write_progress_file(job_id);
            }
            ActivityEvent::SessionEnd { job_id, .. } => {
                self.remove_session(job_id);
            }
            ActivityEvent::PkgStart {
                job_id,
                cpv,
                merge_root,
                ..
            } => {
                self.write_inflight(job_id, *merge_root, cpv);
                self.write_progress_file(job_id);
            }
            ActivityEvent::PhaseEnter {
                job_id,
                cpv,
                merge_root,
                ..
            }
            | ActivityEvent::PhaseLeave {
                job_id,
                cpv,
                merge_root,
                ..
            } => {
                self.write_inflight(job_id, *merge_root, cpv);
            }
            ActivityEvent::PkgEnd {
                job_id,
                cpv,
                merge_root,
                ..
            } => {
                if had_inflight {
                    self.remove_inflight(job_id, *merge_root, cpv);
                }
                self.write_progress_file(job_id);
            }
            // Diagnostics are transient; not part of the on-disk snapshot.
            ActivityEvent::Diagnostic { .. } => {}
        }
    }
}

/// List live session directories under `merge_root` and rebuild a projection
/// from session.json + inflight files (for `em log current` without a bus).
pub fn load_live_from_disk(merge_root: &Utf8Path) -> LiveProjection {
    let mut proj = LiveProjection::new();
    let live = merge_root.join("var/cache/edb/em-activity/live");
    let Ok(entries) = std::fs::read_dir(live.as_std_path()) else {
        return proj;
    };
    for ent in entries.flatten() {
        if !ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(_dir_name) = ent.file_name().into_string() else {
            continue;
        };
        let session_path = ent.path().join("session.json");
        let Ok(text) = std::fs::read_to_string(&session_path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<SessionDisk>(&text) else {
            continue;
        };
        let progress: Option<ProgressDisk> =
            std::fs::read_to_string(ent.path().join("progress.json"))
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok());

        // Rebuild via SessionStart then synthetic PkgStart/PhaseEnter from inflight files.
        proj.apply(&ActivityEvent::SessionStart {
            v: super::event::ACTIVITY_EVENT_VERSION,
            job_id: meta.job_id.clone(),
            parent_job_id: meta.parent_job_id.clone(),
            pid: meta.pid,
            started_at: meta.started_at,
            argv: meta.argv.clone(),
            merge_root: meta.merge_root.clone(),
            host_root: meta.host_root.clone(),
            base_root: meta.base_root.clone(),
            mode: meta.mode,
            plan_total: meta.plan_total,
            flags: meta.flags.clone(),
            plan: meta.plan,
            blockers: meta.blockers,
        });
        // Patch completed/failed from progress.json (SessionStart alone has none).
        let (completed, failed, heartbeat_at) = match &progress {
            Some(p) => (p.completed, p.failed, p.heartbeat_at),
            None => (0, 0, meta.started_at),
        };
        proj.apply(&ActivityEvent::SessionHeartbeat {
            v: super::event::ACTIVITY_EVENT_VERSION,
            job_id: meta.job_id.clone(),
            parent_job_id: meta.parent_job_id.clone(),
            at: heartbeat_at,
            completed,
            failed,
        });

        let inflight_root = ent.path().join("inflight");
        load_inflight_dir(
            &mut proj,
            &meta.job_id,
            &inflight_root,
            ActivityMergeRoot::Host,
        );
        load_inflight_dir(
            &mut proj,
            &meta.job_id,
            &inflight_root,
            ActivityMergeRoot::Target,
        );
    }
    proj
}

#[derive(serde::Deserialize)]
struct SessionDisk {
    job_id: Arc<str>,
    #[serde(default)]
    parent_job_id: Option<Arc<str>>,
    pid: u32,
    started_at: f64,
    argv: Vec<String>,
    merge_root: Arc<str>,
    host_root: Arc<str>,
    #[serde(default)]
    base_root: Arc<str>,
    mode: ActivityMode,
    plan_total: u32,
    flags: SessionFlags,
    #[serde(default)]
    plan: Vec<ActivityPlanPkg>,
    #[serde(default)]
    blockers: Vec<Vec<usize>>,
}

#[derive(serde::Deserialize)]
struct ProgressDisk {
    #[serde(default)]
    completed: u32,
    #[serde(default)]
    failed: u32,
    #[serde(default)]
    heartbeat_at: f64,
}

#[derive(serde::Deserialize)]
struct InflightDisk {
    cpv: Arc<Cpv>,
    cpn: Cpn,
    merge_root: ActivityMergeRoot,
    index: u32,
    of: u32,
    kind: PkgKind,
    pkg_started_at: f64,
    phase: Arc<str>,
    phase_started_at: f64,
    #[serde(default)]
    phases_done: Vec<(Arc<str>, f64)>,
}

fn load_inflight_dir(
    proj: &mut LiveProjection,
    job_id: &str,
    inflight_root: &Path,
    side: ActivityMergeRoot,
) {
    let side_dir = inflight_root.join(side.as_str());
    let Ok(cats) = std::fs::read_dir(&side_dir) else {
        return;
    };
    for cat_ent in cats.flatten() {
        if !cat_ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(pfs) = std::fs::read_dir(cat_ent.path()) else {
            continue;
        };
        for pf_ent in pfs.flatten() {
            let path = pf_ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(p) = serde_json::from_str::<InflightDisk>(&text) else {
                continue;
            };
            proj.apply(&ActivityEvent::PkgStart {
                v: super::event::ACTIVITY_EVENT_VERSION,
                job_id: job_id.into(),
                parent_job_id: None,
                cpv: p.cpv.clone(),
                cpn: p.cpn,
                merge_root: p.merge_root,
                index: p.index,
                of: p.of,
                kind: p.kind,
                at: p.pkg_started_at,
            });
            for (phase, secs) in &p.phases_done {
                proj.apply(&ActivityEvent::PhaseLeave {
                    v: super::event::ACTIVITY_EVENT_VERSION,
                    job_id: job_id.into(),
                    parent_job_id: None,
                    cpv: p.cpv.clone(),
                    merge_root: p.merge_root,
                    phase: phase.clone(),
                    at: p.pkg_started_at,
                    seconds: *secs,
                });
            }
            proj.apply(&ActivityEvent::PhaseEnter {
                v: super::event::ACTIVITY_EVENT_VERSION,
                job_id: job_id.into(),
                parent_job_id: None,
                cpv: p.cpv.clone(),
                merge_root: p.merge_root,
                phase: p.phase.clone(),
                at: p.phase_started_at,
            });
        }
    }
}

/// True if `pid` is still running (best-effort; Unix)
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let Some(pid) = rustix::process::Pid::from_raw(pid as i32) else {
        return false;
    };
    // kill(pid, 0) — existence check
    rustix::process::test_kill_process(pid).is_ok()
}
