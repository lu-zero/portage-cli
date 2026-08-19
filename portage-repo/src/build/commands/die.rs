use std::io::Write;
use std::sync::{Arc, Mutex};

use brush_core::builtins;
use clap::Parser;

// ── die ───────────────────────────────────────────────────────────────────────

/// Cross-subshell `die` signal
///
/// `die` may run inside `$(...)` substitutions or helper-script pipelines
/// where its non-zero exit cannot abort the phase (bash semantics). Portage
/// solves this with a marker file plus a signal to the ebuild process; here
/// the flag is an `Arc` shared by every clone of the shell (subshells, the
/// hermetic baseline), so the phase driver can check it after the phase
/// function returns — see `EbuildShell::run_phase`.
#[derive(Clone, Default)]
pub(crate) struct DieFlag(pub(crate) Arc<Mutex<Option<String>>>);

impl DieFlag {
    /// Record a die (the first message wins)
    pub(crate) fn raise(&self, msg: &str) {
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = Some(msg.to_string());
        }
    }

    /// Clear and return any recorded die
    pub(crate) fn take(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// `die [message]`  (PMS 12.2.1)
///
/// Prints `die: <message>` to stderr, raises the shared [`DieFlag`], and
/// returns 1.
/// The "die: " prefix is load-bearing — it is matched by tests and by
/// `inherit` error paths to distinguish portage die output from other stderr.
#[derive(Parser)]
pub(crate) struct DieCommand {
    /// Honour `nonfatal` (PMS 12.3.1)
    ///
    /// In a `nonfatal` context return non-zero instead of aborting the
    /// build. Used e.g. by ninja-utils' `eninja`.
    #[arg(short = 'n')]
    nonfatal: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for DieCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let msg = self.message.join(" ");
        // PMS 12.3.1: `die -n` honours `nonfatal` — when called in a `nonfatal`
        // context, return non-zero instead of aborting (e.g. ninja-utils'
        // `eninja` does `die -n "… failed"`).
        if self.nonfatal
            && context
                .shell
                .env_str("PORTAGE_NONFATAL")
                .is_some_and(|v| v == "1")
        {
            let shell = context.shell;
            let _ = writeln!(context.params.stderr(shell), "die (nonfatal): {msg}");
            return Ok(brush_core::ExecutionResult::new(1));
        }
        if let Ok(flag) = context.shared::<DieFlag>() {
            flag.raise(&msg);
        }
        let shell = context.shell;
        let _ = writeln!(context.params.stderr(shell), "die: {msg}");
        Ok(brush_core::ExecutionResult::new(1))
    }
}
