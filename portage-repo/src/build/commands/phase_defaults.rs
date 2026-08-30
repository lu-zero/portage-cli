//! PMS 9.1 default phase implementations, as Rust builtins
//!
//! Each `__eapiN_<phase>` name here is what `phase_funcs::resolve_phase_default`
//! wires `default_<phase>()` to for a given EAPI — see that module's
//! `EbuildPhaseFuncsCommand`. Implemented from the PMS 9/12 specification text
//! (listings 9.1-9.9, algorithm 12.3/12.4), not from any real package manager's
//! own source.

use std::io::Write as _;
use std::path::Path;

use brush_core::ShellValue;
use brush_core::builtins;
use clap::Parser;

use super::die::DieFlag;

/// The shape of a variable as the DOCS/HTML_DOCS/PATCHES default-phase logic
/// needs to distinguish it: a non-empty array, a non-empty scalar, or unset
/// (which also covers a declared-but-empty scalar, per PMS's own algorithms).
///
/// Borrows straight out of the shell's own storage — `base_var()`'s
/// reference is tied to the shell's lifetime, not the temporary lookup — so
/// this never clones array/string contents just to inspect their shape.
/// Ignores nameref subscripts (`base_var`, not `resolved_value`): DOCS/
/// HTML_DOCS/PATCHES/PIPESTATUS are never accessed through one in practice.
enum VarShape<'a> {
    Array(Vec<&'a str>),
    Scalar(&'a str),
    Empty,
}

fn var_shape<'a, SE: brush_core::ShellExtensions>(
    shell: &'a brush_core::Shell<SE>,
    name: &str,
) -> VarShape<'a> {
    let Some(r) = shell.env().get(name) else {
        return VarShape::Empty;
    };
    match r.base_var().value() {
        ShellValue::IndexedArray(map) if !map.is_empty() => {
            VarShape::Array(map.values().map(String::as_str).collect())
        }
        ShellValue::String(s) if !s.is_empty() => VarShape::Scalar(s.as_str()),
        _ => VarShape::Empty,
    }
}

