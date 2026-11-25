//! Subprocess JSONL sink (`--activity-fd`) and worker re-emit path

use std::fs::File;
use std::io::Write;
use std::os::fd::{BorrowedFd, OwnedFd, RawFd};
use std::sync::Mutex;

use super::bus::ActivitySink;
use super::event::ActivityEvent;

/// Writes one JSON object per line to a file descriptor
pub struct JsonlFdSink {
    file: Mutex<File>,
}

impl JsonlFdSink {
    /// A sink on a duplicate of `fd`, which stays open: every merge of a multi-step run attaches one
    pub fn from_raw_fd(fd: RawFd) -> std::io::Result<Self> {
        // SAFETY: the caller handed `fd` to this process to write to and does not close it; dup fails
        // with EBADF if it is not open.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let file = File::from(borrowed.try_clone_to_owned()?);
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    pub fn from_owned_fd(fd: OwnedFd) -> Self {
        Self {
            file: Mutex::new(File::from(fd)),
        }
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

    /// Connect to a parent-created Unix domain socket (install-worker re-emit)
    #[cfg(unix)]
    pub fn connect_reemit(path: &str) -> std::io::Result<Self> {
        use std::os::unix::net::UnixStream;
        // UnixStream → OwnedFd → File, all via the I/O-safety From impls.
        let stream = UnixStream::connect(path)?;
        Ok(Self::from_owned_fd(stream.into()))
    }

    #[cfg(not(unix))]
    pub fn connect_reemit(_path: &str) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "activity re-emit requires Unix domain sockets",
        ))
    }
}

impl ActivitySink for JsonlFdSink {
    fn on_event(&self, event: &ActivityEvent) {
        let Ok(mut line) = event.to_jsonl_line() else {
            return;
        };
        line.push('\n');
        // One write per line: writers sharing a FIFO stay whole up to PIPE_BUF.
        let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let _ = f.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::fd::AsRawFd;

    use super::*;

    #[test]
    fn dropping_a_sink_leaves_the_callers_fd_open() {
        let (mut reader, writer) = std::io::pipe().unwrap();
        drop(JsonlFdSink::from_raw_fd(writer.as_raw_fd()).unwrap());
        let mut second = JsonlFdSink::from_raw_fd(writer.as_raw_fd()).unwrap();
        second.file.get_mut().unwrap().write_all(b"x").unwrap();
        drop((second, writer));
        let mut text = String::new();
        reader.read_to_string(&mut text).unwrap();
        assert_eq!(text, "x");
    }
}
