//! Fan-out hub: direct durable sinks + lossy broadcast for UI subscribers

use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use super::event::ActivityEvent;

/// Capacity for in-process broadcast subscribers
///
/// Lagging UIs drop events; durable sinks never use this path.
const BROADCAST_CAPACITY: usize = 1024;

/// Maximum durable events retained by one background sink before producers wait.
const BACKGROUND_QUEUE_CAPACITY: usize = 1024;

/// Receives every event on the durable path (must not drop)
pub trait ActivitySink: Send + Sync {
    fn on_event(&self, event: &ActivityEvent);
}

struct Inner {
    tx: broadcast::Sender<ActivityEvent>,
    sinks: Mutex<Vec<Arc<dyn ActivitySink>>>,
}

/// Process-wide (or driver-wide) activity bus
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

    /// In-process consumer (crossdev-stages UI, tests, embedding apps)
    pub fn subscribe(&self) -> broadcast::Receiver<ActivityEvent> {
        self.inner.tx.subscribe()
    }

    /// Install a durable sink (live FS, history, emergelog, recording)
    pub fn add_sink(&self, sink: Arc<dyn ActivitySink>) {
        self.inner
            .sinks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(sink);
    }

    /// Emit to all direct sinks, then broadcast to subscribers
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

/// Test / in-memory sink that keeps every event in order
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

/// Message sent across the offload channel
enum Msg {
    Event(ActivityEvent),
    /// `flush()` barrier: the worker echoes once it has processed everything
    /// sent before this point.
    Barrier(std::sync::mpsc::Sender<()>),
}

/// Wraps any [`ActivitySink`] so its (potentially blocking, disk-bound)
/// `on_event` runs on a dedicated OS thread instead of the caller's thread.
///
/// This keeps [`ActivityBus::emit`] off the async merge scheduler and the
/// ebuild phase loop: durable sinks (live FS, history, emerge.log) do real I/O
/// on every phase transition, and without offload that I/O runs inline on the
/// single-threaded runtime that also drives the compile subprocesses.
///
/// Events reach the inner sink on one thread **in FIFO order** through a
/// bounded mpsc. The queue preserves the durable sinks' "must not drop"
/// guarantee; a slow sink applies backpressure once its bounded queue fills
/// instead of retaining an unbounded event backlog. [`Self::flush`] is a
/// synchronous barrier; `Drop` joins the worker so a session's final events
/// (e.g. `SessionEnd`) reach disk before return.
///
/// Cheap inline sinks (e.g. [`RecordingSink`] in tests, or a fast FD pipe) do
/// not need wrapping.
pub struct BackgroundSink {
    /// `None` once dropped / shutting down
    tx: Mutex<Option<std::sync::mpsc::SyncSender<Msg>>>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl BackgroundSink {
    /// Drain `inner` on a thread named `name` (e.g. `"em-activity-live"`)
    pub fn new(inner: Arc<dyn ActivitySink>, name: &str) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Msg>(BACKGROUND_QUEUE_CAPACITY);
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

    /// Block until the worker has processed every event emitted before this call
    ///
    /// Waits for queue space if the worker is behind.
    pub fn flush(&self) {
        let tx = {
            let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
            let Some(tx) = guard.as_ref() else {
                return;
            };
            tx.clone()
        };
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        if tx.send(Msg::Barrier(ack_tx)).is_err() {
            tracing::error!("activity sink worker is gone; flush dropped");
            return;
        }
        let _ = ack_rx.recv();
    }
}

impl ActivitySink for BackgroundSink {
    fn on_event(&self, event: &ActivityEvent) {
        let tx = {
            let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
            guard.as_ref().cloned()
        };
        if let Some(tx) = tx
            && tx.send(Msg::Event(event.clone())).is_err()
        {
            tracing::error!("activity sink worker is gone; event dropped");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::event::ACTIVITY_EVENT_VERSION;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    struct BlockingSink {
        started: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        released: Mutex<bool>,
        wake: Condvar,
        count: AtomicUsize,
    }

    impl BlockingSink {
        fn new(started: std::sync::mpsc::Sender<()>) -> Self {
            Self {
                started: Mutex::new(Some(started)),
                released: Mutex::new(false),
                wake: Condvar::new(),
                count: AtomicUsize::new(0),
            }
        }

        fn release(&self) {
            *self.released.lock().unwrap_or_else(|e| e.into_inner()) = true;
            self.wake.notify_all();
        }
    }

    impl ActivitySink for BlockingSink {
        fn on_event(&self, _event: &ActivityEvent) {
            let started = self
                .started
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(started) = started {
                let _ = started.send(());
            }
            let mut released = self.released.lock().unwrap_or_else(|e| e.into_inner());
            while !*released {
                released = self.wake.wait(released).unwrap_or_else(|e| e.into_inner());
            }
            self.count.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn heartbeat(at: u32) -> ActivityEvent {
        ActivityEvent::SessionHeartbeat {
            v: ACTIVITY_EVENT_VERSION,
            job_id: "bounded".into(),
            parent_job_id: None,
            at: f64::from(at),
            completed: at,
            failed: 0,
        }
    }

    #[test]
    fn background_queue_bounds_backlog_without_dropping_events() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let inner = Arc::new(BlockingSink::new(started_tx));
        let background = Arc::new(BackgroundSink::new(inner.clone(), "test-bg-bounded"));
        background.on_event(&heartbeat(0));
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker must enter the blocking sink");

        let sent = Arc::new(AtomicUsize::new(0));
        let producer_background = Arc::clone(&background);
        let producer_sent = Arc::clone(&sent);
        let producer = std::thread::spawn(move || {
            for at in 1..=BACKGROUND_QUEUE_CAPACITY + 1 {
                producer_background.on_event(&heartbeat(at as u32));
                producer_sent.fetch_add(1, Ordering::Relaxed);
            }
        });

        let target = BACKGROUND_QUEUE_CAPACITY + 1;
        let deadline = Instant::now() + Duration::from_millis(250);
        while sent.load(Ordering::Relaxed) < target && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let completed_while_blocked = sent.load(Ordering::Relaxed) >= target;
        inner.release();
        producer.join().unwrap();
        background.flush();
        assert!(
            !completed_while_blocked,
            "a full durable queue must apply backpressure"
        );
        assert_eq!(inner.count.load(Ordering::Relaxed), target + 1);
    }
}
