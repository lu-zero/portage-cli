//! The terminal em's own lines share with a live progress display
//!
//! While `FEATURES=candy` draws its bars, anything else written to the terminal goes through
//! [`suspend`], which clears the bars, writes, and draws them again below the new line.

use std::io::Write;
use std::sync::Mutex;

use indicatif::MultiProgress;

static BARS: Mutex<Option<MultiProgress>> = Mutex::new(None);

/// Install the live bars, or remove them with `None`
pub(crate) fn set_bars(bars: Option<MultiProgress>) {
    *BARS.lock().unwrap_or_else(|e| e.into_inner()) = bars;
}

std::thread_local! {
    static SUSPENDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `write` with any live bars cleared, so its output lands above them
///
/// Nested calls write directly: the bars are already cleared, and indicatif's lock is not
/// reentrant.
pub(crate) fn suspend<R>(write: impl FnOnce() -> R) -> R {
    if SUSPENDED.get() {
        return write();
    }
    let bars = BARS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match bars {
        Some(bars) => bars.suspend(|| {
            SUSPENDED.set(true);
            let out = write();
            SUSPENDED.set(false);
            out
        }),
        None => write(),
    }
}

/// A stderr writer for one `tracing` event, written in one piece through [`suspend`]
///
/// While bars are drawn only warnings and errors are written: em's own narration would scroll
/// them away, as it is held back under `--jobs`.
pub(crate) struct Stderr {
    buf: Vec<u8>,
    keep: bool,
}

/// What `diag` hands its formatter: a [`Stderr`] per event
pub(crate) struct MakeStderr;

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for MakeStderr {
    type Writer = Stderr;

    fn make_writer(&'a self) -> Stderr {
        Stderr {
            buf: Vec::new(),
            keep: true,
        }
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Stderr {
        let bars = BARS.lock().unwrap_or_else(|e| e.into_inner()).is_some();
        Stderr {
            buf: Vec::new(),
            keep: !bars || *meta.level() <= tracing::Level::WARN,
        }
    }
}

impl Write for Stderr {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for Stderr {
    fn drop(&mut self) {
        if !self.keep || self.buf.is_empty() {
            return;
        }
        suspend(|| {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(&self.buf);
            let _ = err.flush();
        });
    }
}
