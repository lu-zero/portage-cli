//! Tracing subscriber setup.
//!
//! `tracing`'s contract is "library emits, application decides": library
//! crates emit [`tracing`] events, and the CLI installs the subscriber that
//! decides where they go. This installs a compact stderr formatter whose level
//! floor encodes the verbosity model:
//!
//! | Invocation | Floor | Effect |
//! |------------|-------|--------|
//! | default | `INFO` | per-package status + warnings + errors |
//! | `-q` or `-j>1` | `WARN` | drop the per-package info noise (interleaving under
//! |   | |   jobs, or requested quiet) — problems still surface |
//! | `-v` | `INFO` | unchanged — `-v` is a *display* flag (it makes the human
//! |   | |   renderer label each phase), not a developer-tracing switch |
//! | `-vv` | `DEBUG` | developer detail |
//! | `-vvv` | `TRACE` | everything |
//!
//! Errors always print (the floor is a maximum level, never below `WARN` for
//! any real run). Later, a bus `Layer` is stacked on the same registry to also
//! feed diagnostics into the activity protocol.
//!
//! The floor applies to **our** crates only; everything else (brush, reqwest,
//! …) stays pinned at `WARN`. `-v` means "tell me more about what `em` is
//! doing", not "dump every word brush expands" — brush-core traces each
//! expansion, command and parse under flat targets (`expansion`, `commands`,
//! …), so a blanket `DEBUG` buries the build log. `RUST_LOG` overrides the
//! whole thing when that detail *is* what you want
//! (`RUST_LOG=expansion=debug`).

use tracing_subscriber::filter::{LevelFilter, Targets};

/// Crates whose events the verbosity flags actually govern. Third-party
/// targets are deliberately absent: they keep the `WARN` default.
const OURS: &[&str] = &[
    "portage_cli",
    "portage_atom",
    "portage_atom_pubgrub",
    "portage_atom_resolvo",
    "portage_binpkg",
    "portage_distfiles",
    "portage_metadata",
    "portage_repo",
    "portage_resolve",
    "portage_solver",
    "portage_vdb",
    "gentoo_core",
    "gentoo_interner",
    "gentoo_stages",
];

/// The level floor for our own crates, per the table above.
fn floor(quiet: bool, verbose: u8, parallel: bool) -> LevelFilter {
    if verbose >= 3 {
        LevelFilter::TRACE
    } else if verbose >= 2 {
        LevelFilter::DEBUG
    } else if quiet || parallel {
        LevelFilter::WARN
    } else {
        LevelFilter::INFO
    }
}

/// Build the filter both layers share: `WARN` for everything, raised to the
/// verbosity floor for [`OURS`]. A parseable `RUST_LOG` replaces it entirely,
/// which is the only way to reach a dependency's own debug traces.
fn filter(quiet: bool, verbose: u8, parallel: bool) -> Targets {
    if let Ok(spec) = std::env::var("RUST_LOG")
        && let Ok(targets) = spec.parse()
    {
        return targets;
    }
    Targets::new()
        .with_default(LevelFilter::WARN)
        .with_targets(OURS.iter().map(|t| (*t, floor(quiet, verbose, parallel))))
}

/// Install the global tracing subscriber. Call once at startup, before any
/// library code that emits tracing events.
///
/// `parallel` is whether more than one merge job will run concurrently (`-j>1`);
/// it drops the floor to `WARN` so per-package info does not interleave.
pub fn init(quiet: bool, verbose: u8, parallel: bool) {
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let floor = filter(quiet, verbose, parallel);

    // `with_ansi` defaults to always-on regardless of redirection unless
    // told otherwise — it does not auto-detect. Read the same global
    // `colorchoice` state `--color`/`NO_COLOR` set (via `anstream`, already
    // the source of truth for color everywhere else in this codebase — see
    // `portage_metadata::diagnostic`'s own use of the identical check) so
    // this "ERROR" tag agrees with the diagnostics it's printed alongside.
    let colored = !matches!(
        anstream::AutoStream::choice(&std::io::stderr()),
        anstream::ColorChoice::Never
    );

    // Two layers on one registry: console (stderr fmt) + the activity-bus
    // bridge (mirrors info/warn/error onto the current session's bus as
    // ActivityEvent::Diagnostic). Both honour the same level floor.
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .with_ansi(colored)
        .with_filter(floor.clone());
    let bus = crate::activity::BusLayer::new().with_filter(floor);

    // `try_init`: a nested re-entry (install __worker child) must not panic
    // when the subscriber is already set.
    let _ = tracing_subscriber::registry()
        .with(fmt)
        .with(bus)
        .try_init();
}

#[cfg(test)]
mod tests {
    use tracing::Level;

    use super::*;

    // The litter this filter exists to stop: brush-core traces every word it
    // expands under target "expansion", so `em -v` used to interleave
    // "DEBUG Basic expanding: '…'" through the whole build log. Two separate
    // guards — a plain `-v` reaches no debug at all, and even the explicit
    // developer levels leave brush's categories alone.
    #[test]
    fn plain_verbose_enables_no_debug_anywhere() {
        let f = filter(false, 1, false);
        for target in ["portage_cli::merge", "expansion", "brush_core::shell"] {
            assert!(
                !f.would_enable(target, &Level::DEBUG),
                "-v must stay out of {target} debug"
            );
        }
        assert!(f.would_enable("portage_cli::merge", &Level::INFO));
    }

    #[test]
    fn developer_levels_skip_brush_trace_categories() {
        let f = filter(false, 2, false);
        assert!(f.would_enable("portage_cli::merge", &Level::DEBUG));
        for category in ["expansion", "commands", "parse", "pattern", "jobs"] {
            assert!(
                !f.would_enable(category, &Level::DEBUG),
                "{category} debug must stay off under -vv"
            );
        }
    }

    #[test]
    fn dependency_warnings_still_surface() {
        let f = filter(false, 0, false);
        assert!(f.would_enable("expansion", &Level::WARN));
        assert!(f.would_enable("brush_core::shell", &Level::ERROR));
        assert!(!f.would_enable("portage_repo::build", &Level::DEBUG));
        assert!(f.would_enable("portage_repo::build", &Level::INFO));
    }

    #[test]
    fn quiet_and_parallel_drop_our_own_info() {
        for f in [filter(true, 0, false), filter(false, 0, true)] {
            assert!(!f.would_enable("portage_cli::merge", &Level::INFO));
            assert!(f.would_enable("portage_cli::merge", &Level::WARN));
        }
    }
}
