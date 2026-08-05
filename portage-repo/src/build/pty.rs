//! A pseudo-terminal for a build phase's output, portage's
//! `portage/util/_pty.py`.
//!
//! Ebuilds and the tools they call ask whether their output is going to a
//! terminal, and behave differently when it is not. `gentoo-functions` — the
//! `e*` implementation every *external* Gentoo tool uses (`binutils-config`,
//! `gcc-config`, `eltpatch`, and so the `elibtoolize` every autotools package
//! calls) — checks `[ -t 1 ]` on **each** call before painting anything, and
//! again before placing `eend`'s indicator with cursor motion. A phase whose
//! stdout is a pipe fails both, which is why those tools came out flat and
//! wrapped onto two lines under `em` even in a fully capable terminal.
//!
//! No amount of `TERM`/`COLUMNS` fixes that; only an actual terminal on fd 1
//! does. So `em` does what portage does: give the phase a pty, and read the
//! other end.
//!
//! The shape here is deliberately conservative, because the obvious
//! implementations have sharp edges:
//!
//! - The pty is **never handed to `brush`**. The redirection stays a plain
//!   shell redirect to the slave's path, exactly as the `tee` it replaces was
//!   a plain shell redirect — so nothing depends on how `brush` tracks,
//!   clones, or closes an `OpenFile`, and no shell-side fd can outlive the
//!   phase and hold the pty open.
//! - The reader is a **dedicated OS thread**, not a `tokio` task. The phase
//!   blocks on writing once the pty buffer fills, so the reader has to make
//!   progress for the phase to finish; a task could be starved by the same
//!   `tokio` LIFO-slot scheduling that already produced one process-
//!   substitution deadlock in this codebase, and a thread cannot.
//! - Shutdown is by **fd lifetime, not a sentinel**: this module keeps its own
//!   slave open for the whole phase (so an early `read` cannot see `EIO`
//!   before the shell has opened its end), then closes it once the phase
//!   returns. The kernel hands back everything still buffered and only then
//!   reports `EIO`, which is the reader's cue to stop — no marker byte to
//!   inject, and none to filter back out of the output.
//!
//! Every failure path falls back to the previous `tee`, as portage falls back
//! to a plain pipe.

use std::io::Write;
use std::os::fd::OwnedFd;

use camino::Utf8Path;

/// A phase's pty: the shell writes to [`slave_path`](Self::slave_path), and a
/// reader thread copies what comes out of the master to the console and the
/// build log.
pub(crate) struct PhasePty {
    /// Held open for the phase's lifetime so the master never reports `EIO`
    /// early; dropping it is what tells the reader to stop.
    slave: Option<OwnedFd>,
    slave_path: String,
    /// Set once the phase has returned, so the reader stops waiting on a
    /// straggler that inherited fd 1 instead of hanging the merge.
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl PhasePty {
    /// Open a pty for one phase, or `None` if this process has no terminal to
    /// render to or anything about the setup fails.
    ///
    /// `log` is opened here rather than in the reader so that a log that
    /// cannot be written is a reason to fall back to the `tee` (which would
    /// report it) instead of silently dropping the phase's output.
    pub(crate) fn open(log: &Utf8Path) -> Option<Self> {
        // No console means nothing a terminal would buy us: a pty would only
        // add escape sequences to a log nobody is watching. Portage gates its
        // own winsize copy on the same check.
        if !rustix::termios::isatty(std::io::stdout()) {
            return None;
        }
        Self::open_with_size(log, rustix::termios::tcgetwinsize(std::io::stdout()).ok())
    }

    /// [`open`](Self::open) with the console check and the size lookup already
    /// done, so a test can drive a real pty without owning a terminal.
    fn open_with_size(log: &Utf8Path, winsize: Option<rustix::termios::Winsize>) -> Option<Self> {
        let master =
            rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
                .ok()?;
        rustix::pty::grantpt(&master).ok()?;
        rustix::pty::unlockpt(&master).ok()?;
        let slave_path = rustix::pty::ptsname(&master, Vec::new())
            .ok()?
            .into_string()
            .ok()?;

        // Opening the slave here is both the handle that keeps the pty alive
        // and the check that the shell will be able to open it: `brush` runs
        // in *this* process, so its redirect opens the same path from the same
        // mount namespace and credentials. If this fails, the redirect would
        // have failed too — and a failed redirect means the phase never runs
        // at all, so it must be caught before the phase is built around it.
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&slave_path)
            .ok()?;

        // Post-processing off, or every `\n` the phase writes becomes `\r\n`
        // in the build log (`_pty.py`'s "weird things ... may occur").
        let mut mode = rustix::termios::tcgetattr(&slave).ok()?;
        mode.output_modes -= rustix::termios::OutputModes::OPOST;
        rustix::termios::tcsetattr(&slave, rustix::termios::OptionalActions::Now, &mode).ok()?;

        // The size the phase sees has to be the real one, or `eend` aligns its
        // indicator to the wrong column and every `stty size` caller guesses.
        if let Some(size) = winsize {
            let _ = rustix::termios::tcsetwinsize(&slave, size);
        }

        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .ok()?;

        let master = std::fs::File::from(master);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = std::thread::Builder::new()
            .name("em-phase-pty".to_string())
            .spawn({
                let stop = std::sync::Arc::clone(&stop);
                move || pump(master, log_file, &stop)
            })
            .ok()?;

        Some(Self {
            slave: Some(slave.into()),
            slave_path,
            stop,
            reader: Some(reader),
        })
    }

