//! Tracing → activity-bus bridge
//!
//! A [`tracing_subscriber::Layer`] that surfaces diagnostics (info/warn/error)
//! onto the current session's [`ActivityBus`] as [`ActivityEvent::Diagnostic`],
//! so `--activity-fd` and front-end consumers see them in the same structured
//! stream as `PkgStart`/`PhaseEnter`. The console `fmt` layer still renders
//! them to stderr; this layer only adds the bus channel.
//!
//! The merge driver installs the session ([`set_session`]) once it has built
//! the bus and resolved the job id; an install `__worker` child is a separate
//! process that never installs one, so its events simply have no bus to reach.
//!
//! `cpv`/`phase` are read from the nearest `pkg`/`phase` tracing spans (recorded
//! via `on_new_span`), so a diagnostic like `fowners: … leaving as-is` is
//! labelled with the package/phase it fired in.

use std::sync::{Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::{Layer, registry::LookupSpan};

use super::bus::ActivityBus;
use super::event::{ACTIVITY_EVENT_VERSION, ActivityEvent, DiagnosticLevel};

/// The session the layer forwards diagnostics to. One per process in practice
static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

struct Session {
    bus: ActivityBus,
    job_id: String,
}

/// Install (or replace) the session the bus layer forwards diagnostics to
pub fn set_session(bus: ActivityBus, job_id: &str) {
    let slot = SESSION.get_or_init(|| Mutex::new(None));
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Session {
        bus,
        job_id: job_id.to_string(),
    });
}

/// Drop the session at end of a merge run so later diagnostics do not reference
/// a bus whose sinks are being torn down.
pub fn clear_session() {
    if let Some(slot) = SESSION.get() {
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Per-span recorded fields: `cpv` (from `pkg` spans) and `phase` (from `phase`
/// spans), so an event's nearest ancestor spans label the diagnostic.
#[derive(Default)]
struct SpanFields {
    cpv: Option<String>,
    phase: Option<String>,
}

struct FieldRecorder<'a>(&'a mut SpanFields);
impl Visit for FieldRecorder<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "cpv" => self.0.cpv = Some(fmt_debug_value(value)),
            "phase" => self.0.phase = Some(fmt_debug_value(value)),
            _ => {}
        }
    }
}

struct MsgRecorder<'a>(&'a mut String);
impl Visit for MsgRecorder<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            *self.0 = fmt_debug_value(value);
        }
    }
}

// Fields recorded with the `%` (Display) sigil arrive as a wrapper whose Debug
// delegates to Display, so `{value:?}` yields the clean string. For genuinely
// Debug-recorded values this is still reasonable.
fn fmt_debug_value(value: &dyn std::fmt::Debug) -> String {
    let s = format!("{value:?}");
    // Strip a single layer of surrounding quotes if the value was a plain str
    // recorded without `%`.
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_string()
    } else {
        s
    }
}

/// Tracing layer that mirrors info/warn/error events onto the activity bus
pub struct BusLayer;

