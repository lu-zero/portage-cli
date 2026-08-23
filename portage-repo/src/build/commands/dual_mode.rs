//! Single registration point for the PMS builtins that can be called from
//! ebuild/eclass global scope, and so are reachable during metadata-only
//! sourcing as well as real phase execution.
//!
//! `source_ebuild` only sources global scope and defines phase functions —
//! it never calls one — so a phase-body-only helper (`econf`, `emake`,
//! `eapply`, the `do*`/`new*` install helpers, …) is simply never invoked
//! during metadata extraction and is registered as its one real
//! implementation, unconditionally, like `die`/`has`/`use` (see
//! `EbuildShell::new_with_cache`). The names here — `einfo` and friends,
//! `has_version`/`best_version` — are the ones eclasses do call from global
//! scope, so what they dispatch to actually depends on which mode is
//! current.
//!
//! Each gets registered as either its real implementation or a stub —
//! plain `Shell::register_builtin` calls, nothing else. No bash function is
//! ever defined for any of these names, so there is no
//! function-shadows-builtin question (a bash function always outranks a
//! same-named builtin in this shell — that's what let `eapply`'s old
//! metadata-mode bash stub silently win over its real builtin for two
//! weeks after the migration to a builtin, `f811d8a`). Switching mode is
//! exactly overwriting this registry slot, called from
//! `EbuildShell::new_with_cache` (initial state), `source_ebuild` (every
//! call), and `init_build_env` (every real phase).

use brush_core::builtins;
use clap::Parser;

use super::output::{EbeginCommand, EchoMessageCommand, EendCommand};
use super::version_query::{BestVersionCommand, HasVersionCommand};

/// Which behavior every name in `set_tool_mode`'s table should currently
/// dispatch to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolMode {
    Metadata,
    Build,
}

/// Register each name here as its real implementation or its metadata-mode
/// stub. See the module doc comment for what belongs in this table and why.
pub(crate) fn set_tool_mode<SE: brush_core::ShellExtensions>(
    shell: &mut brush_core::Shell<SE>,
    mode: ToolMode,
) {
    macro_rules! reg {
        ($($name:literal => ($real:ty, $stub:ty)),+ $(,)?) => {$(
            shell.register_builtin($name, match mode {
                ToolMode::Build => builtins::builtin::<$real, _>(),
                ToolMode::Metadata => builtins::builtin::<$stub, _>(),
            });
        )+};
    }
    reg! {
        "einfo" => (EchoMessageCommand, NoopCommand),
        "einfon" => (EchoMessageCommand, NoopCommand),
        "elog" => (EchoMessageCommand, NoopCommand),
        "ewarn" => (EchoMessageCommand, NoopCommand),
        "eerror" => (EchoMessageCommand, NoopCommand),
        "eqawarn" => (EchoMessageCommand, NoopCommand),
        "ebegin" => (EbeginCommand, NoopCommand),
        "eend" => (EendCommand, EendStubCommand),
        "has_version" => (HasVersionCommand, HasVersionStubCommand),
        "best_version" => (BestVersionCommand, BestVersionStubCommand),
    }
}

/// Metadata-mode no-op: succeeds immediately, ignoring all arguments.
/// Reused for every dual-mode name with no other fallback behavior of its
/// own.
#[derive(Parser)]
pub(crate) struct NoopCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    _args: Vec<String>,
}

impl builtins::Command for NoopCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        _context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        Ok(brush_core::ExecutionResult::new(0))
    }
}

/// Metadata-mode `eend`: passes through its exit-code argument without
/// touching any `ebegin` display state.
#[derive(Parser)]
pub(crate) struct EendStubCommand {
    exit_code: Option<u8>,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    _message: Vec<String>,
}

impl builtins::Command for EendStubCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        _context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        Ok(brush_core::ExecutionResult::new(
            self.exit_code.unwrap_or(0),
        ))
    }
}

/// Metadata-mode `has_version`: always "not installed" — querying a real
/// VDB during a side-effect-free scan isn't meaningful.
#[derive(Parser)]
pub(crate) struct HasVersionStubCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    _args: Vec<String>,
}

impl builtins::Command for HasVersionStubCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        _context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        Ok(brush_core::ExecutionResult::new(1))
    }
}

/// Metadata-mode `best_version`: no output, fails.
#[derive(Parser)]
pub(crate) struct BestVersionStubCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    _args: Vec<String>,
}

impl builtins::Command for BestVersionStubCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        _context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        Ok(brush_core::ExecutionResult::new(1))
    }
}