    /// The path the phase's output should be redirected to.
    pub(crate) fn slave_path(&self) -> &str {
        &self.slave_path
    }
}

impl Drop for PhasePty {
    /// Close this side of the pty and wait for the reader to finish draining.
    ///
    /// In `Drop` rather than an explicit `finish()` so that an early return
    /// from the phase — a `die`, an I/O error, a cancelled future — still
    /// flushes the output the phase managed to produce before it failed, which
    /// is exactly the output someone debugging that failure needs.
    fn drop(&mut self) {
        // Last slave closed: the master returns what is still buffered and
        // then `EIO`, which ends the reader's loop. `stop` is the backstop for
        // when something the phase spawned still holds a slave open.
        self.slave.take();
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Copy the master to the console and the build log until the pty is closed.
///
/// Reads are written straight through with no interpretation: what the phase
/// emitted is what the terminal and the log get, escape sequences included —
/// the same contract the `tee` had, and the same content real portage's build
/// logs carry.
///
/// Waits with a timeout rather than blocking outright. Normally the loop ends
/// on its own the moment the last slave closes, but a phase that leaves a
/// background process holding fd 1 would keep the pty open indefinitely, and a
/// plain blocking read there would hang the merge rather than lose a stray
/// line of output. `stop` bounds that: it is set once the phase has returned,
/// and the next tick drains whatever is already buffered and gives up on the
/// straggler.
fn pump(master: std::fs::File, mut log: std::fs::File, stop: &std::sync::atomic::AtomicBool) {
    /// How long to keep draining after the phase has returned before giving up
    /// on whatever is still holding the pty open.
    const LINGER: std::time::Duration = std::time::Duration::from_secs(2);
    let tick = rustix::fs::Timespec {
        tv_sec: 0,
        tv_nsec: 100_000_000,
    };
    let nowait = rustix::fs::Timespec::default();

    let mut buf = [0u8; 8192];
    let mut deadline = None;
    loop {
        // Readability is always established before reading: the master blocks,
        // and after the phase returns there may be a straggler keeping the pty
        // open forever with nothing more to say.
        let done = stop.load(std::sync::atomic::Ordering::Relaxed);
        let mut fds = [rustix::event::PollFd::new(
            &master,
            rustix::event::PollFlags::IN,
        )];
        let ready =
            rustix::event::poll(&mut fds, Some(if done { &nowait } else { &tick })).unwrap_or(0);
        if ready == 0 {
            // Nothing queued and the phase is over: everything it wrote has
            // been forwarded.
            if done {
                break;
            }
            continue;
        }
        // A pty master reports `EIO` rather than end-of-file once no slave is
        // open; both mean the same thing here.
        match rustix::io::read(&master, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(chunk);
                let _ = out.flush();
                let _ = log.write_all(chunk);
            }
        }
        // A straggler that never stops writing must not hold the merge open
        // either, so the post-phase drain is bounded as well as non-blocking.
        if done
            && *deadline.get_or_insert_with(|| std::time::Instant::now() + LINGER)
                < std::time::Instant::now()
        {
            break;
        }
    }
    let _ = log.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point: what the phase writes to goes to a device that answers
    /// yes to `isatty`, which is what `gentoo-functions` tests before it will
    /// colour anything or place `eend`'s indicator.
    ///
    /// Also pins the two properties the output depends on: `OPOST` off, so a
    /// `\n` stays one byte instead of becoming `\r\n` in the build log; and a
    /// drop that drains, so nothing written just before the phase ended is
    /// lost to the race between closing the pty and stopping the reader.
    #[test]
    fn a_phase_pty_is_a_terminal_that_drains_into_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = camino::Utf8Path::from_path(dir.path())
            .unwrap()
            .join("build.log");
        let pty = PhasePty::open_with_size(&log, None).expect("pty");

        let mut phase = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pty.slave_path())
            .unwrap();
        assert!(
            rustix::termios::isatty(&phase),
            "a phase redirected here must see a terminal on fd 1"
        );
        phase.write_all(b"one\ntwo\n").unwrap();
        drop(phase);

