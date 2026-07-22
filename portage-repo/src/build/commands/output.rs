use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use brush_core::builtins;
use clap::Parser;

// ── P1 output helpers ─────────────────────────────────────────────────────────

/// Shared "suppress informational output" flag.
///
/// Mirrors the phase-log `quiet` bool (`EbuildShell::set_phase_log`) so build
/// helpers — `unpack`, the `e*` family — can honour `-q` / `-j>1` without each
/// threading the flag separately. `EbuildShell` flips it whenever it configures
/// phase logging; helpers read it via `context.shared`. Shared (Arc) across
/// every clone of the inner shell, like [`super::die::DieFlag`].
#[derive(Clone, Default)]
pub(crate) struct QuietFlag(pub(crate) Arc<AtomicBool>);

impl QuietFlag {
    pub(crate) fn set(&self, v: bool) {
        self.0.store(v, Ordering::Relaxed);
    }
    /// True when phase output is log-only (no console tee). Helpers that print
    /// one-line status use this to stay off the terminal under `-q` / `-j>1`.
    pub(crate) fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// `einfo/elog/ewarn/eerror/eqawarn/einfon <message>`
///
/// Prints ` * <message>` to stderr.  All these commands share the same format
/// in a plain terminal; colour is portage's concern.
#[derive(Parser)]
pub(crate) struct EchoMessageCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for EchoMessageCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let msg = self.message.join(" ");
        let _ = writeln!(context.params.stderr(shell), " * {msg}");
        Ok(brush_core::ExecutionResult::success())
    }
}

/// `ebegin <message>`
///
/// Prints ` * <message> ...` to stderr (beginning of a timed action).
#[derive(Parser)]
pub(crate) struct EbeginCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for EbeginCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let msg = self.message.join(" ");
        let _ = writeln!(context.params.stderr(shell), " * {msg} ...");
        Ok(brush_core::ExecutionResult::success())
    }
}

/// `eend [exit_code] [message]`
///
/// Prints `[ ok ]` (exit_code 0) or `[ !! ] message` (exit_code non-zero).
#[derive(Parser)]
pub(crate) struct EendCommand {
    exit_code: Option<u8>,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for EendCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let code = self.exit_code.unwrap_or(0);
        if code == 0 {
            let _ = writeln!(context.params.stderr(shell), " [ ok ]");
        } else {
            let msg = self.message.join(" ");
            let _ = writeln!(context.params.stderr(shell), " [ !! ] {msg}");
        }
        Ok(brush_core::ExecutionResult::new(code))
    }
}
