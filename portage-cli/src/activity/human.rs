//! Human stdout sink — renders the emerge-style progress banners as a
//! *projection* of [`ActivityEvent`](super::event::ActivityEvent)s.
//!
//! This is sink `[5]` of the activity architecture: the terminal gets the same
//! typed stream `--activity-fd` / `em log current` consume, not parallel ad-hoc
//! `println!`s scattered through the merge loop. Verbosity is decided in one
//! place (here), not at every call site.
//!
//! Rendering rules (emerge-style):
//! - `quiet`: render nothing (failures still surface via the merge loop's
//!   end-of-run summary, which is aggregation, not streaming).
//! - default: `>>> Emerging (N of M) cpv` on [`PkgStart`](super::event::ActivityEvent::PkgStart),
//!   `=== (N of M) <Phase> cpv` on the headline [`PhaseEnter`](super::event::ActivityEvent::PhaseEnter)s.
//! - `verbose >= 1`: also print per-phase elapsed time on
//!   [`PhaseLeave`](super::event::ActivityEvent::PhaseLeave).
//!
//! `index`/`of` live on `PkgStart`; `PhaseEnter` only carries `cpv`, so the
//! sink remembers the last `PkgStart` per `(job_id, cpv)` to label phases.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;

use super::bus::ActivitySink;
use super::event::{ActivityEvent, PkgKind};

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

#[derive(Clone, Default)]
struct PkgLoc {
    index: u32,
    of: u32,
}

/// Terminal renderer for the activity bus. Attached as a **direct** (inline)
/// sink so banners appear immediately, not buffered behind the disk sinks'
/// background threads.
pub struct HumanStdoutSink {
    quiet: bool,
    verbose: u8,
    state: Mutex<HashMap<(String, String), PkgLoc>>,
}

impl HumanStdoutSink {
    /// `quiet` suppresses all rendering; `verbose` adds per-phase timings.
    pub fn new(quiet: bool, verbose: u8) -> Self {
        Self {
            quiet,
            verbose,
            state: Mutex::new(HashMap::new()),
        }
    }
}

fn action_for(kind: PkgKind) -> &'static str {
    match kind {
        PkgKind::FetchOnly => "Fetching",
        PkgKind::Binpkg => "Merging", // binary package: no compile, straight to qmerge
        PkgKind::Source => "Emerging",
    }
}

impl ActivitySink for HumanStdoutSink {
    fn on_event(&self, event: &ActivityEvent) {
        if self.quiet {
            return;
        }
        match event {
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
                    },
                );
                println!(">>> {} ({index} of {of}) {cpv}", action_for(*kind));
                let _ = std::io::stdout().flush();
            }
            ActivityEvent::PhaseEnter {
                job_id, cpv, phase, ..
            } => {
                let Some(label) = phase_label(phase) else {
                    return;
                };
                let loc = self
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&(job_id.clone(), cpv.clone()))
                    .cloned();
                match loc {
                    Some(PkgLoc { index, of }) => {
                        println!("=== ({index} of {of}) {label} ({cpv})");
                    }
                    None => {
                        println!("=== {label} ({cpv})");
                    }
                }
                let _ = std::io::stdout().flush();
            }
            ActivityEvent::PhaseLeave {
                cpv,
                phase,
                seconds,
                ..
            } if self.verbose >= 1 && phase_label(phase).is_some() => {
                println!("  >> {cpv} {phase}: {seconds:.1}s");
                let _ = std::io::stdout().flush();
            }
            ActivityEvent::PkgEnd {
                job_id,
                cpv,
                ok: false,
                error,
                ..
            } => {
                // Inline failure marker; the merge loop prints the full
                // aggregated summary at the end. `error` is `None` when the
                // driver didn't supply a message.
                let why = error.as_deref().unwrap_or("(no message)");
                eprintln!(">>> failed: {cpv}: {why}");
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&(job_id.clone(), cpv.clone()));
            }
            ActivityEvent::PkgEnd {
                job_id,
                cpv,
                ok: true,
                ..
            } => {
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&(job_id.clone(), cpv.clone()));
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
    fn quiet_renders_nothing() {
        // Hard to assert stdout; instead assert the state stays empty (no
        // PkgStart recorded) under quiet, proving the early return fired.
        let sink = HumanStdoutSink::new(true, 0);
        sink.on_event(&pkg_start("j", "a/b-1", 1, 2));
        assert!(sink.state.lock().unwrap().is_empty());
    }

    #[test]
    fn phase_enter_resolves_index_from_pkg_start() {
        let sink = HumanStdoutSink::new(false, 0);
        sink.on_event(&pkg_start("j", "sys-devel/gcc-14", 3, 9));
        sink.on_event(&phase_enter("j", "sys-devel/gcc-14", "compile"));
        let loc = sink
            .state
            .lock()
            .unwrap()
            .get(&("j".into(), "sys-devel/gcc-14".into()))
            .cloned()
            .unwrap_or_default();
        assert_eq!(loc.index, 3);
        assert_eq!(loc.of, 9);
    }

    #[test]
    fn phase_label_suppresses_internals() {
        assert_eq!(phase_label("compile"), Some("Compiling"));
        assert_eq!(phase_label("qmerge"), Some("Merging"));
        assert_eq!(phase_label("pkg_preinst"), None);
        assert_eq!(phase_label("someweird"), None);
    }
}