/// Single-quote `s` for a literal bash argument (same convention as
/// `eapply`'s private helper of the same name).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn quoted_args<I: IntoIterator<Item = S>, S: AsRef<str>>(args: I) -> String {
    args.into_iter()
        .map(|a| shell_quote(a.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Unconditional die: matches what the real `die` builtin (no `-n`) does —
/// always raises the shared flag and writes `die: <msg>`, regardless of
/// `PORTAGE_NONFATAL` (that's `die -n`'s job, not a bare `die`'s).
///
/// Takes the die flag and params directly (not `&ExecutionContext`) because
/// callers need to move `context.shell` out first, which would otherwise
/// make `context` itself unusable as a whole.
fn die_now<SE: brush_core::ShellExtensions>(
    params: &brush_core::ExecutionParameters,
    die_flag: Option<&DieFlag>,
    shell: &mut brush_core::Shell<SE>,
    msg: &str,
) {
    if let Some(flag) = die_flag {
        flag.raise(msg);
    }
    let _ = writeln!(params.stderr(shell), "die: {msg}");
}

async fn has_makefile<SE: brush_core::ShellExtensions>(shell: &brush_core::Shell<SE>) -> bool {
    let cwd = shell.working_dir();
    for name in ["Makefile", "GNUmakefile", "makefile"] {
        if cwd.join(name).is_file() {
            return true;
        }
    }
    false
}

/// `params` must be the caller's own `context.params`, not a fresh
/// `default_exec_params()` — this runs `emake`/`make`'s actual build output,
/// which must land wherever the current phase invocation is redirected to
/// (console tee, or `--jobs N`/`-q`'s log-only capture), not always the real
/// console regardless of that.
async fn run_emake<SE: brush_core::ShellExtensions>(
    params: &brush_core::ExecutionParameters,
    shell: &mut brush_core::Shell<SE>,
    args: &[&str],
) -> Result<bool, brush_core::Error> {
    let script = format!("emake {}", quoted_args(args));
    let source_info = brush_core::SourceInfo::from("__eapi_default");
    let result = shell.run_string(script, &source_info, params).await?;
    Ok(result.exit_code.is_success())
}

/// See [`run_emake`]'s doc comment on `params` — `econf`'s own "checking
/// for ..." output is subject to the same rule.
async fn run_econf_if_configure_at<SE: brush_core::ShellExtensions>(
    params: &brush_core::ExecutionParameters,
    shell: &mut brush_core::Shell<SE>,
    configure_dir: &str,
) -> Result<(), brush_core::Error> {
    let cwd = shell.working_dir();
    if !cwd.join(configure_dir).join("configure").is_file() {
        return Ok(());
    }
    let source_info = brush_core::SourceInfo::from("__eapi_default");
    shell.run_string("econf", &source_info, params).await?;
    Ok(())
}

/// PMS 9.1.16 / Table 9.10: pkg_nofetch's default for EAPI &ge; 2
#[derive(Parser)]
pub(crate) struct EapiPkgNofetchCommand;

impl builtins::Command for EapiPkgNofetchCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        // One owned command line per file: each becomes its own `run_string`
        // call below, so none can just borrow from `A`'s Cow — it needs to
        // outlive the earlier calls' `&mut shell` borrows in the loop.
        let lines: Vec<String> = shell
            .env_str("A")
            .map(|a| {
                a.split_whitespace()
                    .map(|f| format!("einfo '  '{}", shell_quote(f)))
                    .collect()
            })
            .unwrap_or_default();
        if lines.is_empty() {
            return Ok(brush_core::ExecutionResult::success());
        }
        let source_info = brush_core::SourceInfo::from("__eapi0_pkg_nofetch");
        shell
            .run_string(
                "einfo 'Please download the following and place them in your DISTDIR:'",
                &source_info,
                &context.params,
            )
            .await?;
        for line in lines {
            shell
                .run_string(line, &source_info, &context.params)
                .await?;
        }
        Ok(brush_core::ExecutionResult::success())
    }
}

/// PMS Listing 9.1: src_unpack's default for every EAPI
#[derive(Parser)]
pub(crate) struct EapiSrcUnpackCommand;

impl builtins::Command for EapiSrcUnpackCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let a = shell.env_str("A").unwrap_or_default();
        if a.split_whitespace().next().is_none() {
            return Ok(brush_core::ExecutionResult::success());
        }
        let script = format!("unpack {}", quoted_args(a.split_whitespace()));
        let source_info = brush_core::SourceInfo::from("__eapi0_src_unpack");
        shell
            .run_string(script, &source_info, &context.params)
            .await?;
        Ok(brush_core::ExecutionResult::success())
    }
}

/// Shared body for src_compile's default across every EAPI/format that ends
/// in "run emake if a Makefile exists" (PMS Listings 9.5, 9.6, 9.7) — only
/// the configure-detection step (if any) differs between formats.
async fn default_src_compile<SE: brush_core::ShellExtensions>(
    params: &brush_core::ExecutionParameters,
    die_flag: Option<&DieFlag>,
    shell: &mut brush_core::Shell<SE>,
    configure_dir: Option<&str>,
) -> Result<brush_core::ExecutionResult, brush_core::Error> {
    if let Some(dir) = configure_dir {
        run_econf_if_configure_at(params, shell, dir).await?;
    }
    if has_makefile(shell).await && !run_emake(params, shell, &[]).await? {
        die_now(params, die_flag, shell, "emake failed");
        return Ok(brush_core::ExecutionResult::new(1));
    }
    Ok(brush_core::ExecutionResult::success())
}

/// PMS Listing 9.5: src_compile, format 0 (EAPI 0)
#[derive(Parser)]
pub(crate) struct EapiSrcCompile0Command;

