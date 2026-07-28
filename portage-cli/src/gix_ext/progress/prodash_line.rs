//! prodash line renderer UI (`feature = "rich"`).

use std::sync::Arc;
use std::time::Duration;

pub struct Guard {
    root: Option<Arc<prodash::tree::Root>>,
    handle: Option<prodash::render::line::JoinHandle>,
}

pub fn spawn(root: Arc<prodash::tree::Root>) -> Guard {
    let handle = prodash::render::line(
        std::io::stderr(),
        Arc::downgrade(&root),
        prodash::render::line::Options {
            level_filter: None,
            frames_per_second: 6.0,
            initial_delay: Some(Duration::from_millis(100)),
            throughput: true,
            hide_cursor: false,
            timestamp: false,
            keep_running_if_progress_is_empty: true,
            ..prodash::render::line::Options::default()
        }
        .auto_configure(prodash::render::line::StreamKind::Stderr),
    );
    Guard {
        root: Some(root),
        handle: Some(handle),
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Weak dies when we drop our Arc clone; don't send Quit (channel deadlock).
        self.root.take();
        if let Some(mut handle) = self.handle.take() {
            handle.disconnect();
            drop(handle);
        }
    }
}