        // Dropping is what closes the pty and joins the reader, so everything
        // written above has to be in the log once it returns.
        drop(pty);
        assert_eq!(
            std::fs::read_to_string(log.as_std_path()).unwrap(),
            "one\ntwo\n",
            "OPOST must stay off — a \\r here means every build log gains CRs"
        );
    }

    /// The log is appended to, never truncated: a phase's output has to land
    /// after the `>>> <phase>` marker the driver already wrote, and after
    /// whatever earlier phases logged.
    #[test]
    fn the_log_is_appended_to() {
        let dir = tempfile::tempdir().unwrap();
        let log = camino::Utf8Path::from_path(dir.path())
            .unwrap()
            .join("build.log");
        std::fs::write(log.as_std_path(), ">>> src_compile\n").unwrap();

        let pty = PhasePty::open_with_size(&log, None).expect("pty");
        let mut phase = std::fs::OpenOptions::new()
            .write(true)
            .open(pty.slave_path())
            .unwrap();
        phase.write_all(b"building\n").unwrap();
        drop(phase);
        drop(pty);

        assert_eq!(
            std::fs::read_to_string(log.as_std_path()).unwrap(),
            ">>> src_compile\nbuilding\n"
        );
    }

    /// A phase that leaves something holding fd 1 must not hang the merge.
    /// The straggler here never closes its end, so nothing will ever close the
    /// pty on its own — dropping still has to return, and still has to have
    /// written what was actually produced.
    #[test]
    fn a_straggler_holding_the_pty_open_does_not_hang_the_drop() {
        let dir = tempfile::tempdir().unwrap();
        let log = camino::Utf8Path::from_path(dir.path())
            .unwrap()
            .join("build.log");
        let pty = PhasePty::open_with_size(&log, None).expect("pty");

        let mut straggler = std::fs::OpenOptions::new()
            .write(true)
            .open(pty.slave_path())
            .unwrap();
        straggler.write_all(b"last words\n").unwrap();

        let started = std::time::Instant::now();
        drop(pty);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "drop must be bounded, not wait on a slave nobody will close"
        );
        assert_eq!(
            std::fs::read_to_string(log.as_std_path()).unwrap(),
            "last words\n"
        );
        drop(straggler);
    }

    /// With no console there is nothing to render to, so the caller keeps its
    /// existing `tee`. This is the path CI and every piped invocation take —
    /// `cargo test`'s own stdout is not a terminal, which is what makes it
    /// assertable here.
    #[test]
    fn no_console_means_no_pty() {
        assert!(!rustix::termios::isatty(std::io::stdout()));
        let dir = tempfile::tempdir().unwrap();
        let log = camino::Utf8Path::from_path(dir.path())
            .unwrap()
            .join("build.log");
        assert!(PhasePty::open(&log).is_none());
        assert!(!log.exists(), "the fallback path must not create the log");
    }
}
