//! Progress sessions for pure-gix work (sync fetch/clone/hard-reset).
//!
//! ## Architecture
//!
//! gix expects a real [`gix::NestedProgress`] implementation. A hand-rolled
//! NestedProgress bridge (custom `inc_by` / `counter`) was leaving pack
//! counters stuck at **0 B** even while work ran — so we always use a
//! **prodash progress tree** for the NestedProgress side (same types gix is
//! built against).
//!
//! Display is separate:
//!
//! | Feature | UI |
//! |---------|-----|
//! | *(default)* | Poll the tree → **indicatif** multi-bars |
//! | `rich` | prodash **line renderer** on stderr |
//!
//! Quiet mode yields [`DoOrDiscard`] discards (no tree, no UI).

use gix::progress::DoOrDiscard;

#[cfg(not(feature = "rich"))]
mod indicatif_render;
#[cfg(feature = "rich")]
mod prodash_line;

/// One progress session spanning a sync step (fetch, clone, or hard-reset).
pub struct ProgressSession {
    root: Option<std::sync::Arc<prodash::tree::Root>>,
    /// Keeps the UI thread alive until drop.
    _ui: Option<UiGuard>,
}

/// Held only so Drop tears the UI down; not read elsewhere.
#[allow(dead_code)]
enum UiGuard {
    #[cfg(not(feature = "rich"))]
    Indicatif(indicatif_render::Guard),
    #[cfg(feature = "rich")]
    ProdashLine(prodash_line::Guard),
}

/// Child handle passed into gix — always a real prodash tree item when active.
pub type ProgressChild = DoOrDiscard<prodash::tree::Item>;

impl ProgressSession {
    /// `quiet` suppresses all progress UI (gix still gets a discard sink).
    pub fn new(quiet: bool) -> Self {
        if quiet {
            return Self {
                root: None,
                _ui: None,
            };
        }

        let root: std::sync::Arc<prodash::tree::Root> = std::sync::Arc::new(
            prodash::tree::root::Options {
                message_buffer_capacity: 200,
                ..Default::default()
            }
            .into(),
        );

        let ui = {
            #[cfg(not(feature = "rich"))]
            {
                UiGuard::Indicatif(indicatif_render::spawn(std::sync::Arc::clone(&root)))
            }
            #[cfg(feature = "rich")]
            {
                UiGuard::ProdashLine(prodash_line::spawn(std::sync::Arc::clone(&root)))
            }
        };

        Self {
            root: Some(root),
            _ui: Some(ui),
        }
    }

    /// A named progress node for a gix call (`prepare_fetch`, `receive`, …).
    pub fn child(&self, name: &str) -> ProgressChild {
        match &self.root {
            Some(root) => DoOrDiscard::from(Some(root.add_child(name))),
            None => DoOrDiscard::from(None),
        }
    }
}

impl Drop for ProgressSession {
    fn drop(&mut self) {
        // Drop UI first so it stops drawing, then the tree.
        self._ui.take();
        self.root.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix::{Count, NestedProgress, Progress};
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[test]
    fn tree_counters_advance_and_session_drops() {
        let session = ProgressSession::new(false);
        {
            let mut root = session.child("receive");
            root.init(None, gix::progress::bytes());
            let mut read = root.add_child("read pack");
            read.init(None, gix::progress::bytes());
            for _ in 0..100 {
                read.inc_by(1024);
            }
            assert!(read.step() >= 100 * 1024);
            // Value is also visible on the shared counter gix workers use.
            assert!(read.counter().load(Ordering::Relaxed) >= 100 * 1024);
        }
        // Let the UI poll once if it's the indicatif path.
        std::thread::sleep(Duration::from_millis(120));
        drop(session);
    }
}
