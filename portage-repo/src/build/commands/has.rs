use std::io::Write;

use brush_core::builtins;
use clap::Parser;

// ── has / hasv / hasq ─────────────────────────────────────────────────────────

/// `has <needle> [haystack...]`  (PMS 12.3.4)
///
/// Returns 0 (success) if needle equals any haystack word, 1 otherwise.
/// `hasq` is a deprecated alias registered under a separate name.
#[derive(Parser)]
pub(crate) struct HasCommand {
    needle: String,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    haystack: Vec<String>,
}

impl builtins::Command for HasCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        _context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let found = self.haystack.iter().any(|item| item == &self.needle);
        Ok(brush_core::ExecutionResult::new(u8::from(!found)))
    }
}

/// `hasv <needle> [haystack...]`  (PMS 12.3.4)
///
/// Like `has`, but also prints the needle to stdout if found.
#[derive(Parser)]
pub(crate) struct HasvCommand {
    needle: String,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    haystack: Vec<String>,
}

impl builtins::Command for HasvCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        if self.haystack.iter().any(|item| item == &self.needle) {
            let _ = writeln!(context.params.stdout(shell), "{}", self.needle);
            Ok(brush_core::ExecutionResult::success())
        } else {
            Ok(brush_core::ExecutionResult::new(1))
        }
    }
}

// ── in_iuse ───────────────────────────────────────────────────────────────────

/// `in_iuse <flag>`  (PMS 12.3.12)
///
/// Returns 0 if flag is in `IUSE_EFFECTIVE`, falling back to `$IUSE`
/// (stripping `+`/`-`) when that variable is unset.
#[derive(Parser)]
pub(crate) struct InIuseCommand {
    #[arg(allow_hyphen_values = true)]
    flag: String,
}

impl builtins::Command for InIuseCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let effective = shell
            .env_str("IUSE_EFFECTIVE")
            .map(|c| c.into_owned())
            .filter(|s| !s.is_empty());
        let haystack = effective.unwrap_or_else(|| {
            shell
                .env_str("IUSE")
                .map(|c| c.into_owned())
                .unwrap_or_default()
        });
        let found = haystack
            .split_whitespace()
            .any(|entry| entry.trim_start_matches(['+', '-']) == self.flag);
        Ok(brush_core::ExecutionResult::new(u8::from(!found)))
    }
}
