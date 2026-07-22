//! Human stdout sink — renders the emerge-style progress banners as a
//! *projection* of [`ActivityEvent`](super::event::ActivityEvent)s.
//!
//! This is sink `[5]` of the activity architecture: the terminal gets the same
//! typed stream `--activity-fd` / `em log current` consume, not parallel ad-hoc
//! `println!`s scattered through the merge loop. Verbosity is decided in one
//! place (here), not at every call site.
//!
//! Rendering rules, matched against real emerge's own `_emerge/MergeListItem.py`
//! / `EbuildBuild.py` / `Binpkg.py` / `JobStatusDisplay.py`:
//! - `quiet`: suppress progress banners. Failures still print — real
//!   `--quiet` never silences errors, only the per-package info noise.
//! - default: `>>> {action} (N of M) cpv` on
//!   [`PkgStart`](super::event::ActivityEvent::PkgStart), plus exactly **one**
//!   headline `=== (N of M) <Phase> (cpv)` banner per package (compile for a
//!   source build, qmerge for a binary merge) — real emerge does not print a
//!   banner for every phase (setup/unpack/prepare/configure/...), only the
//!   one that matters.
//! - `verbose >= 1`: also show every other phase's enter banner, plus elapsed
//!   time on [`PhaseLeave`](super::event::ActivityEvent::PhaseLeave).
//! - `--jobs N` (N > 1): redraw a persistent `Jobs: N of M complete, R
//!   running` status line (real emerge's `JobStatusDisplay`), erased before
//!   and redrawn after each banner so parallel output doesn't just interleave
//!   silently.
//!
//! `index`/`of` live on `PkgStart`; `PhaseEnter` only carries `cpv`, so the
//! sink remembers the last `PkgStart` per `(job_id, cpv)` to label phases.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::sync::Mutex;

use super::bus::ActivitySink;
use super::event::{ActivityEvent, PkgKind};
use crate::style::{C_COUNT, C_PKG, C_PKG_BINARY};

/// Emerge-style label for a phase, or `None` to suppress (internal `pkg_*`
/// helpers the user does not need to see scroll by).
fn phase_label(phase: &str) -> Option<&'static str> {
    Some(match phase {
        "setup" => "Setting up",
        "unpack" => "Unpacking",
        "prepare" => "Preparing",
        "configure" => "Configuring",
        "compile" => "Compiling",
        "test" => "Testing",
        "install" => "Installing",
        "qmerge" | "merge" => "Merging",
        "package" => "Packaging",
        "fetch" => "Fetching",
        // pkg_preinst/postinst/prerm/postrm and other internals: stay quiet.
        _ if phase.starts_with("pkg_") => return None,
        _ => return None,
    })
}

/// The one phase per [`PkgKind`] real emerge banners as `=== (N of M) ...`;
/// every other phase stays silent unless `verbose >= 1`.
fn is_headline_phase(kind: PkgKind, phase: &str) -> bool {
    match kind {
        PkgKind::Source => phase == "compile",
        PkgKind::Binpkg => phase == "qmerge" || phase == "merge",
        PkgKind::FetchOnly => false,
    }
}

/// Combined label for the headline banner (`EbuildBuild`'s "Compiling/Merging",
/// `Binpkg`'s "Merging Binary" — real emerge's exact wording).
fn headline_label(kind: PkgKind) -> &'static str {
    match kind {
        PkgKind::Source => "Compiling/Merging",
        PkgKind::Binpkg => "Merging Binary",
        PkgKind::FetchOnly => "",
    }
}

#[derive(Clone, Copy, Default)]
struct PkgLoc {
    index: u32,
    of: u32,
    kind: PkgKind,
}

/// Per-session `--jobs` bookkeeping for the `Jobs: N of M complete` line.
#[derive(Default)]
struct JobStatus {
    plan_total: u32,
    completed: u32,
    failed: u32,
    /// Requested parallelism; the status line only renders above 1 (real
    /// emerge's sequential path never shows it either).
    jobs: u32,
    /// A status line is currently on screen (tty only) and needs erasing
    /// before the next banner lands.
    displayed: bool,
}

/// Terminal renderer for the activity bus. Attached as a **direct** (inline)
/// sink so banners appear immediately, not buffered behind the disk sinks'
/// background threads.
pub struct HumanStdoutSink {
    quiet: bool,
    verbose: u8,
    is_tty: bool,
    state: Mutex<HashMap<(String, String), PkgLoc>>,
    jobs: Mutex<HashMap<String, JobStatus>>,
}

impl HumanStdoutSink {
    /// `quiet` suppresses progress banners (not failures); `verbose` adds
    /// every phase's banner plus per-phase timings.
    pub fn new(quiet: bool, verbose: u8) -> Self {
        Self {
            quiet,
            verbose,
            is_tty: std::io::stdout().is_terminal(),
            state: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
        }
    }

    /// Erase the in-place `Jobs: ...` line (tty only) so the next banner does
    /// not land on top of it — mirrors `JobStatusDisplay._erase`.
    fn erase_status(&self, job_id: &str) {
        if !self.is_tty {
            return;
        }
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(js) = jobs.get_mut(job_id)
            && js.displayed
        {
            print!("\r\x1b[K");
            js.displayed = false;
        }
    }