impl builtins::Command for EapiSrcCompile0Command {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let die_flag = context.shared::<DieFlag>().ok().cloned();
        let shell = context.shell;
        default_src_compile(&context.params, die_flag.as_ref(), shell, Some(".")).await
    }
}

/// PMS Listing 9.6: src_compile, format 1 (EAPI 1) — same as format 0, but
/// the configure check honours `ECONF_SOURCE`
#[derive(Parser)]
pub(crate) struct EapiSrcCompile1Command;

impl builtins::Command for EapiSrcCompile1Command {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let die_flag = context.shared::<DieFlag>().ok().cloned();
        let shell = context.shell;
        let econf_source = shell
            .env_str("ECONF_SOURCE")
            .map(|s| s.into_owned())
            .unwrap_or_default();
        let dir = if econf_source.is_empty() {
            ".".to_string()
        } else {
            econf_source
        };
        default_src_compile(&context.params, die_flag.as_ref(), shell, Some(&dir)).await
    }
}

/// PMS Listing 9.7: src_compile, format 2 (EAPI 2+) — configure is its own
/// phase by now, so the default here is just the emake step
#[derive(Parser)]
pub(crate) struct EapiSrcCompile2Command;

impl builtins::Command for EapiSrcCompile2Command {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let die_flag = context.shared::<DieFlag>().ok().cloned();
        let shell = context.shell;
        default_src_compile(&context.params, die_flag.as_ref(), shell, None).await
    }
}

/// PMS 9.1.6 Listing 9.4: src_configure's default for EAPI 2+
#[derive(Parser)]
pub(crate) struct EapiSrcConfigureCommand;

impl builtins::Command for EapiSrcConfigureCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let econf_source = shell
            .env_str("ECONF_SOURCE")
            .map(|s| s.into_owned())
            .unwrap_or_default();
        let dir = if econf_source.is_empty() {
            ".".to_string()
        } else {
            econf_source
        };
        run_econf_if_configure_at(&context.params, shell, &dir).await?;
        Ok(brush_core::ExecutionResult::success())
    }
}

/// PMS 9.1.5 Table 9.4: src_prepare's default is a no-op for EAPI 2-5
#[derive(Parser)]
pub(crate) struct EapiSrcPrepareNoopCommand;

impl builtins::Command for EapiSrcPrepareNoopCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        _context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        Ok(brush_core::ExecutionResult::success())
    }
}

/// PMS 9.1.9 Listing 9.8: src_install, format 4 (EAPI 4-5)
#[derive(Parser)]
pub(crate) struct EapiSrcInstall4Command;

/// PMS's fixed fallback filename list (Listing 9.8, Algorithm 12.3) used
/// when DOCS is neither a non-empty array nor a non-empty scalar.
const DEFAULT_DOC_FILES: &[&str] = &[
    "README*",
    "ChangeLog",
    "AUTHORS",
    "NEWS",
    "TODO",
    "CHANGES",
    "THANKS",
    "BUGS",
    "FAQ",
    "CREDITS",
    "CHANGELOG",
];

async fn install_docs_var<SE: brush_core::ShellExtensions>(
    params: &brush_core::ExecutionParameters,
    shell: &mut brush_core::Shell<SE>,
    var: &str,
    extra_dodoc_args: &str,
) -> Result<(), brush_core::Error> {
    // Built before the match ends, so the borrow from `var_shape` (tied to
    // `&*shell`) is done by the time `run_string` needs `&mut shell`.
    let script = match var_shape(shell, var) {
        VarShape::Array(files) => Some(format!(
            "dodoc {extra_dodoc_args} -- {}",
            quoted_args(&files)
        )),
        // Unquoted, matching real bash `dodoc -r ${DOCS}`: a scalar DOCS may
        // itself contain a glob pattern (e.g. dev-build/make's own
        // `DOCS="AUTHORS NEWS README*"`), which must still expand against
        // the current directory here, not be treated as a literal filename.
        VarShape::Scalar(files) => Some(format!("dodoc {extra_dodoc_args} -- {files}")),
        VarShape::Empty => None,
    };
    if let Some(script) = script {
        let source_info = brush_core::SourceInfo::from("__eapi_default");
        shell.run_string(script, &source_info, params).await?;
    }
    Ok(())
}

