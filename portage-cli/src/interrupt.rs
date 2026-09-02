//! Graceful interrupt: stop starting work, let what is running finish
//!
//! `em` installs no other signal handlers — Ctrl+C on a merge otherwise kills
//! the process outright, mid-phase or mid-qmerge. Note this only makes `em`
//! itself graceful: build children share its process group, so a *terminal*
//! Ctrl+C still SIGINTs `gcc`/`make` directly. The "let what is running
//! finish" property therefore holds for `kill -INT`/`SIGTERM`, not for the
//! keystroke.
//!
//! The first `SIGINT`/`SIGTERM` sets a flag every package loop checks before
//! starting the *next* one — the two merge loops and the unmerge batch:
//! nothing new starts, and whatever is already running finishes. That is deliberately not a cancellation — a
//! package interrupted between its collision check and its VDB entry is the
//! failure mode the merge critical section exists to avoid, and declining to
//! start one is free where unwinding one is not.
//!
//! A second signal is the escape hatch for someone who does not want to wait:
//! it exits immediately with `128 + signum`, the encoding a shell reports for
//! a killed child (`130` for `SIGINT`), so `$?` reads the way it would have
//! without a handler. The process is not *actually* killed by the signal, so
//! a caller inspecting `WIFSIGNALED` rather than the status byte can tell the
//! difference; nothing in this tree does, and matching it exactly would mean
//! restoring `SIG_DFL` and re-raising through `libc`.

use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Whether a graceful stop has been requested
///
/// Consulted before starting the next package by both merge loops and by the
/// unmerge batch — every loop `will_build` arms the handler for must check it,
/// or the notice claims the run is winding down while work continues.
pub(crate) fn requested() -> bool {
    REQUESTED.load(Ordering::Relaxed)
}

/// Watch `SIGINT`/`SIGTERM` for the life of the process
///
/// Call once, inside the runtime, and only for an invocation that will
/// actually merge — a query or a regen has no safe point to stop at, and
/// there a single Ctrl+C should keep ending the process outright.
///
/// Failing to register is not fatal: the signals keep their default
/// disposition, which is the behaviour that predates this module.
pub(crate) fn install() {
    for kind in [
        tokio::signal::unix::SignalKind::interrupt(),
        tokio::signal::unix::SignalKind::terminate(),
    ] {
        let Ok(mut stream) = tokio::signal::unix::signal(kind) else {
            tracing::debug!("could not watch signal {kind:?}; leaving it at its default");
            continue;
        };
        tokio::spawn(async move {
            while stream.recv().await.is_some() {
                if REQUESTED.swap(true, Ordering::Relaxed) {
                    die_now(kind);
                }
                crate::style::warn_line!(
                    "interrupt: finishing the packages already building, starting no new ones \
                     (interrupt again to stop now)"
                );
            }
        });
    }
}

/// Exit immediately with the status a shell reports for a signalled child
fn die_now(kind: tokio::signal::unix::SignalKind) -> ! {
    std::process::exit(128 + kind.as_raw_value());
}
