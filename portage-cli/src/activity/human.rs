//! Human stdout sink — renders the emerge-style progress banners as a
//! *projection* of [`ActivityEvent`](super::event::ActivityEvent)s.
//!
//! This is sink `[5]` of the activity architecture: the terminal gets the same
//! typed stream `--activity-fd` / `em log current` consume, not parallel ad-hoc
//! `println!`s scattered through the merge loop. Verbosity is decided in one
//! place (here), not at every call site.
//!
//! Rendering rules, matched against a real interactive `--jobs N` emerge run
//! (real `_emerge/{MergeListItem,PackageMerge,Binpkg,JobStatusDisplay}.py`):
//! - `quiet`: suppress progress banners. Failures still print — real
//!   `--quiet` never silences errors, only the per-package info noise.
//! - default: `>>> {action} (N of M) cpv[ for/to ROOT/]` on
//!   [`PkgStart`](super::event::ActivityEvent::PkgStart) (build start) and on
//!   the `qmerge`/`merge` phase's enter/leave (`Installing`/`Completed` — the
//!   actual VDB-write step, wrapped by real emerge's `PackageMerge`). Real
//!   emerge does **not** print a banner for the phases in between
//!   (setup/unpack/.../compile/install) — that's `EbuildBuild`'s "===
//!   Compiling/Merging" message, which goes to `emerge.log` only
//!   (`logger.log`, a different sink than the interactive `statusMessage`
//!   banners), never to the terminal.
//! - `verbose >= 1`: additionally show every phase's enter banner plus
//!   elapsed time on [`PhaseLeave`](super::event::ActivityEvent::PhaseLeave)
//!   — an `em`-specific extra beyond what real emerge shows interactively,
//!   for anyone who wants the detail.
//! - `--jobs N` (N > 1): redraw a persistent `Jobs: N of M complete, R
//!   running` status line (real emerge's `JobStatusDisplay`), erased before
//!   and redrawn after each banner so parallel output doesn't just interleave
//!   silently.
//!
//! `index`/`of` live on `PkgStart`; `PhaseEnter`/`PhaseLeave` only carry
//! `cpv`, so the sink remembers the last `PkgStart` per `(job_id, cpv)` to
//! label them. `SessionStart` carries the actual root paths (`merge_root`/
//! `host_root` strings) so the `for`/`to ROOT/` suffix can be reproduced —
//! real emerge only appends it when `ROOT != "/"`.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::sync::Mutex;

use super::bus::ActivitySink;
use super::event::{ActivityEvent, ActivityMergeRoot, ActivityMode, PkgKind};
use crate::style::{C_COUNT, C_PKG, C_PKG_BINARY};

/// Emerge-style label for a phase, or `None` to suppress (internal `pkg_*`
/// helpers the user does not need to see scroll by). Only consulted under
/// `verbose >= 1` — real emerge shows none of these interactively.
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

