//! Structured activity events for merge progress (library channel + sinks).
//!
//! See `todo/activity-status.md`. Call sites emit via [`ActivityBus`]; front-ends
//! subscribe or read the live FS sink. emerge.log is not the control plane.

mod bus;
mod emergelog;
mod event;
mod history;
mod jsonl_fd;
mod live_fs;
mod projection;

pub use bus::{ActivityBus, ActivitySink, RecordingSink};
pub use emergelog::EmergeLogSink;
pub use event::{
    ACTIVITY_EVENT_VERSION, ActivityEvent, ActivityMergeRoot, ActivityMode, ActivityPlanPkg,
    ActivitySessionOpts, PhaseTiming, PkgKind, SessionFlags,
};
pub use history::{
    DurationStore, Eta, EtaPkg, HistoryRecord, HistorySink, estimate_remaining,
    estimate_remaining_with_blockers, format_eta, format_list, format_seconds, format_time,
};
pub use jsonl_fd::JsonlFdSink;
pub use live_fs::{LiveFsSink, load_live_from_disk, pid_alive};
pub use projection::{InflightPkg, LiveProjection, LiveSession};

use std::sync::{Arc, Mutex};

use camino::Utf8Path;

/// Per-package activity handle passed into the ebuild phase runner.
///
/// Holds enough identity to emit [`ActivityEvent::PhaseEnter`] /
/// [`ActivityEvent::PhaseLeave`] without the phase loop knowing about the
/// full session. `phases` accumulates leave timings for a richer
/// [`ActivityEvent::PkgEnd`] if the caller wants them.
///
/// `live_root` is the filesystem root of the session's live FS sink (where
/// `var/cache/edb/em-activity/live/` lives). Install workers re-open that
/// tree with a LiveFs-only bus so phase updates continue across the process
/// boundary; Session/PkgStart/PkgEnd stay on the parent.
#[derive(Clone)]
pub struct ActivityPkgCtx {
    pub bus: ActivityBus,
    pub job_id: String,
    pub parent_job_id: Option<String>,
    pub cpv: String,
    pub merge_root: ActivityMergeRoot,
    /// Session live-FS root (`None` when activity is in-process only / tests).
    pub live_root: Option<camino::Utf8PathBuf>,
    phases: Arc<Mutex<Vec<PhaseTiming>>>,
}