impl builtins::Command for EapiSrcInstall4Command {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let die_flag = context.shared::<DieFlag>().ok().cloned();
        let shell = context.shell;
        if has_makefile(shell).await
            && !run_emake(&context.params, shell, &["DESTDIR=${D}", "install"]).await?
        {
            die_now(
                &context.params,
                die_flag.as_ref(),
                shell,
                "emake install failed",
            );
            return Ok(brush_core::ExecutionResult::new(1));
        }
        let declared = shell.env().get("DOCS").is_some();
        if declared {
            install_docs_var(&context.params, shell, "DOCS", "").await?;
        } else {
            let cwd = shell.working_dir().to_path_buf();
            let source_info = brush_core::SourceInfo::from("__eapi4_src_install");
            for pattern in DEFAULT_DOC_FILES {
                for candidate in glob_in(&cwd, pattern) {
                    if candidate.is_file()
                        && candidate.metadata().map(|m| m.len()).unwrap_or(0) > 0
                        && let Some(name) = candidate.file_name().and_then(|n| n.to_str())
                    {
                        let script = format!("dodoc -- {}", shell_quote(name));
                        shell
                            .run_string(script, &source_info, &context.params)
                            .await?;
                    }
                }
            }
        }
        Ok(brush_core::ExecutionResult::success())
    }
}

/// Non-recursive single-component glob (only `*`/`?`/`[...]` on a bare
/// filename, as PMS's `README*`-style fallback lists need) under `dir`.
fn glob_in(dir: &Path, pattern: &str) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            glob_match(pattern, name).then(|| e.path())
        })
        .collect()
}

/// Minimal `fnmatch`-style matcher: only `*` (used by `README*`) needs to
/// work for the fallback filename lists these defaults check.
fn glob_match(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        Some((prefix, suffix)) => name.starts_with(prefix) && name.ends_with(suffix),
        None => pattern == name,
    }
}

/// PMS Algorithm 12.4: `get_libdir`
#[derive(Parser)]
pub(crate) struct GetLibdirCommand;

impl builtins::Command for GetLibdirCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let mut libdir = "lib".to_string();
        let abi = shell
            .env_str("ABI")
            .map(|s| s.into_owned())
            .unwrap_or_default();
        if !abi.is_empty() {
            let libvar = format!("LIBDIR_{abi}");
            if let Some(v) = shell.env_str(&libvar)
                && !v.is_empty()
            {
                libdir = v.into_owned();
            }
        } else if let Some(v) = shell.env_str("CONF_LIBDIR")
            && !v.is_empty()
        {
            libdir = v.into_owned();
        }
        let _ = writeln!(context.params.stdout(shell), "{libdir}");
        Ok(brush_core::ExecutionResult::success())
    }
}

/// PMS 9.1.9 Listing 9.9: src_install, format 6 (EAPI 6+)
#[derive(Parser)]
pub(crate) struct EapiSrcInstall6Command;

impl builtins::Command for EapiSrcInstall6Command {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let die_flag = context.shared::<DieFlag>().ok().cloned();
        let shell = context.shell;
        if has_makefile(shell).await
            && !run_emake(&context.params, shell, &["DESTDIR=${D}", "install"]).await?
        {
            die_now(
                &context.params,
                die_flag.as_ref(),
                shell,
                "emake install failed",
            );
            return Ok(brush_core::ExecutionResult::new(1));
        }
        let source_info = brush_core::SourceInfo::from("__eapi6_src_install");
        shell
            .run_string("einstalldocs", &source_info, &context.params)
            .await?;
        Ok(brush_core::ExecutionResult::success())
    }
}

