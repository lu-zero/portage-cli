//! Fan-out hub: direct durable sinks + lossy broadcast for UI subscribers.

use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use super::event::ActivityEvent;

/// Capacity for in-process broadcast subscribers. Lagging UIs drop events;
/// durable sinks never use this path.
const BROADCAST_CAPACITY: usize = 1024;

/// Receives every event on the durable path (must not drop).
pub trait ActivitySink: Send + Sync {
    fn on_event(&self, event: &ActivityEvent);
}

struct Inner {
    tx: broadcast::Sender<ActivityEvent>,
    sinks: Mutex<Vec<Arc<dyn ActivitySink>>>,
}

/// Process-wide (or driver-wide) activity bus.
///
/// ```text
/// emit(event)
///   ├─► direct sinks  (live FS, history, emergelog — never drop)
///   └─► broadcast     (UI / tests — may lag and miss)
/// ```
#[derive(Clone)]
pub struct ActivityBus {
    inner: Arc<Inner>,
}

impl Default for ActivityBus {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                tx,
                sinks: Mutex::new(Vec::new()),
            }),
        }
    }

    /// In-process consumer (crossdev-stages UI, tests, embedding apps).
    pub fn subscribe(&self) -> broadcast::Receiver<ActivityEvent> {
        self.inner.tx.subscribe()
    }

    /// Install a durable sink (live FS, history, emergelog, recording).
    pub fn add_sink(&self, sink: Arc<dyn ActivitySink>) {
        self.inner
            .sinks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(sink);
    }

    /// Emit to all direct sinks, then broadcast to subscribers.
    pub fn emit(&self, event: ActivityEvent) {
        {
            let sinks = self.inner.sinks.lock().unwrap_or_else(|e| e.into_inner());
            for sink in sinks.iter() {
                sink.on_event(&event);
            }
        }
        // Ignore "no receivers" — normal when only direct sinks are attached.
        let _ = self.inner.tx.send(event);
    }
}

/// Test / in-memory sink that keeps every event in order.
#[derive(Default)]
pub struct RecordingSink {
    events: Mutex<Vec<ActivityEvent>>,
}

impl RecordingSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<ActivityEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn clear(&self) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

impl ActivitySink for RecordingSink {
    fn on_event(&self, event: &ActivityEvent) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event.clone());
    }
}

/// Message sent across the offload channel.
enum Msg {
    Event(ActivityEvent),
    /// `flush()` barrier: the worker echoes once it has processed everything
    /// sent before this point.
    Barrier(std::sync::mpsc::Sender<()>),
}

/// Wraps any [`ActivitySink`] so its (potentially blocking, disk-bound)
/// `on_event` runs on a dedicated OS thread instead of the caller's thread.
///
/// This keeps [`ActivityBus::emit`] non-blocking for the async merge scheduler
/// and the ebuild phase loop: durable sinks (live FS, history, emerge.log) do
/// real I/O on every phase transition, and without offload that I/O runs inline
/// on the single-threaded runtime that also drives the compile subprocesses.
///
/// Events reach the inner sink on one thread **in FIFO order** through an
/// unbounded mpsc, so durable sinks keep their "must not drop" guarantee
/// (the design's decision #4: durability never rides on the lossy broadcast
/// path). [`Self::flush`] is a synchronous barrier; `Drop` joins the worker so
/// a session's final events (e.g. `SessionEnd`) reach disk before return.
///
/// Cheap inline sinks (e.g. [`RecordingSink`] in tests, or a fast FD pipe) do
/// not need wrapping.
pub struct BackgroundSink {
    /// `None` once dropped / shutting down.
    tx: Mutex<Option<std::sync::mpsc::Sender<Msg>>>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl BackgroundSink {
    /// Drain `inner` on a thread named `name` (e.g. `"em-activity-live"`).
    pub fn new(inner: Arc<dyn ActivitySink>, name: &str) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        let handle = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                for msg in rx {
                    match msg {
                        Msg::Event(ev) => inner.on_event(&ev),
                        Msg::Barrier(ack) => {
                            let _ = ack.send(());
                        }
                    }
                }
            })
            .expect("spawn activity background sink thread");
        Self {
            tx: Mutex::new(Some(tx)),
            handle: Mutex::new(Some(handle)),
        }
    }

    /// Block until the worker has processed every event emitted before this
    /// call. Cheap and safe to call from the emit thread.
    pub fn flush(&self) {
        let ack = {
            let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
            let Some(tx) = guard.as_ref() else {
                return;
            };
            let (ack_tx, ack_rx) = std::sync::mpsc::channel();
            if tx.send(Msg::Barrier(ack_tx)).is_err() {
                return;
            }
            ack_rx
        };
        let _ = ack.recv();
    }
}

impl ActivitySink for BackgroundSink {
    fn on_event(&self, event: &ActivityEvent) {
        let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(Msg::Event(event.clone()));
        }
    }
}

impl Drop for BackgroundSink {
    fn drop(&mut self) {
        // Drop the sender first so the worker's receiver disconnects and the
        // drain loop ends; only then join, so pending events (a session's
        // final SessionEnd / history record) are written before we return.
        drop(self.tx.lock().unwrap_or_else(|e| e.into_inner()).take());
        if let Some(handle) = self.handle.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
    }
}