impl ActivityPkgCtx {
    pub fn new(
        bus: ActivityBus,
        job_id: String,
        parent_job_id: Option<String>,
        cpv: String,
        merge_root: ActivityMergeRoot,
    ) -> Self {
        Self {
            bus,
            job_id,
            parent_job_id,
            cpv,
            merge_root,
            live_root: None,
            phases: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Set the session live-FS root so an install worker can mirror phases.
    pub fn with_live_root(mut self, root: impl Into<camino::Utf8PathBuf>) -> Self {
        self.live_root = Some(root.into());
        self
    }

    pub fn phase_enter(&self, phase: &str) -> f64 {
        let at = ActivityEvent::now();
        self.bus.emit(ActivityEvent::PhaseEnter {
            v: ACTIVITY_EVENT_VERSION,
            job_id: self.job_id.clone(),
            parent_job_id: self.parent_job_id.clone(),
            cpv: self.cpv.clone(),
            merge_root: self.merge_root,
            phase: phase.to_string(),
            at,
        });
        at
    }

    pub fn phase_leave(&self, phase: &str, started: f64) {
        let at = ActivityEvent::now();
        let seconds = (at - started).max(0.0);
        self.phases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(PhaseTiming {
                phase: phase.to_string(),
                seconds,
            });
        self.bus.emit(ActivityEvent::PhaseLeave {
            v: ACTIVITY_EVENT_VERSION,
            job_id: self.job_id.clone(),
            parent_job_id: self.parent_job_id.clone(),
            cpv: self.cpv.clone(),
            merge_root: self.merge_root,
            phase: phase.to_string(),
            at,
            seconds,
        });
    }

    pub fn phases_done(&self) -> Vec<PhaseTiming> {
        self.phases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Build the default CLI bus: live FS + history JSONL under `merge_root`
/// (no emerge.log — opt-in only).
pub fn default_cli_bus(merge_root: &Utf8Path) -> ActivityBus {
    let bus = ActivityBus::new();
    bus.add_sink(Arc::new(LiveFsSink::new(merge_root.to_owned())));
    bus.add_sink(Arc::new(HistorySink::new(merge_root.to_owned())));
    bus
}

/// Install-worker bus: LiveFs + optional JSONL re-emit path (Unix socket or
/// pipe write end). Parent owns Session/PkgStart/PkgEnd and history; the child
/// refreshes phase on the shared live tree and streams the same events back so
/// parent `--activity-fd` / subscribers see install/qmerge phases.
pub fn worker_activity_bus(live_root: &Utf8Path, reemit_path: Option<&str>) -> ActivityBus {
    let bus = ActivityBus::new();
    bus.add_sink(Arc::new(LiveFsSink::new(live_root.to_owned())));
    if let Some(path) = reemit_path.filter(|p| !p.is_empty()) {
        match JsonlFdSink::connect_reemit(path) {
            Ok(sink) => bus.add_sink(Arc::new(sink)),
            Err(e) => eprintln!("warning: activity re-emit connect {path}: {e}"),
        }
    }
    bus
}

/// Back-compat alias: LiveFs only (no re-emit).
pub fn worker_live_bus(live_root: &Utf8Path) -> ActivityBus {
    worker_activity_bus(live_root, None)
}

/// Attach optional JSONL FD / path sinks to an existing bus.
pub fn attach_jsonl_outputs(
    bus: &ActivityBus,
    activity_fd: Option<i32>,
    activity_jsonl: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(fd) = activity_fd {
        let sink =
            JsonlFdSink::from_raw_fd(fd).map_err(|e| anyhow::anyhow!("--activity-fd={fd}: {e}"))?;
        bus.add_sink(Arc::new(sink));
    }
    if let Some(path) = activity_jsonl {
        let sink = JsonlFdSink::from_path(path)
            .map_err(|e| anyhow::anyhow!("--activity-jsonl={path}: {e}"))?;
        bus.add_sink(Arc::new(sink));
    }
    Ok(())
}

/// Opt-in Portage-compatible emerge.log dual-write under `merge_root`.
pub fn attach_emergelog(bus: &ActivityBus, merge_root: &Utf8Path) {
    bus.add_sink(Arc::new(EmergeLogSink::for_merge_root(merge_root)));
}

/// Resolve the job id for this merge: explicit opt, else new id (or resume's).
pub fn resolve_job_id(opts: &ActivitySessionOpts, resume_job_id: Option<&str>) -> String {
    if let Some(id) = opts.job_id.as_deref().filter(|s| !s.is_empty()) {
        return id.to_string();
    }
    if let Some(id) = resume_job_id.filter(|s| !s.is_empty()) {
        return id.to_string();
    }
    // Align with resume's id minting style when no resume file is involved.
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{t:x}-{}", std::process::id())
}

/// Format active sessions for `em log current`.
pub fn format_current(proj: &LiveProjection, now: f64) -> String {
    let active: Vec<_> = proj
        .active()
        .into_iter()
        .filter(|s| {
            // Drop clearly dead processes with no heartbeat freshness check yet
            // beyond pid — still show if pid alive OR has inflight.
            pid_alive(s.pid) || !s.inflight.is_empty()
        })
        .collect();

    if active.is_empty() {
        return "No ongoing activities.\n".into();
    }

    let mut out = format!(
        "{} ongoing activit{}\n",
        active.len(),
        if active.len() == 1 { "y" } else { "ies" }
    );
    for (i, s) in active.iter().enumerate() {
        let age = format_duration(now - s.started_at);
        let argv = if s.argv.is_empty() {
            "em".to_string()
        } else {
            s.argv.join(" ")
        };
        out.push_str(&format!(
            "\n[{n}] {argv}  (pid {pid}, started {age} ago)  root={root}\n",
            n = i + 1,
            pid = s.pid,
            root = s.merge_root,
        ));
        if let Some(parent) = &s.parent_job_id {
            out.push_str(&format!("    parent={parent}\n"));
        }
        let jobs = s
            .flags
            .jobs
            .map(|j| format!(", {j} jobs"))
            .unwrap_or_default();
        out.push_str(&format!(
            "    plan {done}/{total} done, {failed} failed{jobs}\n",
            done = s.completed,
            total = s.plan_total,
            failed = s.failed,
        ));
        let inflight = s.inflight_sorted();
        if inflight.is_empty() {
            out.push_str("    inflight: (none)\n");
        } else {
            out.push_str(&format!("    inflight ({}):\n", inflight.len()));
            for p in inflight {
                let pkg_age = format_duration(now - p.pkg_started_at);
                let phase_age = format_duration(now - p.phase_started_at);
                out.push_str(&format!(
                    "      {cpv:<40} {phase:<12} phase {phase_age} (pkg {pkg_age})  [{idx}/{of}]\n",
                    cpv = p.cpv,
                    phase = p.phase,
                    idx = p.index,
                    of = p.of,
                ));
            }
        }
    }
    out
}

fn format_duration(secs: f64) -> String {
    if secs < 0.0 || !secs.is_finite() {
        return "?".into();
    }
    let s = secs as u64;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn bus_delivers_to_sink_and_subscriber() {
        let bus = ActivityBus::new();
        let rec = Arc::new(RecordingSink::new());
        bus.add_sink(rec.clone());
        let mut rx = bus.subscribe();

        let ev = ActivityEvent::SessionStart {
            v: ACTIVITY_EVENT_VERSION,
            job_id: "j1".into(),
            parent_job_id: None,
            pid: 1,
            started_at: 1.0,
            argv: vec!["em".into(), "foo".into()],
            merge_root: "/".into(),
            host_root: "/".into(),
            mode: ActivityMode::Merge,
            plan_total: 2,
            flags: SessionFlags::default(),
            plan: vec![],
            blockers: vec![],
        };
        bus.emit(ev.clone());

        assert_eq!(rec.events().len(), 1);
        let got = rx.try_recv().unwrap();
        assert_eq!(got.job_id(), "j1");
    }

    #[test]
    fn activity_pkg_ctx_emits_phase_enter_leave() {
        let bus = ActivityBus::new();
        let rec = Arc::new(RecordingSink::new());
        bus.add_sink(rec.clone());
        let pkg = ActivityPkgCtx::new(
            bus,
            "j".into(),
            None,
            "sys-apps/foo-1".into(),
            ActivityMergeRoot::Target,
        );
        let t0 = pkg.phase_enter("compile");
        pkg.phase_leave("compile", t0 - 1.0); // force positive seconds
        let evs = rec.events();
        assert!(
            evs.iter().any(
                |e| matches!(e, ActivityEvent::PhaseEnter { phase, .. } if phase == "compile")
            )
        );
        assert!(
            evs.iter().any(
                |e| matches!(e, ActivityEvent::PhaseLeave { phase, .. } if phase == "compile")
            )
        );
        assert_eq!(pkg.phases_done().len(), 1);
        assert_eq!(pkg.phases_done()[0].phase, "compile");
    }

    #[test]
    fn projection_tracks_inflight_and_completion() {
        let mut p = LiveProjection::new();
        let job = "job-a";
        p.apply(&ActivityEvent::SessionStart {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job.into(),
            parent_job_id: Some("stage1".into()),
            pid: 42,
            started_at: 100.0,
            argv: vec!["em".into()],
            merge_root: "/".into(),
            host_root: "/".into(),
            mode: ActivityMode::Merge,
            plan_total: 3,
            flags: SessionFlags {
                jobs: Some(4),
                ..Default::default()
            },
            plan: vec![],
            blockers: vec![],
        });
        p.apply(&ActivityEvent::PkgStart {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job.into(),
            parent_job_id: None,
            cpv: "sys-apps/foo-1".into(),
            cpn: "sys-apps/foo".into(),
            merge_root: ActivityMergeRoot::Target,
            index: 1,
            of: 3,
            kind: PkgKind::Source,
            at: 101.0,
        });
        p.apply(&ActivityEvent::PhaseEnter {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job.into(),
            parent_job_id: None,
            cpv: "sys-apps/foo-1".into(),
            merge_root: ActivityMergeRoot::Target,
            phase: "compile".into(),
            at: 102.0,
        });
        assert_eq!(p.active().len(), 1);
        assert_eq!(p.active()[0].inflight.len(), 1);
        assert_eq!(p.active()[0].inflight_sorted()[0].phase, "compile");

        p.apply(&ActivityEvent::PkgEnd {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job.into(),
            parent_job_id: None,
            cpv: "sys-apps/foo-1".into(),
            cpn: "sys-apps/foo".into(),
            merge_root: ActivityMergeRoot::Target,
            kind: PkgKind::Source,
            ok: true,
            at: 200.0,
            seconds: 99.0,
            phases: vec![],
            error: None,
        });
        assert!(p.active()[0].inflight.is_empty());
        assert_eq!(p.active()[0].completed, 1);
    }

    #[cfg(unix)]
    #[test]
    fn worker_reemit_socket_round_trip() {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("reemit.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (tx, rx) = mpsc::channel();
        let accept = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut lines = BufReader::new(stream).lines();
            let line = lines.next().unwrap().unwrap();
            tx.send(line).unwrap();
        });

        let live = camino::Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let bus = worker_activity_bus(&live, Some(sock.to_str().unwrap()));
        let pkg = ActivityPkgCtx::new(
            bus,
            "j".into(),
            None,
            "sys-apps/foo-1".into(),
            ActivityMergeRoot::Target,
        );
        let t0 = pkg.phase_enter("install");
        pkg.phase_leave("install", t0);

        let line = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("re-emit line");
        let ev = ActivityEvent::from_jsonl_line(&line).unwrap();
        assert!(matches!(
            ev,
            ActivityEvent::PhaseEnter {
                phase,
                ..
            } if phase == "install"
        ));
        accept.join().unwrap();
    }

    #[test]
    fn remaining_for_eta_remaps_blockers() {
        let mut p = LiveProjection::new();
        p.apply(&ActivityEvent::SessionStart {
            v: ACTIVITY_EVENT_VERSION,
            job_id: "j".into(),
            parent_job_id: None,
            pid: 1,
            started_at: 1.0,
            argv: vec![],
            merge_root: "/".into(),
            host_root: "/".into(),
            mode: ActivityMode::Merge,
            plan_total: 3,
            flags: SessionFlags {
                jobs: Some(2),
                ..Default::default()
            },
            plan: vec![
                ActivityPlanPkg {
                    cpn: "a/a".into(),
                    cpv: "a/a-1".into(),
                    merge_root: ActivityMergeRoot::Target,
                },
                ActivityPlanPkg {
                    cpn: "b/b".into(),
                    cpv: "b/b-1".into(),
                    merge_root: ActivityMergeRoot::Target,
                },
                ActivityPlanPkg {
                    cpn: "c/c".into(),
                    cpv: "c/c-1".into(),
                    merge_root: ActivityMergeRoot::Target,
                },
            ],
            // 1 depends on 0; 2 depends on 1
            blockers: vec![vec![], vec![0], vec![1]],
        });
        p.apply(&ActivityEvent::PkgEnd {
            v: ACTIVITY_EVENT_VERSION,
            job_id: "j".into(),
            parent_job_id: None,
            cpv: "a/a-1".into(),
            cpn: "a/a".into(),
            merge_root: ActivityMergeRoot::Target,
            kind: PkgKind::Source,
            ok: true,
            at: 2.0,
            seconds: 1.0,
            phases: vec![],
            error: None,
        });
        let s = p.get("j").unwrap();
        let (pkgs, blockers) = s.remaining_for_eta();
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].cpv, "b/b-1");
        assert_eq!(pkgs[1].cpv, "c/c-1");
        // old 1→0 had blocker 0 (finished) → empty; old 2→1 had blocker 1 → new 0
        assert_eq!(blockers, vec![vec![], vec![0]]);
    }

    #[test]
    fn live_fs_round_trip() {
        use camino::Utf8PathBuf;
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let bus = ActivityBus::new();
        bus.add_sink(Arc::new(LiveFsSink::new(root.clone())));

        let job = "job-fs";
        bus.emit(ActivityEvent::SessionStart {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job.into(),
            parent_job_id: None,
            pid: std::process::id(),
            started_at: 1.0,
            argv: vec!["em".into(), "-u".into(), "foo".into()],
            merge_root: root.as_str().into(),
            host_root: "/".into(),
            mode: ActivityMode::Merge,
            plan_total: 1,
            flags: SessionFlags::default(),
            plan: vec![ActivityPlanPkg {
                cpn: "app-misc/bar".into(),
                cpv: "app-misc/bar-2".into(),
                merge_root: ActivityMergeRoot::Target,
            }],
            blockers: vec![vec![]],
        });
        bus.emit(ActivityEvent::PkgStart {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job.into(),
            parent_job_id: None,
            cpv: "app-misc/bar-2".into(),
            cpn: "app-misc/bar".into(),
            merge_root: ActivityMergeRoot::Target,
            index: 1,
            of: 1,
            kind: PkgKind::Source,
            at: 2.0,
        });
        bus.emit(ActivityEvent::PhaseEnter {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job.into(),
            parent_job_id: None,
            cpv: "app-misc/bar-2".into(),
            merge_root: ActivityMergeRoot::Target,
            phase: "compile".into(),
            at: 3.0,
        });

        let loaded = load_live_from_disk(&root);
        assert_eq!(loaded.active().len(), 1);
        let s = loaded.active()[0];
        assert_eq!(s.plan_total, 1);
        assert_eq!(s.plan.len(), 1);
        assert_eq!(s.blockers.len(), 1);
        assert_eq!(s.inflight.len(), 1);
        assert_eq!(s.inflight_sorted()[0].phase, "compile");

        bus.emit(ActivityEvent::SessionEnd {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job.into(),
            parent_job_id: None,
            at: 10.0,
            ok: true,
            completed: 1,
            failed: 0,
            seconds: 9.0,
        });
        let loaded2 = load_live_from_disk(&root);
        assert!(loaded2.active().is_empty());
    }

    #[test]
    fn jsonl_round_trip() {
        let ev = ActivityEvent::PkgEnd {
            v: ACTIVITY_EVENT_VERSION,
            job_id: "j".into(),
            parent_job_id: None,
            cpv: "a/b-1".into(),
            cpn: "a/b".into(),
            merge_root: ActivityMergeRoot::Host,
            kind: PkgKind::Binpkg,
            ok: false,
            at: 1.5,
            seconds: 0.5,
            phases: vec![PhaseTiming {
                phase: "qmerge".into(),
                seconds: 0.5,
            }],
            error: Some("boom".into()),
        };
        let line = ev.to_jsonl_line().unwrap();
        assert!(line.contains("\"event\":\"pkg_end\""));
        let back = ActivityEvent::from_jsonl_line(&line).unwrap();
        assert_eq!(back, ev);
    }
}