    /// Redraw `Jobs: N of M complete[, R running][, F failed]` for sessions
    /// with `--jobs > 1`; a no-op otherwise (matches real emerge, which never
    /// shows this line for a sequential run).
    fn draw_status(&self, job_id: &str) {
        let running = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .filter(|(j, _)| j == job_id)
            .count();
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let Some(js) = jobs.get_mut(job_id) else {
            return;
        };
        if js.jobs <= 1 {
            return;
        }
        let mut line = format!("Jobs: {} of {} complete", js.completed, js.plan_total);
        if running > 0 {
            line.push_str(&format!(", {running} running"));
        }
        if js.failed > 0 {
            line.push_str(&format!(", {} failed", js.failed));
        }
        if self.is_tty {
            print!("\r\x1b[K{line}");
            js.displayed = true;
        } else {
            println!("{line}");
        }
        let _ = std::io::stdout().flush();
    }
}

fn action_for(kind: PkgKind) -> &'static str {
    match kind {
        PkgKind::FetchOnly => "Fetching",
        // Real emerge: `action_desc = "Emerging"; if binary: += " binary"`.
        PkgKind::Binpkg => "Emerging binary",
        PkgKind::Source => "Emerging",
    }
}

impl ActivitySink for HumanStdoutSink {
    fn on_event(&self, event: &ActivityEvent) {
        match event {
            ActivityEvent::SessionStart {
                job_id,
                plan_total,
                flags,
                ..
            } => {
                self.jobs.lock().unwrap_or_else(|e| e.into_inner()).insert(
                    job_id.clone(),
                    JobStatus {
                        plan_total: *plan_total,
                        completed: 0,
                        failed: 0,
                        jobs: flags.jobs.unwrap_or(1).max(1),
                        displayed: false,
                    },
                );
                if !self.quiet {
                    self.draw_status(job_id);
                }
            }
            ActivityEvent::SessionEnd { job_id, .. } => {
                let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(js) = jobs.remove(job_id)
                    && js.displayed
                {
                    println!();
                }
            }
            ActivityEvent::PkgStart {
                job_id,
                cpv,
                index,
                of,
                kind,
                ..
            } => {
                self.state.lock().unwrap_or_else(|e| e.into_inner()).insert(
                    (job_id.clone(), cpv.clone()),
                    PkgLoc {
                        index: *index,
                        of: *of,
                        kind: *kind,
                    },
                );
                if self.quiet {
                    return;
                }
                self.erase_status(job_id);
                let pkg_style = match kind {
                    PkgKind::Binpkg => C_PKG_BINARY,
                    _ => C_PKG,
                };
                let mut out = anstream::stdout();
                let _ = writeln!(
                    out,
                    ">>> {} ({C_COUNT}{index}{C_COUNT:#} of {C_COUNT}{of}{C_COUNT:#}) {pkg_style}{cpv}{pkg_style:#}",
                    action_for(*kind)
                );
                let _ = out.flush();
                self.draw_status(job_id);
            }
            ActivityEvent::PhaseEnter {
                job_id, cpv, phase, ..
            } => {
                if self.quiet {
                    return;
                }
                let loc = self
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&(job_id.clone(), cpv.clone()))
                    .copied();
                let Some(loc) = loc else { return };
                let headline = is_headline_phase(loc.kind, phase);
                if !headline && self.verbose == 0 {
                    return;
                }
                let Some(label) = phase_label(phase) else {
                    return;
                };
                let label = if headline {
                    headline_label(loc.kind)
                } else {
                    label
                };
                self.erase_status(job_id);
                println!("=== ({} of {}) {label} ({cpv})", loc.index, loc.of);
                let _ = std::io::stdout().flush();
                self.draw_status(job_id);
            }
            ActivityEvent::PhaseLeave {
                cpv,
                phase,
                seconds,
                ..
            } if self.verbose >= 1 && phase_label(phase).is_some() => {
                if self.quiet {
                    return;
                }
                println!("  >> {cpv} {phase}: {seconds:.1}s");
                let _ = std::io::stdout().flush();
            }
            ActivityEvent::PkgEnd {
                job_id,
                cpv,
                ok,
                error,
                ..
            } => {
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&(job_id.clone(), cpv.clone()));
                {
                    let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(js) = jobs.get_mut(job_id) {
                        js.completed += 1;
                        if !*ok {
                            js.failed += 1;
                        }
                    }
                }
                if !*ok {
                    // Failures always surface, even under `-q` — real emerge
                    // never silences errors on `--quiet`, only progress info.
                    self.erase_status(job_id);
                    let why = error.as_deref().unwrap_or("(no message)");
                    eprintln!(">>> failed: {cpv}: {why}");
                }
                if !self.quiet {
                    self.draw_status(job_id);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{ACTIVITY_EVENT_VERSION, ActivityMergeRoot};

    fn pkg_start(job: &str, cpv: &str, index: u32, of: u32) -> ActivityEvent {
        ActivityEvent::PkgStart {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job.into(),
            parent_job_id: None,
            cpv: cpv.into(),
            cpn: cpv
                .split_once('/')
                .map(|(c, _)| c.to_string())
                .unwrap_or_default(),
            merge_root: ActivityMergeRoot::Target,
            index,
            of,
            kind: PkgKind::Source,
            at: 1.0,
        }
    }

    fn phase_enter(job: &str, cpv: &str, phase: &str) -> ActivityEvent {
        ActivityEvent::PhaseEnter {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job.into(),
            parent_job_id: None,
            cpv: cpv.into(),
            merge_root: ActivityMergeRoot::Target,
            phase: phase.into(),
            at: 2.0,
        }
    }

    #[test]
    fn quiet_still_records_state() {
        // `quiet` suppresses printed banners, not the state tracking used for
        // (e.g.) the Jobs line's running count.
        let sink = HumanStdoutSink::new(true, 0);
        sink.on_event(&pkg_start("j", "a/b-1", 1, 2));
        assert!(
            !sink
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
        );
    }

    #[test]
    fn phase_enter_resolves_index_from_pkg_start() {
        let sink = HumanStdoutSink::new(false, 0);
        sink.on_event(&pkg_start("j", "sys-devel/gcc-14", 3, 9));
        sink.on_event(&phase_enter("j", "sys-devel/gcc-14", "compile"));
        let state = sink.state.lock().unwrap_or_else(|e| e.into_inner());
        assert!(matches!(
            state.get(&("j".into(), "sys-devel/gcc-14".into())),
            Some(loc) if loc.index == 3 && loc.of == 9
        ));
    }

    #[test]
    fn phase_label_suppresses_internals() {
        assert_eq!(phase_label("compile"), Some("Compiling"));
        assert_eq!(phase_label("qmerge"), Some("Merging"));
        assert_eq!(phase_label("pkg_preinst"), None);
        assert_eq!(phase_label("someweird"), None);
    }

    #[test]
    fn only_the_headline_phase_is_a_source_headline() {
        assert!(is_headline_phase(PkgKind::Source, "compile"));
        assert!(!is_headline_phase(PkgKind::Source, "unpack"));
        assert!(!is_headline_phase(PkgKind::Source, "install"));
        assert!(is_headline_phase(PkgKind::Binpkg, "qmerge"));
        assert!(!is_headline_phase(PkgKind::FetchOnly, "fetch"));
    }

    #[test]
    fn action_for_matches_real_emerge_wording() {
        assert_eq!(action_for(PkgKind::Source), "Emerging");
        assert_eq!(action_for(PkgKind::Binpkg), "Emerging binary");
        assert_eq!(action_for(PkgKind::FetchOnly), "Fetching");
    }

    #[test]
    fn jobs_status_tracks_completion_and_failure_counts() {
        let sink = HumanStdoutSink::new(false, 0);
        sink.on_event(&ActivityEvent::SessionStart {
            v: ACTIVITY_EVENT_VERSION,
            job_id: "j".into(),
            parent_job_id: None,
            pid: 1,
            started_at: 0.0,
            argv: vec![],
            merge_root: "/".into(),
            host_root: "/".into(),
            mode: crate::activity::ActivityMode::Merge,
            plan_total: 2,
            flags: crate::activity::SessionFlags {
                jobs: Some(4),
                ..Default::default()
            },
            plan: vec![],
            blockers: vec![],
        });
        sink.on_event(&pkg_start("j", "a/a-1", 1, 2));
        sink.on_event(&ActivityEvent::PkgEnd {
            v: ACTIVITY_EVENT_VERSION,
            job_id: "j".into(),
            parent_job_id: None,
            cpv: "a/a-1".into(),
            cpn: "a/a".into(),
            merge_root: ActivityMergeRoot::Target,
            kind: PkgKind::Source,
            ok: false,
            at: 1.0,
            seconds: 1.0,
            phases: vec![],
            error: Some("boom".into()),
        });
        let jobs = sink.jobs.lock().unwrap_or_else(|e| e.into_inner());
        assert!(matches!(
            jobs.get("j"),
            Some(js) if js.completed == 1 && js.failed == 1 && js.plan_total == 2 && js.jobs == 4
        ));
    }

    #[test]
    fn jobs_status_is_inert_for_a_sequential_session() {
        // jobs=1 (or unset): `draw_status` must stay a no-op — no line, no
        // panic — same as real emerge's sequential path.
        let sink = HumanStdoutSink::new(false, 0);
        sink.on_event(&ActivityEvent::SessionStart {
            v: ACTIVITY_EVENT_VERSION,
            job_id: "j".into(),
            parent_job_id: None,
            pid: 1,
            started_at: 0.0,
            argv: vec![],
            merge_root: "/".into(),
            host_root: "/".into(),
            mode: crate::activity::ActivityMode::Merge,
            plan_total: 1,
            flags: crate::activity::SessionFlags::default(),
            plan: vec![],
            blockers: vec![],
        });
        let jobs = sink.jobs.lock().unwrap_or_else(|e| e.into_inner());
        assert!(matches!(jobs.get("j"), Some(js) if js.jobs == 1));
    }
}