impl BusLayer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BusLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for BusLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut fields = SpanFields::default();
        attrs.record(&mut FieldRecorder(&mut fields));
        span.extensions_mut().insert(fields);
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let Some(slot) = SESSION.get() else {
            return;
        };
        // Recursion guard: never re-emit this crate's own events (the bus/sinks
        // must not feed back through the subscriber).
        if event
            .metadata()
            .target()
            .starts_with("portage_cli::activity")
        {
            return;
        }
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => DiagnosticLevel::Error,
            tracing::Level::WARN => DiagnosticLevel::Warn,
            tracing::Level::INFO => DiagnosticLevel::Info,
            _ => return, // DEBUG/TRACE are not surfaced on the bus
        };

        // Nearest ancestor span wins for each field.
        let mut cpv = None;
        let mut phase = None;
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope {
                if let Some(f) = span.extensions().get::<SpanFields>() {
                    if cpv.is_none() {
                        cpv = f.cpv.clone();
                    }
                    if phase.is_none() {
                        phase = f.phase.clone();
                    }
                }
            }
        }

        let mut msg = String::new();
        event.record(&mut MsgRecorder(&mut msg));
        if msg.is_empty() {
            return;
        }

        let handle = slot.lock().unwrap_or_else(|e| e.into_inner());
        let Some(session) = handle.as_ref() else {
            return;
        };
        session.bus.emit(ActivityEvent::Diagnostic {
            v: ACTIVITY_EVENT_VERSION,
            job_id: session.job_id.as_str().into(),
            parent_job_id: None,
            level,
            cpv: cpv.map(Into::into),
            phase: phase.map(Into::into),
            msg: msg.into(),
            at: ActivityEvent::now(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{ActivityBus, RecordingSink};
    use std::sync::Arc;

    // Both tests mutate the process-global SESSION; serialize them so they
    // don't clobber each other's session when cargo runs tests in parallel.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn diagnostic_event_round_trips() {
        let ev = ActivityEvent::Diagnostic {
            v: ACTIVITY_EVENT_VERSION,
            job_id: "j".into(),
            parent_job_id: None,
            level: DiagnosticLevel::Warn,
            cpv: Some("sys-devel/gcc-1".into()),
            phase: Some("compile".into()),
            msg: "fowners: leaving as-is".into(),
            at: 1.5,
        };
        let line = ev.to_jsonl_line().unwrap();
        assert!(line.contains("\"event\":\"diagnostic\""));
        assert!(line.contains("\"level\":\"warn\""));
        let back = ActivityEvent::from_jsonl_line(&line).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn layer_emits_diagnostic_for_warn_event() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let _lock = TEST_LOCK.lock().unwrap();
        // Install a fresh bus as the session target.
        let bus = ActivityBus::new();
        let rec = Arc::new(RecordingSink::new());
        bus.add_sink(rec.clone());
        set_session(bus, "job-layer");

        let sub = tracing_subscriber::registry()
            .with(BusLayer::new())
            .set_default();
        let _pkg = tracing::info_span!("pkg", cpv = %"app-misc/foo-1").entered();
        tracing::warn!(target: "test_diagnostic_source", "test warning");
        drop(sub);

        clear_session();
        let events = rec.events();
        let diags: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, ActivityEvent::Diagnostic { .. }))
            .collect();
        assert_eq!(diags.len(), 1);
        if let ActivityEvent::Diagnostic {
            level, msg, cpv, ..
        } = &diags[0]
        {
            assert_eq!(*level, DiagnosticLevel::Warn);
            assert_eq!(msg.as_ref(), "test warning");
            assert_eq!(cpv.as_deref(), Some("app-misc/foo-1"));
        } else {
            panic!("expected Diagnostic");
        }
    }

    #[test]
    fn parallel_packages_do_not_mix_their_diagnostics() {
        use tracing::Instrument;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let _lock = TEST_LOCK.lock().unwrap();
        let bus = ActivityBus::new();
        let rec = Arc::new(RecordingSink::new());
        bus.add_sink(rec.clone());
        set_session(bus, "job-mix");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let _sub = tracing_subscriber::registry()
                .with(BusLayer::new())
                .set_default();
            // Two "packages" run concurrently; each emits a diagnostic inside
            // its own instrumented pkg span. Their events must not cross-wire.
            let task_a = async {
                tracing::warn!(target: "mix", "from-a");
            }
            .instrument(tracing::info_span!("pkg", cpv = %"pkg-a"));
            let task_b = async {
                tracing::warn!(target: "mix", "from-b");
            }
            .instrument(tracing::info_span!("pkg", cpv = %"pkg-b"));
            futures_util::join!(task_a, task_b);
        });

        clear_session();
        let events = rec.events();
        let by_cpv: std::collections::HashMap<&str, &str> = events
            .iter()
            .filter_map(|e| match e {
                ActivityEvent::Diagnostic { cpv, msg, .. } => Some((cpv.as_deref()?, msg.as_ref())),
                _ => None,
            })
            .collect();
        assert_eq!(by_cpv.get("pkg-a").copied(), Some("from-a"));
        assert_eq!(by_cpv.get("pkg-b").copied(), Some("from-b"));
        assert_eq!(by_cpv.len(), 2, "exactly one event per package, no mixing");
    }
}
