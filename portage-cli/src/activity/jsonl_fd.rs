//! Subprocess JSONL sink (`--activity-fd`).

use std::fs::File;
use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::sync::Mutex;

use super::bus::ActivitySink;
use super::event::ActivityEvent;

/// Writes one JSON object per line to a raw FD (not closed on drop by us if
/// dup'd — we take ownership of the FD number the user passed).
pub struct JsonlFdSink {
    file: Mutex<File>,
}

impl JsonlFdSink {
    /// # Safety
    /// `fd` must be a valid open file descriptor; this takes ownership.
    pub fn from_raw_fd(fd: RawFd) -> std::io::Result<Self> {
        // SAFETY: caller guarantees `fd` is open and exclusively ours.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let file = File::from(owned);
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    pub fn from_path(path: &str) -> std::io::Result<Self> {
        let file = if path == "-" {
            // stdout is wrong for activity (conflicts with human output).
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "use --activity-fd=N instead of - for stdout",
            ));
        } else {
            File::options().create(true).append(true).open(path)?
        };
        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl ActivitySink for JsonlFdSink {
    fn on_event(&self, event: &ActivityEvent) {
        let Ok(line) = event.to_jsonl_line() else {
            return;
        };
        let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(f, "{line}");
        let _ = f.flush();
    }
}
