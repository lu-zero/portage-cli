use std::io::Write;

use brush_core::builtins;
use clap::Parser;

// ── P1 output helpers ─────────────────────────────────────────────────────────

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