/// PMS 12.3 Algorithm 12.3: `einstalldocs`
#[derive(Parser)]
pub(crate) struct EinstalldocsCommand;

impl builtins::Command for EinstalldocsCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let saved_docdesttree = shell
            .env_str("DOCDESTTREE")
            .unwrap_or_default()
            .into_owned();
        let source_info = brush_core::SourceInfo::from("einstalldocs");

        shell
            .run_string("docinto ''", &source_info, &context.params)
            .await?;
        // PMS Algorithm 12.4: the README*-style fallback list only applies
        // when DOCS is unset. A declared-but-empty DOCS (`DOCS=""`,
        // `DOCS=()`) installs nothing — `install_docs_var`'s own
        // `var_shape` check already no-ops for that case, same as
        // `EapiSrcInstall4Command`'s identical `declared` gate.
        if shell.env().get("DOCS").is_some() {
            install_docs_var(&context.params, shell, "DOCS", "-r").await?;
        } else {
            let cwd = shell.working_dir().to_path_buf();
            for pattern in DEFAULT_DOC_FILES {
                for candidate in glob_in(&cwd, pattern) {
                    if candidate.is_file()
                        && candidate.metadata().map(|m| m.len()).unwrap_or(0) > 0
                        && let Some(name) = candidate.file_name().and_then(|n| n.to_str())
                    {
                        let script = format!("dodoc -- {}", shell_quote(name));
                        shell
                            .run_string(script, &source_info, &context.params)
                            .await?;
                    }
                }
            }
        }

        shell
            .run_string("docinto html", &source_info, &context.params)
            .await?;
        install_docs_var(&context.params, shell, "HTML_DOCS", "-r").await?;

        let restore = format!("docinto {}", shell_quote(&saved_docdesttree));
        shell
            .run_string(restore, &source_info, &context.params)
            .await?;

        Ok(brush_core::ExecutionResult::success())
    }
}

/// PMS 9.1.8 / Table 9.7: src_test's default, every EAPI
#[derive(Parser)]
pub(crate) struct EapiSrcTestCommand;

impl builtins::Command for EapiSrcTestCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let die_flag = context.shared::<DieFlag>().ok().cloned();
        let shell = context.shell;
        let eapi: u32 = shell
            .env_str("EAPI")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        // PMS Table 9.7: parallel tests unsupported through EAPI 4.
        let jobs: &[&str] = if eapi <= 4 { &["-j1"] } else { &[] };

        let source_info = brush_core::SourceInfo::from("__eapi0_src_test");
        for target in ["check", "test"] {
            let probe = format!("emake -n {target} &>/dev/null");
            let available = shell
                .run_string(probe, &source_info, &context.params)
                .await?
                .exit_code
                .is_success();
            if !available {
                continue;
            }
            let mut args = jobs.to_vec();
            args.push(target);
            if !run_emake(&context.params, shell, &args).await? {
                die_now(
                    &context.params,
                    die_flag.as_ref(),
                    shell,
                    &format!("{target} target failed"),
                );
                return Ok(brush_core::ExecutionResult::new(1));
            }
            break;
        }
        Ok(brush_core::ExecutionResult::success())
    }
}

/// PMS Listing 9.2: src_prepare, format 6 (EAPI 6-7)
#[derive(Parser)]
pub(crate) struct EapiSrcPrepare6Command;

