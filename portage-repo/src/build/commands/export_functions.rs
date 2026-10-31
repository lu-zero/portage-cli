use std::io::Write;

use brush_core::builtins;

// ── EXPORT_FUNCTIONS ──────────────────────────────────────────────────────────

/// `EXPORT_FUNCTIONS <phase> [phase...]`  (PMS 10.2)
///
/// For each named phase, defines a wrapper function in the shell:
///   `${phase}() { ${ECLASS}_${phase} "$@"; }`
///
/// Ported from bash `eval` to a batched `run_string` call, eliminating the
/// bash for-loop + per-phase eval overhead.
#[derive(winnow_args::Args)]
#[arg(disable_help_flag, disable_version_flag, disable_help_subcommand)]
pub(crate) struct ExportFunctionsCommand {
    #[arg(positional, stop_flags, required)]
    phases: Vec<String>,
}

brush_winnow_args_adapter::impl_parse_args!(ExportFunctionsCommand);

impl<SE: brush_core::ShellExtensions> builtins::Builtin<SE> for ExportFunctionsCommand {
    type Error = brush_core::Error;

    async fn execute(
        self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;

        let eclass = match shell.env_str("ECLASS") {
            Some(e) if !e.is_empty() => e.into_owned(),
            _ => {
                let _ = writeln!(
                    context.params.stderr(shell),
                    "die: EXPORT_FUNCTIONS called outside eclass scope"
                );
                return Ok(brush_core::ExecutionResult::new(1));
            }
        };

        // Build all wrapper definitions as a single script and parse once.
        let script: String = self
            .phases
            .iter()
            .map(|phase| format!("{phase}() {{ {eclass}_{phase} \"$@\"; }}\n"))
            .collect();

        let source_info = brush_core::SourceInfo::from("EXPORT_FUNCTIONS");
        let params = shell.default_exec_params();
        shell.run_string(&script, &source_info, &params).await
    }
}
