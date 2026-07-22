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
