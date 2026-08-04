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

use std::fmt;

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::registry::LookupSpan;

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

/// Whether stderr should get ANSI color (honours `--color` / `NO_COLOR` /
/// real TTY detection via `anstream`).
pub fn stderr_wants_color() -> bool {
    !matches!(
        anstream::AutoStream::choice(&std::io::stderr()),
        anstream::ColorChoice::Never
    )
}

/// Render a [`miette::Diagnostic`] code frame to stderr.
///
/// Color is decided here, at the UI boundary — never pre-baked into a
/// library error string that might later travel through tracing or JSONL.
pub fn print_diagnostic(diag: &dyn miette::Diagnostic) {
    let theme = if stderr_wants_color() {
        miette::GraphicalTheme::unicode()
    } else {
        miette::GraphicalTheme::unicode_nocolor()
    };
    let handler = miette::GraphicalReportHandler::new_themed(theme);
    let mut out = String::new();
    if handler.render_report(&mut out, diag).is_ok() {
        eprint!("{out}");
    }
}

/// Console event formatter: no span context, and portage's own `" * "`
/// marker convention for `WARN`/`ERROR` instead of a literal text tag.
///
/// `tracing_subscriber`'s default (`Full`) formatter prefixes every event
/// with its enclosing span scope, e.g. `pkg{cpv=sys-devel/binutils-2.46.1}:
/// phase{phase="fetch"}: fetch: binutils-2.46.1.tar.xz (already present)` —
/// the `pkg`/`phase` spans exist so [`crate::activity::BusLayer`] can label a
/// diagnostic with the package/phase it fired in (its own doc comment), not
/// for the console. There's no builder toggle to hide span context on the
/// stock formatter (the crate's `Full`/`Compact` formats always print it), so
/// this reimplements just enough of it.
///
/// `INFO` is this codebase's routine status channel (the doc table above:
/// "per-package status + warnings + errors"), and real portage draws on two
/// distinct conventions for it: a bare `">>> "` line for a major action
/// announcement (`Emerging`, `Unpacking`), and `einfo`'s colored `" * "`
/// marker for an ordinary informational note. A literal `"INFO"` tag matches
/// neither, and is redundant noise on top of a call site's own `">>> "`
/// prefix.
///
/// Which of the two an event is comes from its `tracing` **target**
/// (`portage_repo::ACTION_TARGET`, checked below), not from sniffing the
/// rendered message text for a `">>> "` prefix: the message is free-form
/// call-site text, not a stable contract to pattern-match on, and `tracing`
/// already gives every event a structured field for exactly this kind of
/// routing decision. An event on that target is left bare (it already wrote
/// its own `">>> "`); everything else gets `einfo`'s `" * "` in
/// [`crate::style::C_MARKER_INFO`].
///
/// `WARN`/`ERROR` map to real portage's `ewarn`/`eerror` (`portage/output.py`'s
/// `EOutput`): a colored `" * "` marker in [`crate::style::C_WARN`]/
/// [`crate::style::C_ERROR`], distinguished from `einfo` purely by *color*,
/// never by a literal `"WARN"`/`"ERROR"` word — so this drops the text tag
/// for those two levels in favor of the marker, matching portage's own
/// convention instead of inventing a different one. No call
/// site's own message text embeds a `"!!! "`/`">>> "` marker of its own for
/// `WARN`/`ERROR` today (checked directly, so this can't double them up) —
/// if one ever does, it should drop this crate's own `" * "` at that call
/// site instead of the reverse. `DEBUG`/`TRACE` keep the plain text tag: they
/// have no real-portage equivalent (developer detail, never in default
/// output — see the floor table above), so there's no convention to match.
struct CompactFormatter;

