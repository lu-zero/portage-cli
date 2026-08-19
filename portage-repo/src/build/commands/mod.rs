//! Rust builtins for PMS utility functions: eapi predicates, phase setup,
//! output helpers, build helpers, and PMS 12.3 USE/has/ver functions.
//!
//! These reimplement the functionality of portage's `eapi.sh`,
//! `isolated-functions.sh`, `phase-helpers.sh`, and `phase-functions.sh`
//! without sourcing those files.  Rust builtins sidestep brush parser gaps
//! (notably bare `[[ ]]` function bodies used as EAPI predicate bodies).
//!
//! See [PMS 12.3](https://projects.gentoo.org/pms/9/pms.html#available-commands).

pub(crate) mod die;
pub(crate) mod eapply;
pub(crate) mod econf;
pub(crate) mod einstall;
pub(crate) mod emake;
pub(crate) mod export_functions;
pub(crate) mod has;
pub mod inherit;
pub(crate) mod inst_owner;
pub(crate) mod install;
pub(crate) mod install_paths;
pub(crate) mod output;
pub(crate) mod phase_funcs;
pub(crate) mod unpack;
pub(crate) mod use_flag;
pub(crate) mod version_query;

pub(crate) use die::DieCommand;
pub(crate) use eapply::EapplyCommand;
pub(crate) use econf::EconfCommand;
pub(crate) use einstall::EinstallCommand;
pub(crate) use emake::EmakeCommand;
pub(crate) use export_functions::ExportFunctionsCommand;
pub(crate) use has::{HasCommand, HasvCommand, InIuseCommand};
pub(crate) use install::{HELPER_NAMES, register_install_builtins};
pub(crate) use install_paths::{DocompressCommand, DostripCommand};
pub(crate) use output::{EbeginCommand, EchoMessageCommand, EendCommand};
pub(crate) use phase_funcs::{EAPI_PREDICATE_NAMES, EapiPredicateCommand, EbuildPhaseFuncsCommand};
pub(crate) use unpack::UnpackCommand;
pub(crate) use use_flag::{UseCommand, UseEnableCommand, UseWithCommand, UsevCommand, UsexCommand};

/// `(stdout, stderr)` Stdio handles honouring the shell context's current
/// redirections
///
/// A Rust builtin's spawned children otherwise inherit the host process
/// stdio and escape e.g. `src_compile > build.log 2>&1`.
pub(crate) fn context_stdio<SE: brush_core::ShellExtensions>(
    context: &brush_core::ExecutionContext<'_, SE>,
) -> (std::process::Stdio, std::process::Stdio) {
    use brush_core::openfiles::{OpenFile, OpenFiles};
    let stdout = match context.try_fd(OpenFiles::STDOUT_FD) {
        Some(OpenFile::Stdout(_)) | None => std::process::Stdio::inherit(),
        // brush dup's the descriptor (handles are Arc-shared, so not movable);
        // a dup failure falls back to inheriting the host fd.
        Some(f) => f
            .try_into()
            .unwrap_or_else(|_| std::process::Stdio::inherit()),
    };
    let stderr = match context.try_fd(OpenFiles::STDERR_FD) {
        Some(OpenFile::Stderr(_)) | None => std::process::Stdio::inherit(),
        Some(f) => f
            .try_into()
            .unwrap_or_else(|_| std::process::Stdio::inherit()),
    };
    (stdout, stderr)
}

/// Stdio handle for a builtin's spawned child stdin, honouring the shell
/// context's redirections
///
/// Needed for `emake -f -` (read makefile from a pipe): without
/// forwarding the pipeline's read end, the child `make` sees the host
/// stdin and reads no makefile (`No targets. Stop.`). A non-redirected
/// stdin (the real terminal) is inherited.
pub(crate) fn context_stdin<SE: brush_core::ShellExtensions>(
    context: &brush_core::ExecutionContext<'_, SE>,
) -> std::process::Stdio {
    use brush_core::openfiles::{OpenFile, OpenFiles};
    match context.try_fd(OpenFiles::STDIN_FD) {
        Some(OpenFile::Stdin(_)) | None => std::process::Stdio::inherit(),
        Some(f) => f
            .try_into()
            .unwrap_or_else(|_| std::process::Stdio::inherit()),
    }
}

/// The shell's exported environment as `(name, value)` pairs
///
/// A Rust builtin's spawned child (`configure`, `make`, …) otherwise
/// inherits only em's host process environment, missing the build env the
/// shell carries: make.conf flags, USE-driven vars, and — crucially for
/// `--prefix` overlay builds — the `PKG_CONFIG_*`/`CPPFLAGS`/`LDFLAGS` a
/// `bashrc` hook exports to expose already-merged deps. Mirrors how brush
/// builds the env for external commands.
pub(crate) fn context_env<SE: brush_core::ShellExtensions>(
    context: &brush_core::ExecutionContext<'_, SE>,
) -> Vec<(String, String)> {
    context
        .shell
        .env()
        .iter_exported()
        .map(|(k, v)| (k.clone(), v.value().to_cow_str(context.shell).into_owned()))
        .collect()
}