/// See [`run_emake`]'s doc comment on `params` — `eapply`'s own "Applying
/// patches from …" status line is subject to the same rule (and is the
/// concrete incident this whole family of fixes exists for: `sys-kernel/
/// linux-headers`' `src_prepare` sets `PATCHES` then calls `default`, which
/// reaches `eapply` through here, not through a directly-invoked `eapply`
/// call an ebuild's own `src_prepare` would make).
async fn apply_patches_var<SE: brush_core::ShellExtensions>(
    params: &brush_core::ExecutionParameters,
    shell: &mut brush_core::Shell<SE>,
    dashdash: bool,
) -> Result<(), brush_core::Error> {
    let sep = if dashdash { "-- " } else { "" };
    let script = match var_shape(shell, "PATCHES") {
        VarShape::Array(files) => Some(format!("eapply {sep}{}", quoted_args(&files))),
        // Unquoted, matching real bash `eapply ${PATCHES}`: a scalar may
        // itself contain a glob pattern, which must expand here.
        VarShape::Scalar(files) => Some(format!("eapply {sep}{files}")),
        VarShape::Empty => None,
    };
    let source_info = brush_core::SourceInfo::from("__eapi_default");
    if let Some(script) = script {
        shell.run_string(script, &source_info, params).await?;
    }
    shell
        .run_string("eapply_user", &source_info, params)
        .await?;
    Ok(())
}

impl builtins::Command for EapiSrcPrepare6Command {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        apply_patches_var(&context.params, context.shell, false).await?;
        Ok(brush_core::ExecutionResult::success())
    }
}

/// PMS Listing 9.3: src_prepare, format 8 (EAPI 8+) — `eapply --`
#[derive(Parser)]
pub(crate) struct EapiSrcPrepare8Command;

impl builtins::Command for EapiSrcPrepare8Command {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        apply_patches_var(&context.params, context.shell, true).await?;
        Ok(brush_core::ExecutionResult::success())
    }
}

/// `eapply_user` — no user patch directory support (yet); a no-op is a
/// conforming implementation (PMS only requires it be safe to call).
#[derive(Parser)]
pub(crate) struct EapplyUserCommand;

impl builtins::Command for EapplyUserCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        _context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        Ok(brush_core::ExecutionResult::success())
    }
}

/// `nonfatal <command> [args...]`  (PMS 12.3.1, EAPI &ge; 4)
///
/// Runs `command` with `PORTAGE_NONFATAL=1` scoped to that one invocation
/// (a bash prefix assignment), so nonfatal-aware builtins (`emake`, `die -n`,
/// ...) can return a failure instead of aborting the build.
#[derive(Parser)]
pub(crate) struct NonfatalCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for NonfatalCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let die_flag = context.shared::<DieFlag>().ok().cloned();
        let shell = context.shell;
        if self.args.is_empty() {
            die_now(
                &context.params,
                die_flag.as_ref(),
                shell,
                "nonfatal: missing argument",
            );
            return Ok(brush_core::ExecutionResult::new(1));
        }
        let script = format!("PORTAGE_NONFATAL=1 {}", quoted_args(&self.args));
        let source_info = brush_core::SourceInfo::from("nonfatal");
        shell
            .run_string(script, &source_info, &context.params)
            .await
    }
}

/// `assert [message...]`  (PMS 12.3.1, EAPI &le; 8)
///
/// Dies if any element of `$PIPESTATUS` from the previous pipeline was
/// non-zero.
#[derive(Parser)]
pub(crate) struct AssertCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for AssertCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let die_flag = context.shared::<DieFlag>().ok().cloned();
        let shell = context.shell;
        // PIPESTATUS is a `Dynamic` value (computed on read), not a real
        // stored array — `var_shape`'s direct-storage borrow never resolves
        // it. `element_values` is the accessor that calls through a
        // Dynamic's getter, so this allocation (unlike DOCS/PATCHES's)
        // isn't avoidable.
        let failed = shell
            .env()
            .get("PIPESTATUS")
            .map(|r| r.base_var().value().element_values(shell))
            .unwrap_or_default()
            .iter()
            .any(|c| c.parse::<i64>() != Ok(0));
        if failed {
            let msg = if self.message.is_empty() {
                "assert: command failed".to_string()
            } else {
                self.message.join(" ")
            };
            die_now(&context.params, die_flag.as_ref(), shell, &msg);
            return Ok(brush_core::ExecutionResult::new(1));
        }
        Ok(brush_core::ExecutionResult::success())
    }
}
