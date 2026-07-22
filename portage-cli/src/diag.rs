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
//! | `-v` | `DEBUG` | add detail |
//! | `-vv` | `TRACE` | everything |
//!
//! Errors always print (the floor is a maximum level, never below `WARN` for
//! any real run). Later, a bus `Layer` is stacked on the same registry to also
//! feed diagnostics into the activity protocol.

use tracing_subscriber::filter::LevelFilter;

/// Install the global tracing subscriber. Call once at startup, before any
/// library code that emits tracing events.
///
/// `parallel` is whether more than one merge job will run concurrently (`-j>1`);
/// it drops the floor to `WARN` so per-package info does not interleave.
pub fn init(quiet: bool, verbose: u8, parallel: bool) {
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let floor = if verbose >= 2 {
        LevelFilter::TRACE
    } else if verbose >= 1 {
        LevelFilter::DEBUG
    } else if quiet || parallel {
        LevelFilter::WARN
    } else {
        LevelFilter::INFO
    };

    // Two layers on one registry: console (stderr fmt) + the activity-bus
    // bridge (mirrors info/warn/error onto the current session's bus as
    // ActivityEvent::Diagnostic). Both honour the same level floor.
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .with_filter(floor);
    let bus = crate::activity::BusLayer::new().with_filter(floor);

    // `try_init`: a nested re-entry (install __worker child) must not panic
    // when the subscriber is already set.
    let _ = tracing_subscriber::registry()
        .with(fmt)
        .with(bus)
        .try_init();
}