/// The phase real emerge's `PackageMerge` wraps with "Installing"/"Completed"
/// — the actual VDB-write step, distinct from the build phases before it.
fn is_merge_phase(phase: &str) -> bool {
    phase == "qmerge" || phase == "merge"
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
    /// Actual root paths from `SessionStart`, for the `for`/`to ROOT/` message
    /// suffix (real emerge only adds it when the root isn't `/`).
    host_root: String,
    merge_root: String,
    mode: ActivityMode,
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

    /// Erase the in-place status line so the next banner does not land on
    /// top of it — mirrors `JobStatusDisplay._erase`. Regen counters live on
    /// stderr; merge job status on stdout.
    fn erase_status(&self, job_id: &str) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(js) = jobs.get_mut(job_id)
            && js.displayed
        {
            if js.mode == ActivityMode::Regen {
                // Always clear the counter line (tty or redirected with `\r`).
                eprint!("\r\x1b[K");
                let _ = std::io::stderr().flush();
            } else if self.is_tty {
                print!("\r\x1b[K");
            }
            js.displayed = false;
        }
    }

    /// Redraw progress for the session.
    ///
    /// - **Regen:** always `[done/total]` (optionally with a failed count) —
    ///   the traditional `em regen` / egencache-style counter.
    /// - **Merge (and friends):** `Jobs: N of M complete…` only when
    ///   `--jobs > 1` (matches real emerge's sequential path, which never
    ///   shows that line).
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
        let line = if js.mode == ActivityMode::Regen {
            if js.failed > 0 {
                format!(
                    "[{}/{}] ({} failed)",
                    js.completed, js.plan_total, js.failed
                )
            } else {
                format!("[{}/{}]", js.completed, js.plan_total)
            }
        } else {
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
            line
        };
        if self.is_tty {
            print!("\r\x1b[K{line}");
            js.displayed = true;
        } else {
            // Non-TTY: regen keeps a single-line counter; merge jobs already
            // printed per-banner lines.
            if js.mode == ActivityMode::Regen {
                eprint!("\r{line}");
                js.displayed = true;
            } else {
                println!("{line}");
            }
        }
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
    }

    /// `" {preposition} {root}/"`, or empty when the root is `/` — real
    /// emerge's `if pkg.root_config.settings["ROOT"] != "/": msg += " for/to
    /// {root}"`.
    fn root_suffix(&self, job_id: &str, root: ActivityMergeRoot, preposition: &str) -> String {
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let Some(js) = jobs.get(job_id) else {
            return String::new();
        };
        let path = match root {
            ActivityMergeRoot::Host => &js.host_root,
            ActivityMergeRoot::Target => &js.merge_root,
        };
        if path.is_empty() || path == "/" {
            return String::new();
        }
        let path = path.strip_suffix('/').unwrap_or(path);
        format!(" {preposition} {path}/")
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

fn pkg_style(kind: PkgKind) -> anstyle::Style {
    match kind {
        PkgKind::Binpkg => C_PKG_BINARY,
        _ => C_PKG,
    }
}

impl ActivitySink for HumanStdoutSink {
    fn on_event(&self, event: &ActivityEvent) {
        match event {
            ActivityEvent::SessionStart {
                job_id,
                plan_total,
                flags,
                merge_root,
                host_root,
                mode,
                argv,
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
                        host_root: host_root.clone(),
                        merge_root: merge_root.clone(),
                        mode: *mode,
                    },
                );
                if self.quiet {
                    return;
                }
                if *mode == ActivityMode::Regen {
                    // Driver puts `regen ::repo (N ebuilds)` in argv[1] when present.
                    let banner = argv
                        .get(1)
                        .cloned()
                        .unwrap_or_else(|| format!("regen ({plan_total} ebuilds)"));
                    eprintln!("{banner}");
                }
                self.draw_status(job_id);
            }
            ActivityEvent::SessionEnd { job_id, .. } => {
                let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(js) = jobs.remove(job_id)
                    && js.displayed
                {
                    // Regen progress was on stderr; merge jobs status on stdout.
                    if js.mode == ActivityMode::Regen {
                        eprintln!();
                    } else {
                        println!();
                    }
                }
            }
            ActivityEvent::PkgStart {
                job_id,
                cpv,
                merge_root,
                index,
                of,
                kind,
                ..
            } => {
                let mode = self
                    .jobs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(job_id)
                    .map(|j| j.mode)
                    .unwrap_or_default();
                // Regen does not emit per-ebuild start banners (thousands of
                // packages); only track location if we ever need it.
                if mode == ActivityMode::Regen {
                    return;
                }
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
                let style = pkg_style(*kind);
                let suffix = self.root_suffix(job_id, *merge_root, "for");
                let mut out = anstream::stdout();
                let _ = writeln!(
                    out,
                    ">>> {} ({C_COUNT}{index}{C_COUNT:#} of {C_COUNT}{of}{C_COUNT:#}) {style}{cpv}{style:#}{suffix}",
                    action_for(*kind)
                );
                let _ = out.flush();
                self.draw_status(job_id);
            }
            ActivityEvent::PhaseEnter {
                job_id,
                cpv,
                merge_root,
                phase,
                ..
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
                if is_merge_phase(phase) {
                    let style = pkg_style(loc.kind);
                    let suffix = self.root_suffix(job_id, *merge_root, "to");
                    self.erase_status(job_id);
                    let mut out = anstream::stdout();
                    let _ = writeln!(
                        out,
                        ">>> Installing ({C_COUNT}{}{C_COUNT:#} of {C_COUNT}{}{C_COUNT:#}) {style}{cpv}{style:#}{suffix}",
                        loc.index, loc.of
                    );
                    let _ = out.flush();
                    self.draw_status(job_id);
                    return;
                }
                if self.verbose == 0 {
                    return;
                }
                let Some(label) = phase_label(phase) else {
                    return;
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
                cpn: _,
                merge_root,
                kind,
                ok,
                error,
                ..
            } => {
                let mode = self
                    .jobs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(job_id)
                    .map(|j| j.mode)
                    .unwrap_or_default();
                let had = self
                    .state
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
                if mode == ActivityMode::Regen {
                    // Progress only for successes; failures always print a
                    // short line (rich miette frames, if any, are printed by
                    // the regen driver after this event — same bus item).
                    if !*ok {
                        self.erase_status(job_id);
                        let why = error.as_deref().unwrap_or("(no message)");
                        eprintln!(">>> failed: {cpv}: {why}");
                    }
                    if !self.quiet {
                        self.draw_status(job_id);
                    }
                    return;
                }
                if *ok {
                    if !self.quiet
                        && let Some(loc) = had
                    {
                        self.erase_status(job_id);
                        let style = pkg_style(*kind);
                        let suffix = self.root_suffix(job_id, *merge_root, "to");
                        let mut out = anstream::stdout();
                        let _ = writeln!(
                            out,
                            ">>> Completed ({C_COUNT}{}{C_COUNT:#} of {C_COUNT}{}{C_COUNT:#}) {style}{cpv}{style:#}{suffix}",
                            loc.index, loc.of
                        );
                        let _ = out.flush();
                    }
                } else {
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

    fn session_start(
        job: &str,
        plan_total: u32,
        jobs: Option<u32>,
        merge_root: &str,
    ) -> ActivityEvent {
        ActivityEvent::SessionStart {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job.into(),
            parent_job_id: None,
            pid: 1,
            started_at: 0.0,
            argv: vec![],
            merge_root: merge_root.into(),
            host_root: "/".into(),
            mode: crate::activity::ActivityMode::Merge,
            plan_total,
            flags: crate::activity::SessionFlags {
                jobs,
                ..Default::default()
            },
            plan: vec![],
            blockers: vec![],
        }
    }

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

    fn pkg_end(job: &str, cpv: &str, ok: bool) -> ActivityEvent {
        ActivityEvent::PkgEnd {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job.into(),
            parent_job_id: None,
            cpv: cpv.into(),
            cpn: cpv
                .split_once('/')
                .map(|(c, _)| c.to_string())
                .unwrap_or_default(),
            merge_root: ActivityMergeRoot::Target,
            kind: PkgKind::Source,
            ok,
            at: 3.0,
            seconds: 1.0,
            phases: vec![],
            error: if ok { None } else { Some("boom".into()) },
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
    fn only_qmerge_or_merge_is_the_merge_phase() {
        assert!(is_merge_phase("qmerge"));
        assert!(is_merge_phase("merge"));
        assert!(!is_merge_phase("compile"));
        assert!(!is_merge_phase("install"));
    }

    #[test]
    fn action_for_matches_real_emerge_wording() {
        assert_eq!(action_for(PkgKind::Source), "Emerging");
        assert_eq!(action_for(PkgKind::Binpkg), "Emerging binary");
        assert_eq!(action_for(PkgKind::FetchOnly), "Fetching");
    }

    #[test]
    fn root_suffix_is_empty_for_the_live_root() {
        let sink = HumanStdoutSink::new(false, 0);
        sink.on_event(&session_start("j", 1, None, "/"));
        assert_eq!(sink.root_suffix("j", ActivityMergeRoot::Target, "for"), "");
    }

    #[test]
    fn root_suffix_names_a_non_live_root() {
        let sink = HumanStdoutSink::new(false, 0);
        sink.on_event(&session_start("j", 1, None, "/tmp/portage-act"));
        assert_eq!(
            sink.root_suffix("j", ActivityMergeRoot::Target, "for"),
            " for /tmp/portage-act/"
        );
        assert_eq!(
            sink.root_suffix("j", ActivityMergeRoot::Target, "to"),
            " to /tmp/portage-act/"
        );
    }

    #[test]
    fn jobs_status_tracks_completion_and_failure_counts() {
        let sink = HumanStdoutSink::new(false, 0);
        sink.on_event(&session_start("j", 2, Some(4), "/"));
        sink.on_event(&pkg_start("j", "a/a-1", 1, 2));
        sink.on_event(&pkg_end("j", "a/a-1", false));
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
        sink.on_event(&session_start("j", 1, None, "/"));
        let jobs = sink.jobs.lock().unwrap_or_else(|e| e.into_inner());
        assert!(matches!(jobs.get("j"), Some(js) if js.jobs == 1));
    }
}