impl<S, N> FormatEvent<S, N> for CompactFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let level = event.metadata().level();
        if *level == Level::INFO {
            // An event on `portage_repo::ACTION_TARGET` already wrote its own
            // `">>> "` action-announcement prefix (`">>> Unpacking …"`) and
            // stays bare; everything else is an ordinary informational note
            // — real portage's `einfo` equivalent — and gets a green `" * "`.
            if event.metadata().target() != portage_repo::ACTION_TARGET {
                if writer.has_ansi_escapes() {
                    let s = crate::style::C_MARKER_INFO;
                    write!(writer, "{s} * {s:#}")?;
                } else {
                    write!(writer, " * ")?;
                }
            }
            ctx.format_fields(writer.by_ref(), event)?;
            return writeln!(writer);
        }
        match *level {
            Level::WARN | Level::ERROR => {
                if writer.has_ansi_escapes() {
                    // Matches portage/output.py: colorize("WARN"|"ERR", " * ").
                    let s = if *level == Level::WARN {
                        crate::style::C_WARN
                    } else {
                        crate::style::C_ERROR
                    };
                    write!(writer, "{s} * {s:#}")?;
                } else {
                    write!(writer, " * ")?;
                }
            }
            Level::DEBUG | Level::TRACE => {
                if writer.has_ansi_escapes() {
                    // Developer-only tags with no real-portage equivalent to
                    // match — plain `anstyle` colors, not part of the shared
                    // UI palette.
                    let color = if *level == Level::DEBUG {
                        anstyle::AnsiColor::Blue
                    } else {
                        anstyle::AnsiColor::Magenta
                    };
                    let s = anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(color)));
                    write!(writer, "{s}{level:>5}{s:#} ")?;
                } else {
                    write!(writer, "{level:>5} ")?;
                }
            }
            Level::INFO => unreachable!(),
        }
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
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
    // told otherwise — it does not auto-detect. Same colorchoice check as
    // [`stderr_wants_color`] / miette frames so tags and code frames agree.
    let colored = stderr_wants_color();

    // Two layers on one registry: console (stderr fmt) + the activity-bus
    // bridge (mirrors info/warn/error onto the current session's bus as
    // ActivityEvent::Diagnostic). Both honour the same level floor.
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .with_ansi(colored)
        .event_format(CompactFormatter)
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

    /// `CompactFormatter`'s own rendering, exercised end to end through a
    /// real `tracing_subscriber` registry (not just the filter): `INFO` gets
    /// einfo's `" * "` marker unless it is an `ACTION_TARGET` event (which
    /// stays bare), `WARN`/`ERROR` get portage's colored `" * "` marker —
    /// never a literal `"WARN"`/`"ERROR"` word — and no span-context prefix
    /// leaks in even though the event fires inside `pkg`/`phase` spans
    /// (mirroring `BusLayer`'s own real usage).
    #[test]
    fn compact_formatter_matches_portage_conventions() {
        use std::sync::{Arc, Mutex};

        use tracing_subscriber::layer::SubscriberExt;

        #[derive(Clone)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().write(b)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
            type Writer = Buf;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        fn render(ansi: bool) -> String {
            let buf = Buf(Arc::new(Mutex::new(Vec::new())));
            let fmt = tracing_subscriber::fmt::layer()
                .with_writer(buf.clone())
                .with_target(false)
                .without_time()
                .with_ansi(ansi)
                .event_format(CompactFormatter);
            let subscriber = tracing_subscriber::registry().with(fmt);
            tracing::subscriber::with_default(subscriber, || {
                let pkg = tracing::info_span!("pkg", cpv = "sys-devel/binutils-2.46.1");
                let _pkg = pkg.enter();
                let phase = tracing::info_span!("phase", phase = "fetch");
                let _phase = phase.enter();
                tracing::info!("fetch: binutils-2.46.1.tar.xz (already present)");
                tracing::info!(
                    target: portage_repo::ACTION_TARGET,
                    ">>> Unpacking binutils-2.46.1.tar.xz to /work"
                );
                tracing::warn!("something needs attention");
                tracing::error!("something failed");
            });
            let bytes = buf.0.lock().unwrap().clone();
            String::from_utf8(bytes).unwrap()
        }

        let plain = render(false);
        let mut lines = plain.lines();
        assert_eq!(
            lines.next().unwrap(),
            " * fetch: binutils-2.46.1.tar.xz (already present)",
            "an ordinary INFO note gets einfo's \" * \" marker, no span context: {plain:?}"
        );
        assert_eq!(
            lines.next().unwrap(),
            ">>> Unpacking binutils-2.46.1.tar.xz to /work",
            "an event on ACTION_TARGET (already carrying its own >>> prefix) \
             must stay bare, not get a second marker: {plain:?}"
        );
        assert_eq!(lines.next().unwrap(), " * something needs attention");
        assert_eq!(lines.next().unwrap(), " * something failed");

        let colored = render(true);
        assert!(
            colored.contains("\x1b[32m * \x1b[0mfetch: binutils"),
            "an ordinary INFO note's \" * \" marker is portage's darkgreen, \
             i.e. plain ANSI green: {colored:?}"
        );
        assert!(
            colored.contains("\x1b[33m * \x1b[0msomething needs attention"),
            "WARN must use portage's yellow \" * \" marker, not a text tag: {colored:?}"
        );
        assert!(
            colored.contains("\x1b[31m * \x1b[0msomething failed"),
            "ERROR must use portage's red \" * \" marker, not a text tag: {colored:?}"
        );
        assert!(
            !colored.contains("WARN") && !colored.contains("ERROR"),
            "no literal level word anywhere: {colored:?}"
        );
    }
}
