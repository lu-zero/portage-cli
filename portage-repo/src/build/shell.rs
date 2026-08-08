use std::collections::HashSet;
use std::path::{Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};

use brush_builtins::ShellExt;
use brush_core::parser::ParserImpl;
use brush_core::{
    ProfileLoadBehavior, RcLoadBehavior, Shell, ShellValue, ShellVariable, SourceInfo,
};
use portage_metadata::{Eapi, EbuildMetadata, Phase, SrcUriEntry};

use super::commands;
use super::commands::inherit;
use super::pty;
use super::stubs;
use super::terminal;
use super::ver_funcs;
use crate::error::{Error, Result};
use crate::repo::ebuild::Ebuild;
use crate::repo::repository::Repository;
use crate::source::SourceContext;

/// Metadata variables extracted from a sourced ebuild.
///
/// These correspond to the PMS-defined metadata variables that an ebuild
/// is expected to define after being sourced.
const METADATA_VARS: &[&str] = &[
    "DESCRIPTION",
    "HOMEPAGE",
    "SRC_URI",
    "LICENSE",
    "SLOT",
    "KEYWORDS",
    "IUSE",
    "REQUIRED_USE",
    "RESTRICT",
    "PROPERTIES",
    "DEPEND",
    "RDEPEND",
    "BDEPEND",
    "PDEPEND",
    "IDEPEND",
    "INHERIT",
    "INHERITED",
];

/// Maps a portage phase name to (EBUILD_PHASE value, function name).
fn phase_to_func(phase: &str) -> (&str, &str) {
    match phase {
        "pretend" => ("pretend", "pkg_pretend"),
        "setup" => ("setup", "pkg_setup"),
        "unpack" => ("unpack", "src_unpack"),
        "prepare" => ("prepare", "src_prepare"),
        "configure" => ("configure", "src_configure"),
        "compile" => ("compile", "src_compile"),
        "test" => ("test", "src_test"),
        "install" => ("install", "src_install"),
        "preinst" => ("preinst", "pkg_preinst"),
        "postinst" => ("postinst", "pkg_postinst"),
        "prerm" => ("prerm", "pkg_prerm"),
        "postrm" => ("postrm", "pkg_postrm"),
        "nofetch" => ("nofetch", "pkg_nofetch"),
        "info" => ("info", "pkg_info"),
        "config" => ("config", "pkg_config"),
        // accept raw function names too
        other => (other, other),
    }
}

/// PMS phase function names mapped to their [`Phase`] variants.
///
/// Used to compute `DEFINED_PHASES` by inspecting which functions are
/// defined in the shell after sourcing an ebuild.
///
/// See [PMS 7.4](https://projects.gentoo.org/pms/9/pms.html#defined-phases).
const PHASE_FUNCTIONS: &[(&str, Phase)] = &[
    ("pkg_pretend", Phase::PkgPretend),
    ("pkg_setup", Phase::PkgSetup),
    ("src_unpack", Phase::SrcUnpack),
    ("src_prepare", Phase::SrcPrepare),
    ("src_configure", Phase::SrcConfigure),
    ("src_compile", Phase::SrcCompile),
    ("src_test", Phase::SrcTest),
    ("src_install", Phase::SrcInstall),
    ("pkg_preinst", Phase::PkgPreinst),
    ("pkg_postinst", Phase::PkgPostinst),
    ("pkg_prerm", Phase::PkgPrerm),
    ("pkg_postrm", Phase::PkgPostrm),
    ("pkg_config", Phase::PkgConfig),
    ("pkg_info", Phase::PkgInfo),
    ("pkg_nofetch", Phase::PkgNofetch),
];

/// Per-EAPI default phase implementations loaded by `init_build_env`.
///
/// These bash functions are called by `__ebuild_phase_funcs` (a Rust builtin)
/// when wiring up `default()` and `default_<phase>()`.  The functions call
/// `econf` / `emake` / `eapply` which are Rust builtins.
const PHASE_DEFAULT_FUNCTIONS: &str = r#"
__eapi0_pkg_nofetch() {
    [[ -z ${A} ]] && return
    elog "The following files cannot be fetched for ${PN}:"
    local x
    for x in ${A}; do elog "   ${x}"; done
}
__eapi0_src_unpack() { [[ -n ${A} ]] && unpack ${A}; }
__eapi0_src_compile() {
    if [[ -x ./configure ]]; then econf; fi
    __eapi2_src_compile
}
__eapi0_src_test() {
    # PMS: default src_test forces -j1 for EAPI ≤ 4 to avoid parallel test races.
    local jobflag=""
    ___eapi_default_src_test_disables_parallel_jobs && jobflag="-j1"
    local emake_cmd="${MAKE:-make} ${MAKEOPTS} ${EXTRA_EMAKE}${jobflag:+ ${jobflag}}"
    if ${emake_cmd} check -n &>/dev/null; then
        ${emake_cmd} check || die "check target failed"
    elif ${emake_cmd} test -n &>/dev/null; then
        ${emake_cmd} test || die "test target failed"
    fi
}
__eapi1_src_compile() { __eapi2_src_configure; __eapi2_src_compile; }
__eapi2_src_prepare() { :; }
__eapi2_src_configure() {
    if [[ -x ${ECONF_SOURCE:-.}/configure ]]; then econf; fi
}
__eapi2_src_compile() {
    if [[ -f Makefile || -f GNUmakefile || -f makefile ]]; then
        emake || die "emake failed"
    fi
}
__eapi4_src_install() {
    if [[ -f Makefile || -f GNUmakefile || -f makefile ]]; then
        emake DESTDIR="${D}" install || die "emake install failed"
    fi
    if [[ -v DOCS ]]; then
        if [[ ${DOCS@a} == *a* ]]; then
            [[ ${#DOCS[@]} -gt 0 ]] && dodoc "${DOCS[@]}"
        elif [[ -n ${DOCS} ]]; then
            dodoc ${DOCS}
        fi
    fi
}
__eapi6_src_prepare() {
    if [[ -n ${PATCHES+set} ]]; then
        if [[ ${PATCHES@a} == *a* ]]; then
            [[ ${#PATCHES[@]} -gt 0 ]] && eapply "${PATCHES[@]}"
        elif [[ -n ${PATCHES} ]]; then
            eapply ${PATCHES}
        fi
    fi
    eapply_user
}
__eapi6_src_install() {
    if [[ -f Makefile || -f GNUmakefile || -f makefile ]]; then
        emake DESTDIR="${D}" install || die "emake install failed"
    fi
    einstalldocs
}
__eapi8_src_prepare() {
    if [[ -n ${PATCHES+set} ]]; then
        if [[ ${PATCHES@a} == *a* ]]; then
            [[ ${#PATCHES[@]} -gt 0 ]] && eapply -- "${PATCHES[@]}"
        elif [[ -n ${PATCHES} ]]; then
            eapply -- ${PATCHES}
        fi
    fi
    eapply_user
}
nonfatal() { PORTAGE_NONFATAL=1 "$@"; local _r=$?; unset PORTAGE_NONFATAL; return $_r; }
assert() {
    local pipestatus=("${PIPESTATUS[@]}")
    local x
    for x in "${pipestatus[@]}"; do
        (( x == 0 )) && continue
        [[ $# -gt 0 ]] && die "$@" || die "assert: command failed"
    done
}
__eapply_patch() {
    local patch=$1 output
    shift
    # -p1 sane default, -f non-interactive, -g0 no VCS, no stray .orig backups.
    if output=$(LC_ALL= LC_MESSAGES=C patch -p1 -f -g0 --no-backup-if-mismatch "$@" < "${patch}" 2>&1); then
        # Quiet on a clean apply; surface fuzz so it isn't silently absorbed.
        # Must end with a success status: the caller uses `|| return`, so a
        # falsy `[[ ]]` test here would abort the patch loop after one file.
        [[ ${output} == *[0-9]" with fuzz "[0-9]* ]] && printf '%s\n' "${output}"
        return 0
    else
        printf '%s\n' "${output}" >&2
        die "eapply: patch failed: ${patch}"
    fi
}
eapply() {
    # PMS 11.3.3: a `--` separates patch options (left) from operands (right);
    # absent `--`, options must precede operands. A directory operand applies
    # its direct *.diff/*.patch children in C-collation order.
    local LC_ALL LC_COLLATE=C f path i
    local -a operands=() options=()
    while (( $# )); do
        [[ $1 == -- ]] && break
        options+=("$1")
        shift
    done
    if (( $# )); then
        shift
        operands=("$@")
    else
        set -- "${options[@]}"
        options=()
        while (( $# )); do
            if [[ $1 == -* ]]; then
                (( ${#operands[@]} )) && die "eapply: options must precede non-option arguments"
                options+=("$1")
            else
                operands+=("$1")
            fi
            shift
        done
    fi
    (( ${#operands[@]} )) || die "eapply: no operands were specified"
    for path in "${operands[@]}"; do
        if [[ -d ${path} ]]; then
            i=0
            for f in "${path}"/*; do
                [[ ${f} == *.diff || ${f} == *.patch ]] || continue
                (( i++ == 0 )) && einfo "Applying patches from ${path} ..."
                __eapply_patch "${f}" "${options[@]}" || return
            done
            (( i == 0 )) && die "No *.{patch,diff} files in directory ${path}"
        else
            __eapply_patch "${path}" "${options[@]}" || return
        fi
    done
    # Success: callers may use `eapply … || die`, so don't leak a non-zero
    # status from the final arithmetic/test above.
    return 0
}
eapply_user() { :; }
get_libdir() {
    local libdir_var="LIBDIR_${ABI}"
    [[ -n ${ABI} && -n ${!libdir_var} ]] && echo "${!libdir_var}" || echo "lib"
}
"#;

/// Sandbox path-registration functions as no-ops (real enforcement is M3).
/// Defined so eclasses/ebuilds that call them don't abort with "command not
/// found"; each portage equivalent only appends to a `SANDBOX_*` colon list.
const SANDBOX_STUBS: &str = r#"
addread()    { :; }
addwrite()   { :; }
addpredict() { :; }
adddeny()    { :; }
"#;

/// P3 install helpers loaded by `init_build_env` (PMS §12.3.x).
///
/// These bash functions replace the no-op stubs from `builtins.rs` during
/// build phases.  They install files into `${ED}` (= `${D}${EPREFIX}`) using the standard `install`
/// utility and track destination-directory state in shell variables.
const INSTALL_HELPERS: &str = r#"
# Destination-directory state — reset to defaults by this sourcing.
# _into_dir mirrors portage's DESTTREE (set both so eclasses reading either win).
_into_dir=/usr
DESTTREE=/usr
INSDESTTREE=
EXEDESTTREE=
DOCDESTTREE=
_insopts="-m0644"
_exeopts="-m0755"

into()    { _into_dir="$1"; DESTTREE="$1"; }
insinto() { INSDESTTREE="$1"; }
exeinto() { EXEDESTTREE="$1"; }
docinto() { DOCDESTTREE="$1"; }
insopts() { _insopts="$*"; }
exeopts() { _exeopts="$*"; }

# The do* install helpers (dodir keepdir doins doexe dobin dosbin dodoc doheader
# doinfo doman domo dolib dolib.a dolib.so dosym fperms fowners) and the new*
# helpers (newbin newsbin newins newexe newdoc newman newheader newlib.a
# newlib.so newinitd newconfd newenvd) are Rust builtins — see
# commands/install.rs.  They are registered on the shell and the metadata stubs
# unset in init_build_env so the builtins win.  new* reads stdin when its source
# arg is `-`.  The pure destination-state setters and the doinitd/doconfd/doenvd
# do* wrappers (which set insinto then call doins) stay in bash for now.

doinitd() {
    [[ $# -gt 0 ]] || die "doinitd: at least one argument required"
    insinto /etc/init.d
    insopts -m0755
    doins "$@"
    insopts -m0644
}

doconfd() {
    [[ $# -gt 0 ]] || die "doconfd: at least one argument required"
    insinto /etc/conf.d
    doins "$@"
}

doenvd() {
    [[ $# -gt 0 ]] || die "doenvd: at least one argument required"
    insinto /etc/env.d
    doins "$@"
}

edo() {
    einfo "$@"
    "$@" || die "edo: command failed: $*"
}

get_libdir() {
    local v=lib
    if [[ -n ${ABI} ]]; then
        local var="LIBDIR_${ABI}"
        [[ -n ${!var} ]] && v=${!var}
    elif [[ -n ${CONF_LIBDIR} ]]; then
        v=${CONF_LIBDIR}
    fi
    echo "${v}"
}

einstalldocs() {
    local f
    if [[ -v DOCS ]]; then
        if [[ ${DOCS@a} == *a* ]]; then
            [[ ${#DOCS[@]} -gt 0 ]] && dodoc -r "${DOCS[@]}"
        elif [[ -n ${DOCS} ]]; then
            dodoc -r ${DOCS}
        fi
    else
        for f in README* CHANGES* ChangeLog* CHANGELOG* AUTHORS* NEWS* TODO* THANKS*; do
            [[ -s ${f} ]] && dodoc "${f}"
        done
    fi
    if [[ -v HTML_DOCS ]]; then
        if [[ ${HTML_DOCS@a} == *a* ]]; then
            [[ ${#HTML_DOCS[@]} -gt 0 ]] && dodoc -r "${HTML_DOCS[@]}"
        elif [[ -n ${HTML_DOCS} ]]; then
            dodoc -r ${HTML_DOCS}
        fi
    fi
}
"#;

/// An embedded bash shell for sourcing ebuilds, eclasses, and `make.defaults`.
///
/// The PMS name/version strings a caller reuses after the vars are set.
struct PmsNames {
    p: String,
    pf: String,
}

/// Wraps [`brush_core::Shell`] configured for Gentoo ebuild evaluation.
/// The shell has standard bash builtins registered and eclass directories
/// set up for the repository.
///
/// See [PMS 7](https://projects.gentoo.org/pms/9/pms.html#ebuilddefined-variables)
/// for the metadata variables extracted after sourcing an ebuild.
pub struct EbuildShell {
    shell: Shell,
    /// Caller-chosen writable distfiles dir (e.g. `<prefix>/var/cache/distfiles`).
    /// The auto-resolved primary joins the read-only fallbacks so shared
    /// caches keep being found.
    distdir_override: Option<Utf8PathBuf>,
    /// Phase-output log. `Some((path, quiet))`: phase function output is
    /// appended to `path` — tee'd to the console, or captured silently when
    /// `quiet`.
    phase_log: Option<(Utf8PathBuf, bool)>,
    /// Cross-subshell `die` signal: `die` raises it even from `$(...)` or
    /// helper pipelines where its exit status cannot abort the phase; the
    /// phase driver checks it after the phase function returns. Shared (Arc)
    /// with every clone of the inner shell, including the baseline.
    die_flag: commands::die::DieFlag,
    /// Cross-subshell accumulator for the `docompress`/`dostrip` path lists
    /// (PMS 12.3.9/12.3.10), populated during `src_install` and read by the
    /// merge driver's post-install pass. Shared (Arc) like `die_flag`.
    install_paths: commands::install_paths::InstallPaths,
    /// Resolved `PORTAGE_INST_UID`/`PORTAGE_INST_GID` for `dobin`/`dosbin` and
    /// PATH shims. Shared (Arc) like `die_flag`.
    inst_owner: commands::inst_owner::InstOwnerDefaults,
    /// Width and palette the `e*` output builtins render with. Shared (Arc)
    /// like `die_flag`. Set by [`EbuildShell::set_terminal`].
    terminal: terminal::TerminalState,
    /// `PORTAGE_CONFIGROOT` for phases — where profile/make.conf live. `None`
    /// keeps the host. Set by the merge driver from the root model.
    build_config_root: Option<Utf8PathBuf>,
    /// `SYSROOT`/`ESYSROOT` for phases — the base system the build resolves
    /// `DEPEND` against. `None` defaults to `ROOT` (the install target). For an
    /// overlay (`--prefix`) this is the base, with the target layered on top.
    build_sysroot: Option<Utf8PathBuf>,
    /// `EPREFIX` for an in-place prefix build (`--local`): packages are
    /// configured for and installed at this offset, so `ROOT=/`, `EROOT=ROOT+
    /// EPREFIX` (== the merge root), and `ED=D+EPREFIX`. `None` ⇒ `EPREFIX=""`
    /// (host / ROOT-offset `--prefix`).
    build_eprefix: Option<Utf8PathBuf>,
    /// `LD_LIBRARY_PATH` for phases, resolved by the caller (`ebuild.rs`),
    /// not read from disk here. See todo/for-sonnet.md 2026-08-08.
    build_ld_library_path: Option<String>,
    /// Where `BDEPEND`-class build tools (a `${CHOST}-*` cross toolchain, its
    /// `pkg-config`, …) actually live for this invocation — `Cli::host_roots()`'s
    /// merge root: the real host `/` for a privileged `--root` offset, the
    /// prefix itself for an unprivileged `--prefix` overlay (which cannot
    /// write the real host). `None` when unknown (the standalone `em ebuild`
    /// debug path). Deliberately NOT the package's own merge root — using
    /// that for a plain same-arch `--root` offset was tried (2026-07-12) and
    /// reverted (see [`init_build_env`]'s toolchain-selection comment): a
    /// same-arch binary built into an offset is not safely runnable
    /// unchrooted, unlike a cross-compiler (host-native by construction) or
    /// the real host's own tools.
    build_broot: Option<Utf8PathBuf>,
    /// Portage `bashrc` hooks sourced per phase after the environment is set up
    /// (profile `profile.bashrc` files in stack order, then the user's
    /// `${PORTAGE_CONFIGROOT}/etc/portage/bashrc`). Not PMS; matches portage's
    /// `__source_all_bashrcs`. The user hook is where overlay search paths can
    /// be wired without build-system knowledge in our code.
    bashrc_files: Vec<Utf8PathBuf>,
    /// Snapshot of the fully-configured shell, captured at the first sourcing
    /// and restored before every subsequent one, so that nothing a previously
    /// sourced ebuild defined — variables, eclass functions, aliases, shell
    /// options — leaks into the next (`brush_core::Shell` is a deep `Clone`).
    /// Configuration mutators reset it to `None` for re-capture.
    baseline: Option<Box<Shell>>,
    /// Path of the ebuild already sourced into the live (carried-forward) shell
    /// for the current package's phase run. Phases of one package reuse a single
    /// sourcing: the ebuild + eclass global scope runs once, and later phases run
    /// against the carried environment (matching portage's saved-env model)
    /// rather than re-sourcing — which would re-assert raw ebuild variables while
    /// `inherit` skips the eclass that mutates them (e.g. distutils-r1 converting
    /// `DISTUTILS_USE_PEP517=flit` → `flit-core`). `None` until the first phase
    /// sources it; reset when a different ebuild is run.
    phase_sourced_ebuild: Option<Utf8PathBuf>,
    repo_path: Utf8PathBuf,
    eclass_dirs: Vec<Utf8PathBuf>,
    /// Active USE flags for this shell session.
    /// Used by the `use()`, `usev()`, `usex()` functions.
    use_flags: HashSet<String>,
}

/// Run a single do*/new* install helper as a standalone process, reading build
/// state from the (exported) environment. Backs the PATH shims `init_build_env`
/// drops so `find -exec doman` / `xargs do*` reach helpers that are otherwise
/// in-shell builtins. Returns the process exit code.
///
/// A minimal brush shell is created inheriting the process environment (so ED,
/// INSDESTTREE, _insopts, _into_dir, PF, … are present as the builtins expect),
/// the same install builtins are registered, and `<name> <args…>` is run.
pub async fn run_helper(name: &str, args: &[String]) -> i32 {
    let mut shell = match Shell::builder()
        .profile(ProfileLoadBehavior::Skip)
        .rc(RcLoadBehavior::Skip)
        .parser(ParserImpl::Winnow)
        .build()
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("em __helper: failed to create shell: {e}");
            return 1;
        }
    };
    shell.register_default_builtins(brush_builtins::BuiltinSet::BashMode);
    commands::register_install_builtins(&mut shell);

    let inst_owner = commands::inst_owner::InstOwnerDefaults::default();
    inst_owner.seed_from_process_env();
    shell.set_shared(inst_owner.clone());
    let (inst_uid, inst_gid) = inst_owner.resolved_pair();
    for (name, value) in [
        ("PORTAGE_INST_UID", inst_uid),
        ("PORTAGE_INST_GID", inst_gid),
    ] {
        let _ = shell.set_env_global(name, ShellVariable::new(ShellValue::String(value)));
    }

    // Build `name 'arg1' 'arg2' …`, single-quoting each argument so paths with
    // spaces or glob characters survive re-parsing by the shell.
    let mut cmd = name.to_string();
    for a in args {
        cmd.push(' ');
        cmd.push('\'');
        cmd.push_str(&a.replace('\'', r"'\''"));
        cmd.push('\'');
    }

    let params = shell.default_exec_params();
    match shell
        .run_string(cmd, &SourceInfo::from("em __helper"), &params)
        .await
    {
        Ok(result) => u8::from(result.exit_code) as i32,
        Err(e) => {
            tracing::error!("em __helper {name}: {e}");
            1
        }
    }
}

impl EbuildShell {
    /// Create a new shell configured for the given repository.
    ///
    /// Registers Portage-specific bash functions (`inherit`, `die`,
    /// `EXPORT_FUNCTIONS`, etc.) and sets up eclass directories from
    /// the repository's `eclass/` directory.
    pub async fn new(repo: &Repository) -> Result<Self> {
        Self::new_with_cache(repo, &SourceContext::new()).await
    }

    /// Create a new shell with a shared eclass AST cache.
    ///
    /// When processing many ebuilds, pass the same [`SourceContext`] to every
    /// shell so that each eclass is parsed at most once. The cache contents
    /// (brush AST nodes) are an internal implementation detail.
    pub async fn new_with_cache(repo: &Repository, ctx: &SourceContext) -> Result<Self> {
        let eclass_cache = ctx.0.clone();
        let mut shell = Shell::builder()
            .do_not_inherit_env(true)
            .profile(ProfileLoadBehavior::Skip)
            .rc(RcLoadBehavior::Skip)
            .parser(ParserImpl::Winnow)
            // Pinned rather than left to default to the process's ambient
            // cwd (brush falls back to `std::env::current_dir()` when this
            // is unset): `run_phase` later anchors the process cwd to the
            // real build's `work_root` via `std::env::set_current_dir`
            // (below), which is itself process-global — a concurrently
            // running test/shell in the same process can observe a stale or
            // just-deleted cwd between one test's build finishing (its
            // tempdir dropped) and another's shell construction, surfacing
            // as a bare `Shell("... No such file or directory")` from
            // `getcwd()`. The repo root always exists and is a more
            // meaningful starting point than whatever the caller's process
            // cwd happened to be anyway.
            .working_dir(repo.path().as_std_path().to_path_buf())
            .build()
            .await
            .map_err(|e| Error::Shell(e.to_string()))?;

        shell.register_default_builtins(brush_builtins::BuiltinSet::BashMode);

        let eclass_dir: Utf8PathBuf = repo.path().join("eclass");
        let eclass_dirs: Vec<Utf8PathBuf> = if eclass_dir.is_dir() {
            vec![eclass_dir]
        } else {
            Vec::new()
        };

        // Register Portage-specific shell functions (die, EXPORT_FUNCTIONS, etc.)
        stubs::register(&mut shell).await?;

        // Register `inherit` with a shared eclass AST cache.
        let inherit_reg = brush_core::builtins::builtin::<inherit::InheritCommand, _>().with_state(
            inherit::InheritState {
                inherited: Vec::new(),
                cache: eclass_cache,
            },
        );
        shell.register_builtin("inherit", inherit_reg);

        // Register PMS 12.3 utility builtins (has, use, usev, usex, etc.).
        for (name, builtin) in [
            (
                "die",
                brush_core::builtins::builtin::<commands::DieCommand, _>(),
            ),
            (
                "has_version",
                brush_core::builtins::builtin::<commands::version_query::HasVersionCommand, _>(),
            ),
            (
                "best_version",
                brush_core::builtins::builtin::<commands::version_query::BestVersionCommand, _>(),
            ),
            (
                "EXPORT_FUNCTIONS",
                brush_core::builtins::builtin::<commands::ExportFunctionsCommand, _>(),
            ),
            (
                "has",
                brush_core::builtins::builtin::<commands::HasCommand, _>(),
            ),
            (
                "hasv",
                brush_core::builtins::builtin::<commands::HasvCommand, _>(),
            ),
            (
                "hasq",
                brush_core::builtins::builtin::<commands::HasCommand, _>(),
            ),
            (
                "use",
                brush_core::builtins::builtin::<commands::UseCommand, _>(),
            ),
            (
                "usev",
                brush_core::builtins::builtin::<commands::UsevCommand, _>(),
            ),
            (
                "usex",
                brush_core::builtins::builtin::<commands::UsexCommand, _>(),
            ),
            (
                "use_enable",
                brush_core::builtins::builtin::<commands::UseEnableCommand, _>(),
            ),
            (
                "use_with",
                brush_core::builtins::builtin::<commands::UseWithCommand, _>(),
            ),
            (
                "in_iuse",
                brush_core::builtins::builtin::<commands::InIuseCommand, _>(),
            ),
            (
                "docompress",
                brush_core::builtins::builtin::<commands::DocompressCommand, _>(),
            ),
            (
                "dostrip",
                brush_core::builtins::builtin::<commands::DostripCommand, _>(),
            ),
        ] {
            shell.register_builtin(name, builtin);
        }

        // Register 74 ___eapi_* EAPI predicate builtins (portage eapi.sh).
        for &name in commands::EAPI_PREDICATE_NAMES {
            shell.register_builtin(
                name,
                brush_core::builtins::builtin::<commands::EapiPredicateCommand, _>(),
            );
        }

        // Register phase-setup builtin (__ebuild_phase_funcs).
        shell.register_builtin(
            "__ebuild_phase_funcs",
            brush_core::builtins::builtin::<commands::EbuildPhaseFuncsCommand, _>(),
        );

        // Register P1 output helper builtins (einfo, ewarn, …).
        for &name in &["einfo", "elog", "ewarn", "eerror", "eqawarn", "einfon"] {
            shell.register_builtin(
                name,
                brush_core::builtins::builtin::<commands::EchoMessageCommand, _>(),
            );
        }
        shell.register_builtin(
            "ebegin",
            brush_core::builtins::builtin::<commands::EbeginCommand, _>(),
        );
        shell.register_builtin(
            "eend",
            brush_core::builtins::builtin::<commands::EendCommand, _>(),
        );

        // Register P2 build helper builtins (emake, econf).
        shell.register_builtin(
            "emake",
            brush_core::builtins::builtin::<commands::EmakeCommand, _>(),
        );
        shell.register_builtin(
            "econf",
            brush_core::builtins::builtin::<commands::EconfCommand, _>(),
        );
        // einstall: pre-EAPI-6 install helper (banned in 6+); kept for
        // completeness with legacy ebuilds.
        shell.register_builtin(
            "einstall",
            brush_core::builtins::builtin::<commands::EinstallCommand, _>(),
        );

        // Install helpers migrated from bash (INSTALL_HELPERS) to Rust builtins
        // (clap arg parsing, ${ED}/dest-tree aware). The do* doers and the new*
        // variants both live here; new* reads stdin when its source arg is `-`.
        commands::install::register_install_builtins(&mut shell);

        // Register P4 unpack builtin.
        shell.register_builtin(
            "unpack",
            brush_core::builtins::builtin::<commands::UnpackCommand, _>(),
        );

        // Register PMS 12.3.14 version manipulation builtins (ver_cut/ver_rs/
        // ver_test) as Rust builtins — avoids bash arithmetic issues in array
        // slice expressions. (An earlier note claimed ver_rs had to stay bash
        // because brush dropped empty-string args to Rust builtins; that's no
        // longer true — verified `has "" "" foo` reaches the builtin with the
        // empties intact, so the do*/new* helpers can move to Rust too.)
        shell.register_builtin(
            "ver_cut",
            brush_core::builtins::builtin::<ver_funcs::VerCutCommand, _>(),
        );
        shell.register_builtin(
            "ver_rs",
            brush_core::builtins::builtin::<ver_funcs::VerRsCommand, _>(),
        );
        shell.register_builtin(
            "ver_test",
            brush_core::builtins::builtin::<ver_funcs::VerTestCommand, _>(),
        );
        // ver_replacing (EAPI 9): outputs versions being replaced; always
        // empty during metadata extraction.
        shell.register_builtin(
            "ver_replacing",
            brush_core::builtins::builtin::<ver_funcs::VerReplacingCommand, _>(),
        );

        let die_flag = commands::die::DieFlag::default();
        shell.set_shared(die_flag.clone());

        let install_paths = commands::install_paths::InstallPaths::default();
        shell.set_shared(install_paths.clone());

        let inst_owner = commands::inst_owner::InstOwnerDefaults::default();
        shell.set_shared(inst_owner.clone());

        let terminal = terminal::TerminalState::default();
        shell.set_shared(terminal.clone());

        let mut ebuild_shell = EbuildShell {
            shell,
            distdir_override: None,
            phase_log: None,
            die_flag,
            install_paths,
            inst_owner,
            terminal,
            build_config_root: None,
            build_sysroot: None,
            build_eprefix: None,
            build_ld_library_path: None,
            build_broot: None,
            bashrc_files: Vec::new(),
            baseline: None,
            phase_sourced_ebuild: None,
            repo_path: repo.path().to_path_buf(),
            eclass_dirs,
            use_flags: HashSet::new(),
        };
        ebuild_shell.sync_eclass_dirs_var();

        Ok(ebuild_shell)
    }

    /// Set a variable that persists across subsequent phases (e.g.
    /// `REPLACING_VERSIONS`, computed by the merge driver). Exported to
    /// child processes by the per-phase export list.
    pub fn preset_var(&mut self, name: &str, value: &str) {
        self.set_var(name, value);
    }

    /// Snapshot the `docompress`/`dostrip` path lists accumulated during the
    /// install phase (PMS 12.3.9/12.3.10), for the post-install pass.
    pub fn install_paths(&self) -> commands::install_paths::InstallPathLists {
        self.install_paths.snapshot()
    }

    /// Set `PORTAGE_CONFIGROOT`, `SYSROOT`/`ESYSROOT`, `broot`, and
    /// `LD_LIBRARY_PATH` for subsequent phases. All already-resolved values
    /// from the caller — see docs/root-model.md and todo/for-sonnet.md
    /// 2026-08-08.
    pub fn set_build_roots(
        &mut self,
        config_root: Option<&Utf8Path>,
        sysroot: Option<&Utf8Path>,
        eprefix: Option<&Utf8Path>,
        broot: Option<&Utf8Path>,
        ld_library_path: Option<&str>,
    ) {
        self.build_config_root = config_root.map(Utf8Path::to_path_buf);
        self.build_sysroot = sysroot.map(Utf8Path::to_path_buf);
        self.build_eprefix = eprefix.map(Utf8Path::to_path_buf);
        self.build_broot = broot.map(Utf8Path::to_path_buf);
        self.build_ld_library_path = ld_library_path.map(str::to_string);
    }

    /// Host-side cross code generators (`cross-*`/`cross_llvm-*` binutils,
    /// gcc, gdb, clang-crossdev-wrappers) that need PATH/EPREFIX/ESYSROOT
    /// specials. llvm-runtimes use host-env package.env but are not codegen.
    /// See `docs/bash-crossdev-matrix.md`.
    pub fn is_cross_host_codegen(category: &str, pn: &str) -> bool {
        let is_cross = category.starts_with("cross-") || category.starts_with("cross_llvm-");
        is_cross && matches!(pn, "binutils" | "gcc" | "gdb" | "clang-crossdev-wrappers")
    }

    /// CTARGET triple from a cross category (`cross-<T>` or `cross_llvm-<T>`).
    pub fn cross_category_triple(category: &str) -> Option<&str> {
        category
            .strip_prefix("cross_llvm-")
            .or_else(|| category.strip_prefix("cross-"))
    }

    /// Set the `bashrc` hooks to source per phase (profile `profile.bashrc`
    /// files then the user's `/etc/portage/bashrc`), in source order.
    pub fn set_bashrc_files(&mut self, files: Vec<Utf8PathBuf>) {
        self.bashrc_files = files;
    }

    /// Describe the caller's terminal to this shell: width and palette for the
    /// `e*` output builtins, plus the environment variables an ebuild or an
    /// external helper script reads for itself.
    ///
    /// This crate has no business querying a terminal — the caller
    /// (`portage-cli`) resolves `--color`/`NO_COLOR`/TTY-detection and its own
    /// width once, and carries the answer in. The same split real portage uses
    /// between its Python and bash halves.
    ///
    /// Phase-env vars with consumers outside this crate:
    ///
    /// * `COLUMNS` — portage sets it to avoid `stty`; with `PORTAGE_BIN_PATH`
    ///   set, gentoo-functions reads `$COLUMNS` only (no stty fallback), so
    ///   missing COLUMNS forces non-tty rendering in `binutils-config` etc.
    /// * `NOCOLOR` — cargo/cmake/ruby-ng eclasses.
    /// * `NO_COLOR` — cross-project convention (python/zig eclasses,
    ///   gentoo-functions). Any non-empty value disables colour; enabled =
    ///   empty string, not a false-ish word.
    ///
    /// `PORTAGE_COLOR_*` stays out of the environment: portage
    /// exports it only because its own `e*` helpers are bash, and nothing in
    /// `::gentoo` reads it — checked across every eclass and ebuild.
    pub fn set_terminal(&mut self, terminal: terminal::TerminalConfig) {
        self.terminal.set(terminal);
        self.set_var("COLUMNS", &terminal.columns.to_string());
        let plain = terminal.colors.is_plain();
        self.set_var("NOCOLOR", if plain { "true" } else { "false" });
        self.set_var("NO_COLOR", if plain { "1" } else { "" });
    }

    /// Log phase output to `path` (created on first write): tee'd to the
    /// console, or captured silently when `quiet`.
    pub fn set_phase_log(&mut self, path: Option<(Utf8PathBuf, bool)>) {
        self.phase_log = path;
    }

    /// Use `dir` as the writable distfiles directory for this shell (the
    /// auto-resolved location becomes a read-only fallback).
    pub fn set_distdir(&mut self, dir: Utf8PathBuf) {
        self.invalidate_baseline();
        std::fs::create_dir_all(&dir).ok();
        self.distdir_override = Some(dir);
    }

    /// The effective `(DISTDIR, PORTAGE_RO_DISTDIRS)` pair: the override when
    /// set (auto-resolved primary demoted to read-only), else the resolved one.
    fn effective_distdir(&self) -> (String, Vec<String>) {
        let (resolved, mut ro) = Self::resolved_distdir();
        match &self.distdir_override {
            Some(dir) => {
                ro.insert(0, resolved);
                (dir.to_string(), ro)
            }
            None => (resolved, ro),
        }
    }

    /// Restore the configured baseline, capturing it on first use. Makes each
    /// sourcing hermetic without curated reset lists; see the `baseline` field.
    ///
    /// Interacts with [`init_build_env`](Self::init_build_env), which
    /// invalidates the baseline at the start of every phase: the baseline is
    /// then re-captured from the *live* shell at the next `run_phase`, so
    /// state set in one phase carries into the next. See the comment on that
    /// invalidation for why this is intentional.
    fn restore_baseline(&mut self) {
        match &self.baseline {
            Some(b) => self.shell = (**b).clone(),
            None => self.baseline = Some(Box::new(self.shell.clone())),
        }
    }

    /// Forget the captured baseline: the caller is reconfiguring the shell, so
    /// the next sourcing re-captures it with the new configuration included.
    fn invalidate_baseline(&mut self) {
        self.baseline = None;
    }

    /// Whether `ebuild` is the one already sourced into the live shell for the
    /// current package's phase run (see `phase_sourced_ebuild`). Lets callers
    /// (e.g. the `fetch` phase) read ebuild variables like `SRC_URI` from the
    /// carried environment instead of re-sourcing — which over an
    /// already-sourced shell would no-op the eclasses (their include guards are
    /// set) and lose their global-scope effects such as a custom `S`.
    pub fn is_phase_sourced(&self, ebuild: &Ebuild) -> bool {
        self.phase_sourced_ebuild.as_deref() == Some(ebuild.path())
    }

    /// Declare `ebuild` already sourced into the live shell for the current
    /// phase run. For an install worker that rebuilt the package state by
    /// sourcing the ebuild and overlaying the parent's captured environment:
    /// the next `run_phase` must treat it as a later phase of the same package
    /// (no re-source, no re-asserting the default `S` over the restored one).
    pub fn mark_phase_sourced(&mut self, ebuild: &Ebuild) {
        self.phase_sourced_ebuild = Some(ebuild.path().to_path_buf());
    }

    /// Append an eclass directory (searched after existing dirs).
    pub fn add_eclass_dir(&mut self, dir: Utf8PathBuf) {
        self.invalidate_baseline();
        self.eclass_dirs.push(dir);
        self.sync_eclass_dirs_var();
    }

    /// Update the `__PORTAGE_ECLASS_DIRS` shell variable to reflect the
    /// current set of eclass directories.  Called after any mutation of
    /// [`Self::eclass_dirs`].
    fn sync_eclass_dirs_var(&mut self) {
        let value: String = self
            .eclass_dirs
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(":");
        self.set_var("__PORTAGE_ECLASS_DIRS", &value);
    }

    /// Set the PMS mandatory name/version variables (`CATEGORY`, `PN`, `PV`,
    /// `PR`, `PVR`, `P`, `PF`) from an ebuild and return the derived strings.
    ///
    /// See [PMS 7.2](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
    fn set_pms_name_vars(&mut self, ebuild: &Ebuild) -> PmsNames {
        let category = ebuild.category();
        let pn = ebuild.name();
        let version = ebuild.version();
        // Use the raw filename version string to preserve leading zeros per PMS §7.2.
        // Version::to_string() normalises numeric components (26.04.0 → 26.4.0).
        let pvr = version.to_string();
        let pr = format!("r{}", version.revision.0);
        let pv = if version.revision.0 > 0 {
            pvr.strip_suffix(&format!("-{pr}"))
                .unwrap_or(&pvr)
                .to_owned()
        } else {
            pvr.clone()
        };
        let p = format!("{pn}-{pv}");
        let pf = format!("{pn}-{pvr}");

        self.set_var("CATEGORY", category);
        self.set_var("PN", pn);
        self.set_var("PV", &pv);
        self.set_var("PR", &pr);
        self.set_var("PVR", &pvr);
        self.set_var("P", &p);
        self.set_var("PF", &pf);

        PmsNames { p, pf }
    }

    /// Source an ebuild file and extract its metadata.
    ///
    /// This performs the following steps:
    /// 1. Set PM-provided variables (`CATEGORY`, `PN`, `PV`, `PVR`, `PF`, `P`,
    ///    `FILESDIR`, `WORKDIR`, etc.)
    /// 2. Source the ebuild — the `inherit` shell function handles eclass
    ///    sourcing, line continuations, and nesting automatically
    /// 3. Extract metadata variables from the shell environment
    ///
    /// See [PMS 7.2](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
    pub async fn source_ebuild(&mut self, ebuild: &Ebuild) -> Result<crate::source::SourcedEbuild> {
        // Hermetic sourcing: start from the configured baseline so nothing
        // from a previously sourced ebuild survives into this one.
        self.restore_baseline();
        // This re-sources from a clean baseline, so any phase-run sourcing of an
        // ebuild into the live shell is no longer valid; force the next phase to
        // re-source. (See `phase_sourced_ebuild`.)
        self.phase_sourced_ebuild = None;

        // Set PM-provided variables
        let PmsNames { p, pf, .. } = self.set_pms_name_vars(ebuild);
        let category = ebuild.category();
        let pn = ebuild.name();

        // FILESDIR is the ebuild's own dir + `files` (PMS), not repo+cat+pn —
        // they differ only for a `cross-*` symlink, whose real files live with
        // the symlinked ebuild, under the real category.
        let filesdir = ebuild
            .path()
            .parent()
            .map(|d| d.join("files"))
            .unwrap_or_else(|| self.repo_path.join(category).join(pn).join("files"));
        self.set_var("FILESDIR", filesdir.as_str());

        // Detect EAPI before sourcing per PMS 7.3.1
        let eapi = ebuild.detect_eapi()?;
        self.set_var("EAPI", &eapi.to_string());

        // Absolute path to the ebuild file (PMS 11.1)
        let ebuild_abs = std::fs::canonicalize(ebuild.path())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ebuild.path().to_string());
        self.set_var("EBUILD", &ebuild_abs);

        // Build-directory variables (PMS 11.1)
        // Deterministic placeholders — no temp directories are created.
        let base = format!("/var/tmp/portage/{category}/{pf}");
        let workdir = format!("{base}/work");
        self.set_var("WORKDIR", &workdir);
        self.set_var("S", &format!("{workdir}/{p}"));
        self.set_var("T", &format!("{base}/temp"));
        self.set_var("TMPDIR", &format!("{base}/temp"));
        self.set_var("HOME", &format!("{base}/homedir"));
        self.set_var("D", &format!("{base}/image/"));
        let (distdir, ro) = self.effective_distdir();
        self.set_var("DISTDIR", &distdir);
        self.set_var("PORTAGE_RO_DISTDIRS", &ro.join(" "));

        // Phase/merge variables (PMS 11.1)
        self.set_var("EBUILD_PHASE", "depend");
        self.set_var("EBUILD_PHASE_FUNC", "");
        self.set_var("ROOT", "/");
        self.set_var("MERGE_TYPE", "source");

        // EAPI 3+ prefix variables (PMS 11.1)
        if eapi >= Eapi::Three {
            self.set_var("EPREFIX", "");
            self.set_var("ED", &format!("{base}/image/"));
            self.set_var("EROOT", "/");
        }

        // EAPI 7+ sysroot variables (PMS 11.1)
        if eapi >= Eapi::Seven {
            self.set_var("SYSROOT", "/");
            self.set_var("ESYSROOT", "/");
            self.set_var("BROOT", "/");
        }

        // PMS 10.2 accumulating variables (EAPI-dependent).
        // Cleared before sourcing so the ebuild's inherit calls populate E_*
        // from scratch.  Combined with ebuild values after sourcing.
        // Mirrors Portage's B_*/E_* pattern in ebuild.sh.
        let accum_vars: &[&str] = if eapi >= Eapi::Eight {
            &[
                "IUSE",
                "REQUIRED_USE",
                "DEPEND",
                "BDEPEND",
                "RDEPEND",
                "PDEPEND",
                "IDEPEND",
                "PROPERTIES",
                "RESTRICT",
            ]
        } else {
            &[
                "IUSE",
                "REQUIRED_USE",
                "DEPEND",
                "BDEPEND",
                "RDEPEND",
                "PDEPEND",
                "IDEPEND",
            ]
        };

        // EAPI 6+ requires failglob in global scope (PMS 6, Table 6.1).
        // Reset each call so re-used shells get the right state per ebuild.
        if eapi >= Eapi::Six {
            self.run_string("shopt -s failglob").await?;
        } else {
            self.run_string("shopt -u failglob").await?;
        }

        // Source the ebuild — `inherit` is a Rust builtin that accumulates
        // each eclass's contribution into E_{VAR} and restores the var after
        // each eclass (PMS 10.2 / Portage B_*/E_* pattern).
        let params = self.shell.default_exec_params();
        self.shell
            .source_script(
                ebuild.path().as_std_path(),
                std::iter::empty::<&str>(),
                &params,
            )
            .await
            .map_err(|e| Error::Shell(format!("sourcing {}: {e}", ebuild.path())))?;

        // PMS 10.2: combine ebuild-defined values with eclass contributions.
        // After sourcing, `var` holds only what the ebuild set; `E_{var}` holds
        // the total of all eclass contributions.  Append eclass total to ebuild value.
        let e_accum_vars: &[&str] = if eapi >= Eapi::Eight {
            inherit::E_VARS_ALL
        } else {
            inherit::E_VARS_BASE
        };
        for (&var, &e_var) in accum_vars.iter().zip(e_accum_vars.iter()) {
            let ebuild_val = self.get_var(var).unwrap_or_default();
            let eclass_val = self.get_var(e_var).unwrap_or_default();
            let combined = match (ebuild_val.is_empty(), eclass_val.is_empty()) {
                (true, true) => String::new(),
                (true, false) => eclass_val.trim().to_string(),
                (false, true) => ebuild_val,
                (false, false) => format!("{} {}", ebuild_val, eclass_val.trim()),
            };
            self.set_var(var, &combined);
            self.set_var(e_var, ""); // clean up E_*
        }

        // Extract metadata, then override EAPI with the pre-detected value
        // (the authoritative source per PMS 7.3.1)
        let mut metadata = self.extract_metadata()?;
        metadata.eapi = eapi;

        // CacheEntry::parse derives `inherited` from `_eclasses_`, which doesn't
        // exist yet during regen. Read the transitive list directly from the
        // `inherit` builtin's Rust state — no bash-string parsing needed. The
        // resolved file paths come along too so the cache writer can md5 each
        // eclass without re-resolving the name (which would miss masters).
        let inherited = self
            .shell
            .builtin_state_of::<inherit::InheritCommand>("inherit")
            .map(|s| s.inherited.clone())
            .unwrap_or_default();
        metadata.inherited = inherited.iter().map(|e| e.name.clone()).collect();
        let eclasses = inherited.into_iter().map(|e| (e.name, e.path)).collect();

        Ok(crate::source::SourcedEbuild { metadata, eclasses })
    }

    /// Locate portage's script directory under `/usr/lib/portage`.
    ///
    /// Scans for a subdirectory (typically `pythonX.Y`) that contains
    /// `isolated-functions.sh`, and returns the highest-sorted match.
    fn find_portage_bin_path() -> Option<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir("/usr/lib/portage")
            .ok()?
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.is_dir() && p.join("isolated-functions.sh").exists() {
                    Some(p)
                } else {
                    None
                }
            })
            .collect();
        dirs.sort();
        dirs.pop()
    }

    /// Set up the build environment.
    ///
    /// Exports `PORTAGE_BIN_PATH` (when portage is installed; some eclasses
    /// reference it) but relies on our own self-contained `do*`/`new*` install
    /// helpers rather than portage's `ebuild-helpers/`,
    /// configures a writable `DISTDIR`, passes through build-tool variables
    /// (`CFLAGS`, `MAKEOPTS`, …) from the caller's environment, and defines
    /// the per-EAPI default phase implementation bash functions
    /// (`__eapi0_src_unpack`, `__eapi2_src_compile`, …) that `__ebuild_phase_funcs`
    /// wires together.
    ///
    /// The output helpers (`einfo`, `ewarn`, …), predicates (`___eapi_*`),
    /// `emake`, `econf`, and `__ebuild_phase_funcs` are registered as Rust
    /// builtins in `new_with_cache` and therefore never sourced from portage.
    pub async fn init_build_env(&mut self) -> Result<()> {
        // Invalidate at the start of every phase. With restore_baseline()'s
        // capture-on-first-use, this is what yields phase-to-phase state
        // persistence within a package: once this phase mutates the shell the
        // baseline stays None, so the *next* run_phase's restore_baseline()
        // re-captures the then-current shell — exported vars and other state
        // set in one phase are visible in the next (portage gets the same
        // effect by saving/restoring the ebuild environment between phases,
        // and REPLACING_VERSIONS / USE rely on it here). Package isolation
        // comes from a fresh EbuildShell per package in the merge driver, NOT
        // from the baseline; do not hoist this invalidation out expecting
        // stricter hermeticity — it would drop inter-phase state.
        self.invalidate_baseline();
        // Establish PATH as a real shell variable (not just the inherited
        // process env) so eclasses that do `export PATH="...:${PATH}"` — e.g.
        // python-any-r1's wrapper setup — keep the system bin dirs instead of
        // expanding ${PATH} to empty and stranding mkdir/cp/ln/chmod.
        //
        // Sanitise it: expunge non-system install dirs that would shadow the
        // Gentoo toolchain — everything under $HOME (uv/cargo/pip user installs,
        // e.g. ~/.local/bin/python3.13, a uv python without gpep517 that broke
        // distutils-r1 wheel builds) and /usr/local (locally-installed tools).
        // System dirs including /usr/lib/llvm/*/bin (clang) are kept; a --local
        // prefix's own bin is re-added deliberately by its bashrc hook.
        let raw_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
        let home = std::env::var("HOME").unwrap_or_default();
        let home_prefix = (!home.is_empty()).then(|| format!("{}/", home.trim_end_matches('/')));
        let base_path = raw_path
            .split(':')
            .filter(|p| {
                let under_home = home_prefix
                    .as_deref()
                    .is_some_and(|hp| *p == home || p.starts_with(hp));
                let under_local = *p == "/usr/local" || p.starts_with("/usr/local/");
                !under_home && !under_local
            })
            .collect::<Vec<_>>()
            .join(":");
        self.set_var("PATH", &base_path);
        // TERM rides through from the host, as it does in portage
        // (`special_env_vars.py`'s environ_whitelist). Leaving it unset is not
        // neutral: bash substitutes `dumb` for an unset TERM, and `dumb` is
        // what every terminal-capability probe tests for first — real
        // `gentoo-functions` drops its whole palette on it
        // (`rc.sh`'s `_has_color_terminal`), so `elibtoolize` and friends came
        // out flat even in a fully capable terminal.
        if let Ok(term) = std::env::var("TERM") {
            self.set_var("TERM", &term);
        }
        // Our do*/new* install helpers are Rust builtins plus a few bash
        // wrappers (INSTALL_HELPERS below), so portage need not be installed. We still
        // export PORTAGE_BIN_PATH when available because some eclasses reference
        // it (e.g. for misc data files), but we do NOT prepend its
        // ebuild-helpers/ to PATH: the in-shell functions are authoritative and
        // would only be shadowed by the forked scripts there.
        if let Some(bin_path) = Self::find_portage_bin_path() {
            self.set_var("PORTAGE_BIN_PATH", &bin_path.to_string_lossy());
        }

        let (distdir, ro) = self.effective_distdir();
        self.set_var("DISTDIR", &distdir);
        self.set_var("PORTAGE_RO_DISTDIRS", &ro.join(" "));

        // Pass through build-tool variables from the caller's environment.
        for var in &[
            "MAKEOPTS",
            "CFLAGS",
            "CXXFLAGS",
            "CPPFLAGS",
            "LDFLAGS",
            "RUSTFLAGS",
            "CC",
            "CXX",
            "AR",
            "RANLIB",
            "NM",
            "STRIP",
            "PKG_CONFIG",
        ] {
            if let Ok(val) = std::env::var(var) {
                self.set_var(var, &val);
            }
        }

        // MAKEOPTS default: portage derives `-j<nproc>` when neither make.conf
        // nor the process env sets it (make.globals leaves it empty and the PM
        // computes a default in config.py). Without this, emake runs serially
        // on every build — a 128-core box builds gcc one file at a time
        //. Apply only
        // if still unset after the env pass-through above so an explicit
        // make.conf or `MAKEOPTS=... em ...` always wins. Conservative: use
        // 2/3 of cores + 1 for `-j` (I/O-bound link steps and memory-heavy
        // compiles like gcc/lto should not fully saturate; mirrors the heuristic
        // Portage itself recommends), plus `-l<cores>` so make throttles new
        // jobs once the load average reaches the core count (avoids thrash on
        // memory-heavy link steps).
        if self
            .get_var("MAKEOPTS")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
            let j = cores * 2 / 3 + 1;
            self.set_var("MAKEOPTS", &format!("-j{j} -l{cores}"));
        }

        // An LD_PRELOAD fake-root session (pseudoroot) only holds if every
        // phase child loads the interposer; pass its variables through the
        // otherwise clean build env, exported, as portage does for its
        // sandbox preload.
        if std::env::vars().any(|(k, _)| k.starts_with("PSEUDOROOT_")) {
            for (key, val) in std::env::vars() {
                if key == "LD_PRELOAD" || key.starts_with("PSEUDOROOT_") {
                    let mut var = ShellVariable::new(ShellValue::String(val));
                    var.export();
                    let _ = self.shell.set_env_global(&key, var);
                }
            }
        }

        // Default CBUILD to CHOST when unset, as portage does (`portageq envvar
        // CBUILD` yields CHOST on a native system even with no CBUILD in
        // make.conf). Without it `econf` omits `--build`, so configure sees
        // `--host` alone, leaves `cross_compiling=maybe`, and strict packages
        // (python) `die` "Cross compiling required --host and --build" on a plain
        // native `--root` build.
        if let Some(chost) = self.get_var("CHOST").filter(|s| !s.is_empty())
            && self.get_var("CBUILD").filter(|s| !s.is_empty()).is_none()
        {
            self.set_var("CBUILD", &chost);
        }

        // Toolchain selection: when CHOST≠CBUILD (cross) or a prefix has its
        // own compiler (`build_eprefix`), export `${tool_tuple}-<tool>` unless
        // the ebuild already set them — proactive `tc-getPROG` so raw
        // `./configure` ebuilds don't pick host gcc. Plain `--root` leaves
        // PATH alone (catalyst seed-compiler model).
        //
        // Cross tools under `--local` live in `<build_broot>/usr/bin` (often
        // under $HOME, stripped from sanitised PATH); prepend that dir.
        // Host crossdev already has tools on PATH.
        if let (Some(chost), Some(cbuild)) = (
            self.get_var("CHOST").filter(|s| !s.is_empty()),
            self.get_var("CBUILD").filter(|s| !s.is_empty()),
        ) && (chost != cbuild || self.build_eprefix.is_some())
        {
            let prefix_bin = self
                .build_broot
                .as_deref()
                .map(|broot| broot.join("usr/bin"));
            let tool_exists = |bin: Option<&Utf8PathBuf>, tuple: &str, tool: &str| -> bool {
                match bin {
                    Some(bin) => bin.join(format!("{tuple}-{tool}")).is_file(),
                    None => program_on_path(&format!("{tuple}-{tool}")),
                }
            };
            let tool_path = |bin: Option<&Utf8PathBuf>, tuple: &str, tool: &str| -> Utf8PathBuf {
                match bin {
                    // Use the absolute prefix path when known: em's own
                    // post-`src_install` estrip (and any helper) runs the tool
                    // outside the build shell, with the host process PATH that
                    // lacks the $HOME prefix bin — a bare `${tuple}-strip` then
                    // fails. A bare name is fine for host crossdev (on PATH).
                    Some(bin) => bin.join(format!("{tuple}-{tool}")),
                    None => Utf8PathBuf::from(format!("{tuple}-{tool}")),
                }
            };

            // Target-code packages (`cross-<T>/glibc`, headers) have CTARGET
            // in package.env without TARGET_ABI → compile with `${CTARGET}-*`.
            // Host tools (binutils/gcc) set TARGET_ABI and keep host CHOST.
            // Do not pre-set CC to host CHOST when ambient CHOST==CBUILD under
            // `use_outer_eroot`: glibc's `tc-getCPP ${CTARGET}` only self-
            // corrects when CC is unset.
            let target_abi_set = self
                .get_var("TARGET_ABI")
                .filter(|s| !s.is_empty())
                .is_some();
            let target_tuple = self
                .get_var("CTARGET")
                .filter(|t| !t.is_empty() && !target_abi_set && *t != chost);
            let produces_target_code = target_tuple.is_some();
            let tool_tuple = target_tuple.unwrap_or_else(|| chost.clone());

            let prefix_bin = prefix_bin.filter(|bin| tool_exists(Some(bin), &tool_tuple, "gcc"));
            if let Some(bin) = &prefix_bin {
                let path = self.get_var("PATH").unwrap_or_default();
                if !path.split(':').any(|p| p == bin.as_str()) {
                    self.set_var("PATH", &format!("{bin}:{path}"));
                }
            }
            if prefix_bin.is_some() || program_on_path(&format!("{tool_tuple}-gcc")) {
                for (var, tool) in [
                    ("CC", "gcc"),
                    ("CXX", "g++"),
                    ("AR", "ar"),
                    ("AS", "as"),
                    ("NM", "nm"),
                    ("RANLIB", "ranlib"),
                    ("STRIP", "strip"),
                    ("OBJCOPY", "objcopy"),
                    ("OBJDUMP", "objdump"),
                    ("READELF", "readelf"),
                    ("LD", "ld"),
                    ("PKG_CONFIG", "pkg-config"),
                ] {
                    if self.get_var(var).filter(|s| !s.is_empty()).is_none() {
                        // Unlike the other 11 (built by crossdev's own toolchain
                        // steps alongside gcc, so they exist whenever gcc does),
                        // `${tool_tuple}-pkg-config` is never created by anything
                        // em builds — real crossdev relies on a
                        // separately-installed `cross-pkg-config` wrapper this
                        // project doesn't reproduce. Blindly setting PKG_CONFIG
                        // here pointed at a file that doesn't exist, which —
                        // unlike an unset var — short-circuits
                        // `tc-getPKG_CONFIG`'s own "already set" fast path and
                        // skips its real PATH-search/bare-name fallback.
                        // Require the specific tool to exist, not only gcc.
                        if tool_exists(prefix_bin.as_ref(), &tool_tuple, tool) {
                            self.set_var(
                                var,
                                tool_path(prefix_bin.as_ref(), &tool_tuple, tool).as_str(),
                            );
                        }
                    }
                }
            }

            // A genuine target package's own toolchain vars now point at
            // CTARGET (above); also give real toolchain-funcs.eclass's
            // `tc-getBUILD_*` family (`BUILD_CC`, `BUILD_CXX`, …) the ambient
            // host/CBUILD-side tools, so a host-side sub-probe that correctly
            // asks for the *build* compiler still gets one — matching
            // `tc-getBUILD_PROG`'s own real var-check order (`BUILD_CC` /
            // `CC_FOR_BUILD` / `HOSTCC`, never falling back to plain `CC` at
            // all once genuinely cross-compiling).
            if produces_target_code {
                let host_bin = self
                    .build_broot
                    .as_deref()
                    .map(|broot| broot.join("usr/bin"));
                let host_bin = host_bin.filter(|bin| tool_exists(Some(bin), &chost, "gcc"));
                if host_bin.is_some() || program_on_path(&format!("{chost}-gcc")) {
                    for (var, tool) in [
                        ("BUILD_CC", "gcc"),
                        ("BUILD_CXX", "g++"),
                        ("BUILD_AR", "ar"),
                        ("BUILD_NM", "nm"),
                        ("BUILD_RANLIB", "ranlib"),
                        ("BUILD_STRIP", "strip"),
                        ("BUILD_OBJCOPY", "objcopy"),
                        ("BUILD_READELF", "readelf"),
                        ("BUILD_LD", "ld"),
                        ("BUILD_PKG_CONFIG", "pkg-config"),
                    ] {
                        if self.get_var(var).filter(|s| !s.is_empty()).is_none()
                            && tool_exists(host_bin.as_ref(), &chost, tool)
                        {
                            self.set_var(var, tool_path(host_bin.as_ref(), &chost, tool).as_str());
                        }
                    }
                }
            }
        }

        // Strip GNU make jobserver tokens from MAKEFLAGS if set.  The fds
        // (--jobserver-auth=R,W or legacy --jobserver-fds=R,W) belong to a
        // make process in the caller's tree and are not valid here.  Leaving
        // them in causes every make invocation in every phase to try to open
        // dead file descriptors.
        if let Ok(flags) = std::env::var("MAKEFLAGS") {
            let clean = strip_jobserver_tokens(&flags);
            self.set_var("MAKEFLAGS", &clean);
        }

        // Remove bash stub no-ops that were installed for metadata extraction.
        // These stubs shadow the Rust builtins for econf, emake, einfo, etc.
        // Unsetting them lets the Rust builtin registry take over during build.
        self.run_string(
            "unset -f econf emake einstall unpack einfo einfon elog ewarn eerror eqawarn ebegin eend nonfatal has_version best_version docompress dostrip \
             dodir keepdir doins doexe dobin dosbin dodoc doheader doinfo doman domo dolib dolib.a dolib.so dosym fperms fowners \
             newbin newsbin newins newexe newdoc newman newheader newlib.a newlib.so newinitd newconfd newenvd",
        )
        .await
        .ok();

        // Define per-EAPI default phase implementations as bash functions.
        // These are called by __ebuild_phase_funcs (a Rust builtin) to set up
        // default() and default_<phase>() for the currently executing phase.
        self.run_string(PHASE_DEFAULT_FUNCTIONS).await?;

        // Define real P3 install helpers (dobin, doins, dodoc, dosym, …).
        // These override the no-op stubs from builtins.rs that were needed
        // for metadata extraction.
        self.run_string(INSTALL_HELPERS).await?;

        // Sandbox control functions as no-ops. Portage provides these to
        // register paths with its LD_PRELOAD sandbox; with no sandbox active
        // they only need to exist so eclasses calling them (e.g. python-any-r1,
        // kernel/linux-info via addpredict) don't abort with "command not
        // found". Real write-confinement is M3.
        self.run_string(SANDBOX_STUBS).await?;

        // Reconstruct USE_EXPAND group variables from the final USE set. Some
        // groups (e.g. llvm-r1's LLVM_SLOT) are populated only by an IUSE
        // default (`+llvm_slot_21`) with no make.conf value, so without this
        // reverse mapping the eclass reads an empty LLVM_SLOT and dies.
        self.populate_use_expand_vars();

        Ok(())
    }

    /// Set each prefixed `USE_EXPAND` group variable to the values of its
    /// enabled flags (the reverse of profile expansion): for group `FOO`, every
    /// enabled `foo_<value>` flag contributes `<value>` to `FOO`. Eclasses read
    /// these variables directly (`LLVM_SLOT`, `PYTHON_TARGETS`, …). Only groups
    /// with at least one enabled flag are written, so a group set by other means
    /// is never blanked. `USE_EXPAND_UNPREFIXED` groups are left alone (their
    /// flags are indistinguishable from plain USE without the value list).
    fn populate_use_expand_vars(&mut self) {
        let use_str = self.get_var("USE").unwrap_or_default();
        let use_expand = self.get_var("USE_EXPAND").unwrap_or_default();
        if use_expand.is_empty() {
            return;
        }
        let enabled: Vec<&str> = use_str.split_whitespace().collect();
        for key in use_expand.split_whitespace() {
            let prefix = format!("{}_", key.to_ascii_lowercase());
            let mut values: Vec<&str> = enabled
                .iter()
                .filter_map(|f| f.strip_prefix(prefix.as_str()))
                .collect();
            if values.is_empty() {
                continue;
            }
            values.sort_unstable();
            self.set_var(key, &values.join(" "));
        }
    }

    /// Source an ebuild and run a single phase function.
    ///
    /// Creates the standard build directories under `work_root` if they don't
    /// exist, sets all PMS environment variables, sources the ebuild (which
    /// triggers `inherit` and populates eclass functions), then calls the
    /// phase function if it is defined.
    ///
    /// Unlike `source_ebuild`, no metadata extraction is performed.  Output
    /// from the phase (stdout/stderr) is passed through to the caller's
    /// terminal.
    ///
    /// # Arguments
    /// * `ebuild`    – the ebuild to source
    /// * `phase` – portage phase name (`"compile"`, `"install"`, …) or
    ///   function name (`"src_compile"`)
    /// * `work_root` – root for build dirs; `work/`, `temp/`, `image/` are
    ///   created beneath it
    pub async fn run_phase(
        &mut self,
        ebuild: &Ebuild,
        phase: &str,
        work_root: &Path,
        root: &Path,
    ) -> Result<()> {
        // Hermetic sourcing, as in `source_ebuild`.
        self.restore_baseline();

        // A package's phases share a single sourcing of the ebuild: its global
        // scope (and every eclass's, via `inherit`) runs once on the first
        // phase, and later phases execute against the carried-forward shell.
        // Re-sourcing each phase would re-assert the ebuild's raw global
        // variables while `inherit` skips the now-"already inherited" eclasses,
        // dropping eclass global-scope mutations (e.g. distutils-r1 converting
        // `DISTUTILS_USE_PEP517=flit` → `flit-core`). See `phase_sourced_ebuild`.
        let need_source = self.phase_sourced_ebuild.as_deref() != Some(ebuild.path());

        let PmsNames { p, .. } = self.set_pms_name_vars(ebuild);
        let category = ebuild.category();
        let pn = ebuild.name();

        // Host-codegen specials (PATH/EPREFIX/ESYSROOT/-idirafter) for
        // binutils/gcc/gdb/clang wrappers only — not every host-env package.
        let host_codegen = Self::is_cross_host_codegen(category, pn);
        let cross_triple = Self::cross_category_triple(category).map(str::to_owned);

        // FILESDIR is the ebuild's own dir + `files` (PMS), not repo+cat+pn —
        // they differ only for a `cross-*` symlink, whose real files live with
        // the symlinked ebuild, under the real category.
        let filesdir = ebuild
            .path()
            .parent()
            .map(|d| d.join("files"))
            .unwrap_or_else(|| self.repo_path.join(category).join(pn).join("files"));
        self.set_var("FILESDIR", filesdir.as_str());

        let eapi = ebuild.detect_eapi()?;
        self.set_var("EAPI", &eapi.to_string());

        let ebuild_abs = std::fs::canonicalize(ebuild.path())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ebuild.path().to_string());
        self.set_var("EBUILD", &ebuild_abs);

        // Source portage's function libraries and configure build environment.
        // Must happen after EAPI is set so phase-functions.sh registers the
        // right EAPI-specific defaults.
        self.init_build_env().await?;

        // Create and set real build directories.
        let workdir = work_root.join("work");
        let t = work_root.join("temp");
        let d = work_root.join("image");
        let homedir = work_root.join("homedir");
        // `${T}/logging` is portage's `PKG_LOGDIR`: its existence is what enables
        // the `e*` builtins' elog capture, so it is created unconditionally per
        // package (as `prepare_build_dirs.py` does) and the *dispatch* side
        // decides what, if anything, to do with what lands there.
        let pkg_logdir = t.join("logging");
        for dir in [&workdir, &t, &d, &homedir, &pkg_logdir] {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::Shell(format!("creating {}: {e}", dir.display())))?;
        }
        // Anchor the *process* working directory inside the build tree. The brush
        // shell tracks its own `working_dir` and sets it on each spawned command,
        // but some eclass helpers (notably autotools' `eautoconf`/`eaclocal`, which
        // run `autoconf -o configure.sh` etc.) reach the process cwd directly — and
        // that would otherwise be wherever `em` was launched (e.g. the user's source
        // checkout), where they drop `aclocal.m4`/`config.h.in`/symlinks. Anchor to
        // `work_root` (not WORKDIR): it survives the post-merge cleanup, which wipes
        // work/image/temp/homedir — anchoring to WORKDIR would leave the process cwd
        // a *deleted* dir, and the next package's shell init `getcwd()`s and fails
        // with ENOENT. The per-phase `cd "${S}"` still moves the shell into the tree.
        let _ = std::env::set_current_dir(work_root);
        self.set_var("WORKDIR", &workdir.to_string_lossy());
        // S defaults to ${WORKDIR}/${P}; the ebuild may override it at global
        // scope while sourcing. Only (re)assert the default when about to source,
        // so later phases keep the value carried from the first phase.
        if need_source {
            self.set_var("S", &workdir.join(&p).to_string_lossy());
        }
        self.set_var("T", &t.to_string_lossy());
        self.set_var("TMPDIR", &t.to_string_lossy());
        self.set_var("HOME", &homedir.to_string_lossy());
        self.set_var("D", &format!("{}/", d.display()));
        // DISTDIR is already set by init_build_env() from env or ~/.cache/distfiles;
        // do not override it here.

        // Drop PATH shims for the do*/new* install helpers so ebuilds that run
        // them via `find -exec`/`xargs` (which need a real executable, not an
        // in-shell builtin) work. Each shim re-invokes `em __helper <name>`,
        // which runs the same builtin logic against the exported build env.
        // Written once per package; the directory is prepended to PATH each
        // phase (init_build_env rebuilds PATH from scratch).
        let helper_dir = work_root.join(".em-helpers");
        if need_source && let Ok(em) = std::env::current_exe() {
            if let Err(e) = std::fs::create_dir_all(&helper_dir) {
                return Err(Error::Shell(format!(
                    "creating helper shim dir {}: {e}",
                    helper_dir.display()
                )));
            }
            use std::os::unix::fs::PermissionsExt;
            for name in commands::HELPER_NAMES {
                let shim = helper_dir.join(name);
                let script = format!(
                    "#!/bin/sh\nexec '{}' __helper '{name}' \"$@\"\n",
                    em.display()
                );
                if std::fs::write(&shim, script).is_ok() {
                    let _ = std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755));
                }
            }
        }
        let path = self.get_var("PATH").unwrap_or_default();
        self.set_var("PATH", &format!("{}:{path}", helper_dir.display()));

        // Phase and merge variables.
        let (phase_val, func_name) = phase_to_func(phase);
        self.set_var("EBUILD_PHASE", phase_val);
        self.set_var("EBUILD_PHASE_FUNC", func_name);
        // Normalise root: always ends with '/'.
        let root_str = {
            let s = root.to_string_lossy();
            if s.ends_with('/') {
                s.into_owned()
            } else {
                format!("{s}/")
            }
        };
        // Prepend `<root>/usr/bin` only for host-side cross codegen so later
        // steps find earlier ones' `<T>-*` tools (no host copy). Never for
        // plain `--root` stage1: that would shadow host tools with
        // ROOT-installed binaries that lack LD_LIBRARY_PATH. Prefix/local
        // already have working PATH hooks.
        if root_str != "/"
            && self.build_eprefix.is_none()
            && self.build_sysroot.is_none()
            && host_codegen
        {
            let bin = format!("{root_str}usr/bin");
            let path = self.get_var("PATH").unwrap_or_default();
            if !path.split(':').any(|p| p == bin) {
                self.set_var("PATH", &format!("{bin}:{path}"));
            }
        }
        // In-place prefix (`--local`): `root` (the merge root) is EROOT; the
        // package is configured for EPREFIX, so the live ROOT is `/` and files
        // stage under `ED = D + EPREFIX`. Without an eprefix this is a no-op
        // (ROOT = EROOT = root, EPREFIX = "", ED = D) — host/`--prefix` paths
        // are unchanged.
        let eprefix = self
            .build_eprefix
            .as_deref()
            .map(|p| p.as_str().trim_end_matches('/').to_string())
            .unwrap_or_default();

        // A `cross-<tuple>/*` host toolchain tool (binutils/gcc/gdb/
        // clang-crossdev-wrappers — see the ESYSROOT block below for the same
        // filter) needs an EPREFIX-style offset regardless of `--local`, for a
        // reason distinct from ESYSROOT: `toolchain.eclass` computes its own
        // `PREFIX=${EPREFIX}/usr` and passes `--prefix=${PREFIX}` to the
        // package's own `./configure` — and DESTDIR+prefix is a *physical*
        // install-path convention (`make install DESTDIR=${D}` literally
        // writes under `${D}${prefix}/...`), so unlike ESYSROOT (a pure
        // DEPEND-resolution hint) this one is NOT just cosmetic: whatever
        // `PREFIX`/`--with-sysroot` bakes in also determines where the built
        // files actually land inside `${D}`, and our own `ED` must match it
        // for the merge step to find them.
        //
        // `--local` already supplies this correctly (root_str == eprefix
        // there, by construction of the root model). A self-contained
        // `--root DIR` (no `--local`) has `eprefix` empty, so
        // `toolchain.eclass`'s baked-in `--with-sysroot` collapsed to the bare
        // host path `/usr/<tuple>` (the *host's* own unrelated real crossdev
        // sysroot, if any, not this root's)
        // from-scratch cross-stage1 test, Fix:
        // for this package class only, when `eprefix` is otherwise empty,
        // offset it exactly as `--local` would (root_str becomes EPREFIX, ROOT
        // becomes "/") — this reuses the *already-correct*, already-tested
        // EPREFIX-subtree merge logic (`ebuild.rs::ed_image_dir`) generically,
        // rather than inventing a new merge path for this one package class.
        //
        // SYSROOT/ESYSROOT are untouched by this flip and must stay so:
        // SYSROOT already equals `root_str` for a plain `--root` build (unlike
        // `--local`, where it's host `/`) — which is *already* correct for a
        // self-contained host toolchain (it must link against the root's own,
        // not the real host's, native libc — that's the native toolchain this
        // same root just bootstrapped). ESYSROOT for this package class is
        // computed straight from `root_str` below, independent of `eprefix` —
        // so flipping `eprefix` here does not double-count either.
        //
        // NOTE for future refactoring: this function derives `ROOT`, `EPREFIX`,
        // `ED`, `EROOT`, `SYSROOT`, `ESYSROOT` — six PMS location variables —
        // through a chain of local variables computed in sequence. `PATH`
        // above and `EPREFIX`/`ESYSROOT` here all key off the same
        // `build_class` signal (computed once, near `category`/`pn`,
        // above). If a further package-class special-case shows up, this
        // whole block deserves extracting into a small `RootVars { root,
        // eprefix, ed, eroot, sysroot, esysroot }` value type built by one
        // function that takes `(build_class, root_str, build_sysroot,
        // build_eprefix)` and returns all six together — so the invariants
        // connecting them (ED must track EPREFIX; ESYSROOT must NOT
        // double-count a flipped EPREFIX) are enforced in one place instead
        // of by convention across a 100-line function.
        let eprefix = if host_codegen && eprefix.is_empty() && root_str != "/" {
            root_str.trim_end_matches('/').to_string()
        } else {
            eprefix
        };

        // A build-time tool the package compiles and re-executes may need
        // its own just-built or host-shared libs. See todo/for-sonnet.md
        // 2026-08-08.
        if let Some(llp) = self.build_ld_library_path.as_deref() {
            let existing = self.get_var("LD_LIBRARY_PATH").unwrap_or_default();
            let combined = if existing.is_empty() {
                llp.to_string()
            } else {
                format!("{llp}:{existing}")
            };
            self.set_var("LD_LIBRARY_PATH", &combined);
        }

        let root_var = if eprefix.is_empty() {
            root_str.clone()
        } else {
            "/".to_string()
        };
        self.set_var("ROOT", &root_var);
        self.set_var("MERGE_TYPE", "source");
        // PORTAGE_CONFIGROOT: where profile/make.conf live (host unless offset).
        let configroot = self
            .build_config_root
            .as_deref()
            .map_or("/", Utf8Path::as_str)
            .to_string();
        self.set_var("PORTAGE_CONFIGROOT", &configroot);

        // EPREFIX/ED/EROOT are PMS EAPI-3+ vars, but the install helpers use
        // `${ED}` unconditionally, so always set them (ED == D when EPREFIX is
        // empty, matching portage's EAPI 0-2 behaviour).
        self.set_var("EPREFIX", &eprefix);
        // ED = D + EPREFIX (the prefix subtree within the image); == D when
        // EPREFIX is empty.
        let ed = if eprefix.is_empty() {
            format!("{}/", d.display())
        } else {
            format!("{}/{}/", d.display(), eprefix.trim_start_matches('/'))
        };
        self.set_var("ED", &ed);
        // EROOT = ROOT + EPREFIX, i.e. the merge root.
        self.set_var("EROOT", &root_str);
        if eapi >= Eapi::Seven {
            // SYSROOT = the base the build resolves DEPEND against (the host for
            // a --prefix overlay; ROOT otherwise). SYSROOT's trailing slash is
            // stripped ("/"→"") to avoid autotools.eclass bug 654600.
            let sysroot = match self.build_sysroot.as_deref() {
                Some(p) => {
                    let s = p.as_str();
                    if s.ends_with('/') {
                        s.to_string()
                    } else {
                        format!("{s}/")
                    }
                }
                None => root_str.clone(),
            };
            let sysroot_trimmed = sysroot.trim_end_matches('/');
            self.set_var("SYSROOT", sysroot_trimmed);
            // ESYSROOT = SYSROOT + EPREFIX (PMS 11.1): the location of DEPEND
            // headers/libs/data. For `--local` this is the prefix (SYSROOT=/ +
            // EPREFIX=~/.gentoo), so ebuilds that reference `${ESYSROOT}/usr`
            // (e.g. spirv-tools' `-DSPIRV-Headers_SOURCE_DIR`) find prefix-built
            // deps — while SYSROOT stays `/`, so cmake/autotools do NOT pass
            // `--sysroot` and the compiler keeps host glibc (features.h). They
            // are equal (no EPREFIX) for host / ROOT-offset `--prefix` builds.
            //
            // Host cross tools need ESYSROOT = cross sysroot (`<EROOT>/usr/<T>/`)
            // for toolchain.eclass `--with-build-sysroot`. Target packages keep
            // SYSROOT+EPREFIX (else alt_prefix doubles `/usr/<T>`).
            //
            // When `build_sysroot` is None, `sysroot` already is the full
            // install target (plain `--root` or global `--target` with
            // base==target); do not append outer eprefix or ESYSROOT doubles
            // and breaks header search.
            let esysroot = if let (true, Some(triple)) = (host_codegen, cross_triple.as_deref()) {
                format!("{root_str}usr/{triple}/")
            } else if eprefix.is_empty() || self.build_sysroot.is_none() {
                sysroot.clone()
            } else {
                format!("{}/{}/", sysroot_trimmed, eprefix.trim_start_matches('/'))
            };
            self.set_var("ESYSROOT", &esysroot);
            self.set_var("BROOT", "/");

            // Host cross tools under a prefix compiler: BDEPENDs like elfutils
            // may only exist on the host. `-idirafter /usr/include` finds host
            // headers without shadowing prefix includes.
            if host_codegen && self.build_sysroot.is_some() {
                let cppflags = self.get_var("CPPFLAGS").unwrap_or_default();
                let updated = format!("{cppflags} -idirafter /usr/include");
                self.set_var("CPPFLAGS", updated.trim_start());
            }
        }

        // PORTAGE_INST_UID/GID: owner dobin/dosbin apply via `install -o/-g`.
        // Resolve Portage-style (0/0 privileged, EROOT owner in unprivileged
        // mode, make.conf overrides) into shared state and sync shell vars so
        // `export` and PATH shims (`em __helper`) inherit the same values.
        let (inst_uid, inst_gid) =
            commands::inst_owner::resolve_inst_owner(&self.shell, &self.inst_owner, root);
        self.set_var("PORTAGE_INST_UID", &inst_uid);
        self.set_var("PORTAGE_INST_GID", &inst_gid);

        let accum_vars: &[&str] = if eapi >= Eapi::Eight {
            &[
                "IUSE",
                "REQUIRED_USE",
                "DEPEND",
                "BDEPEND",
                "RDEPEND",
                "PDEPEND",
                "IDEPEND",
                "PROPERTIES",
                "RESTRICT",
            ]
        } else {
            &[
                "IUSE",
                "REQUIRED_USE",
                "DEPEND",
                "BDEPEND",
                "RDEPEND",
                "PDEPEND",
                "IDEPEND",
            ]
        };
        let e_vars: &[&str] = if eapi >= Eapi::Eight {
            inherit::E_VARS_ALL
        } else {
            inherit::E_VARS_BASE
        };

        if eapi >= Eapi::Six {
            self.run_string("shopt -s failglob").await?;
        } else {
            self.run_string("shopt -u failglob").await?;
        }

        // Export all PM-provided variables so external processes (make,
        // ./configure, …) inherit them as environment variables. Our do*/new*
        // install helpers run in-shell and read these directly.
        // Bash `export` on an unset/empty name is harmless — it just marks it for export.
        //
        // CHOST/CBUILD/CTARGET must be here: real portage's config object
        // exports its whole settings dict as the ebuild process's real OS
        // environment from the start, so every real subprocess a plain
        // ebuild/eclass command spawns (e.g. openssl's `bash
        // "${FILESDIR}/gentoo.config"`) inherits them transparently. em instead
        // sources make.conf/profile into an already-running brush shell as
        // plain (non-exported) assignments — fine for our own Rust builtins
        // (econf/emake read brush's variable table directly, bypassing export
        // entirely) but invisible to a real child process unless listed here.
        // Omitting CHOST specifically meant `gentoo.config` saw no CHOST at
        // all, so openssl's `Configure` fell back to autodetecting the *build
        // host's* kernel arch via `uname` — silently producing a host-arch
        // Configure target (and ARM-only assembly files) under `--cross`,
        // even though CC/CFLAGS (which real Rust builtins forward explicitly)
        // were already correctly cross-targeted.
        self.run_string(
            "export CATEGORY PN PV PR PVR P PF FILESDIR WORKDIR S T D TMPDIR EAPI EBUILD \
             HOME ROOT DISTDIR PORTAGE_BIN_PATH PATH LD_LIBRARY_PATH EBUILD_PHASE \
             EBUILD_PHASE_FUNC \
             MERGE_TYPE EPREFIX ED EROOT SYSROOT ESYSROOT BROOT PORTAGE_CONFIGROOT USE \
             PORTAGE_INST_UID PORTAGE_INST_GID \
             REPLACING_VERSIONS REPLACED_BY_VERSION \
             MAKEOPTS CFLAGS CXXFLAGS CPPFLAGS LDFLAGS CC CXX AR RANLIB NM STRIP \
             INSDESTTREE EXEDESTTREE DOCDESTTREE DESTTREE _into_dir _insopts _exeopts \
             MOPREFIX ABI CONF_LIBDIR CHOST CBUILD CTARGET ARCH COLUMNS NOCOLOR NO_COLOR TERM",
        )
        .await
        .ok();

        // Source the ebuild — defines all phase functions and global variables —
        // only on the first phase of the package; later phases reuse the carried
        // environment (see `need_source` above).
        if need_source {
            let params = self.shell.default_exec_params();
            self.shell
                .source_script(
                    ebuild.path().as_std_path(),
                    std::iter::empty::<&str>(),
                    &params,
                )
                .await
                .map_err(|e| Error::Shell(format!("sourcing {}: {e}", ebuild.path())))?;

            // Combine eclass E_* contributions with ebuild-defined values (PMS 10.2).
            for (&var, &e_var) in accum_vars.iter().zip(e_vars.iter()) {
                let ebuild_val = self.get_var(var).unwrap_or_default();
                let eclass_val = self.get_var(e_var).unwrap_or_default();
                let combined = match (ebuild_val.is_empty(), eclass_val.is_empty()) {
                    (true, true) => String::new(),
                    (true, false) => eclass_val.trim().to_string(),
                    (false, true) => ebuild_val,
                    (false, false) => format!("{} {}", ebuild_val, eclass_val.trim()),
                };
                self.set_var(var, &combined);
                self.set_var(e_var, "");
            }

            self.phase_sourced_ebuild = Some(ebuild.path().to_path_buf());
        }

        // Compute $A from $SRC_URI for the active USE flag set.
        // $A is the space-separated list of distfile names needed by this ebuild.
        // It is a PM-provided variable (not computed in bash) and must be set
        // before any phase function runs (src_unpack reads it via ${A}).
        self.set_a_from_src_uri();
        self.run_string("export A").await.ok();

        // Wire up `default` and any missing EAPI default implementations
        // (e.g. src_compile → __eapi2_src_compile) for the current phase.
        // __ebuild_phase_funcs is a Rust builtin (not in funcs()), always run it.
        self.run_string(&format!("__ebuild_phase_funcs {eapi} {func_name}"))
            .await
            .ok();

        // Set the working directory for the phase.
        // src_unpack and pkg_nofetch run in $WORKDIR (archives are extracted there;
        // $S doesn't exist yet).  All other phases run in $S (the source tree).
        let cd_target = match func_name {
            "src_unpack" | "pkg_nofetch" => "\"${WORKDIR}\"",
            _ => "\"${S}\" 2>/dev/null || cd \"${WORKDIR}\" 2>/dev/null || true",
        };
        self.run_string(&format!("cd {cd_target}")).await.ok();

        // failglob is a *global-scope* requirement (PMS 6, Table 6.1): it
        // applies while sourcing the ebuild, not inside phase functions —
        // portage likewise strips it before running phases (an unmatched glob
        // in e.g. `dodoc CHANGES*` is the install helper's error to report,
        // not a shell abort).
        self.run_string("shopt -u failglob").await.ok();

        // Fresh die slate before bashrc + phase: clear stale flags from
        // metadata sourcing or a previous phase so they don't abort this one.
        // Must run *before* profile/user bashrc — those hooks (e.g.
        // `profiles/releases/23.0/profile.bashrc`'s merged-usr check) call
        // `die` and must be able to abort the phase. Previously the take()
        // lived *after* bashrc and discarded their die, so packages reported
        // Completed despite `die: ERROR: … merged-usr profile, but disk is
        // split-usr` (live 2026-08-07, 27 packages under --prefix --target).
        self.die_flag.take();

        // Source portage bashrc hooks (profile.bashrc files, then the user's
        // /etc/portage/bashrc) with the full phase environment available — not
        // PMS; matches portage's __source_all_bashrcs. A bashrc may define the
        // phase function or tweak the env (e.g. overlay search paths), so it
        // runs before the function is resolved below.
        if !self.bashrc_files.is_empty() {
            let mut script = String::new();
            for f in &self.bashrc_files {
                // __try_source: source if readable; bashrc is trusted code.
                script.push_str(&format!("[[ -r '{0}' ]] && source '{0}'\n", f.as_str()));
            }
            self.run_string(&script).await.ok();
            if let Some(msg) = self.die_flag.take() {
                return Err(Error::Shell(format!(
                    "bashrc (before {func_name}): die: {msg}"
                )));
            }
        }

        // Run the phase function (may have been defined by the ebuild or by
        // __ebuild_phase_funcs as a fallback calling default()).
        let phase_defined = self.shell.funcs().get(func_name).is_some();
        if phase_defined {
            // Kept alive until after the phase returns: dropping it closes the
            // pty and joins its reader, so the phase's output is fully drained
            // to the console and the log before anything else happens.
            let mut pty = None;
            let invocation = match &self.phase_log {
                Some((log, quiet)) => {
                    if let Some(parent) = log.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    let marker = format!(">>> {func_name}\n");
                    let _ = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(log)
                        .and_then(|mut f| std::io::Write::write_all(&mut f, marker.as_bytes()));
                    // A phase aborts only via `die` (the shared flag checked
                    // below), matching portage: the build helpers self-die on
                    // failure (`emake`/`econf`/`unpack`/the install helpers) and
                    // `eapply`/explicit `die` raise it directly. We deliberately
                    // do NOT treat the phase function's *trailing* exit status as
                    // fatal — many stock phases legitimately end on a non-zero
                    // command (e.g. binutils' `find … -exec rmdir {} +` to trim
                    // empty dirs), which portage tolerates.
                    if *quiet {
                        format!("{{ {func_name} ; }} >> {log} 2>&1")
                    } else {
                        // A pty on fd 1, so the phase and everything it spawns
                        // sees a real terminal — the only thing that satisfies
                        // `gentoo-functions`' per-call `[ -t 1 ]`, which no
                        // amount of TERM/COLUMNS can. `em`'s own reader takes
                        // the place of `tee`. Falls back when there is no
                        // console to render to, as portage falls back to a
                        // plain pipe.
                        pty = pty::PhasePty::open(log);
                        match &pty {
                            Some(pty) => {
                                format!("{{ {func_name} ; }} > {} 2>&1", pty.slave_path())
                            }
                            // The process-sub body may be polled after the
                            // phase (and even after the build tree is cleaned
                            // up); cd out of the cwd it cloned so the lazy
                            // `tee` spawn never starts from a deleted ${S}.
                            None => {
                                format!("{{ {func_name} ; }} > >(cd / && tee -a {log}) 2>&1")
                            }
                        }
                    }
                }
                None => func_name.to_string(),
            };
            let phase_result = self.run_string(&invocation).await;
            // Drain and close the pty before looking at how the phase went: a
            // failing phase's own output is the first thing anyone reads.
            drop(pty);
            phase_result?;
            // `die` aborts the phase even when it ran in a subshell or a
            // helper pipeline whose exit status the phase ignored.
            if let Some(msg) = self.die_flag.take() {
                return Err(Error::Shell(format!("{func_name}: die: {msg}")));
            }
        } else {
            tracing::warn!("{func_name} not defined, nothing to do");
        }

        Ok(())
    }

    /// Pre-parse every `.eclass` file in the shell's configured eclass directories
    /// into the shared AST cache.
    ///
    /// Calling this once before spawning workers guarantees 100% cache hits during
    /// ebuild processing, eliminating all per-worker parse work and any concurrent
    /// insert races.  Directories are searched in order; the first definition of
    /// each eclass name wins (same priority as `inherit`).
    /// Source an eclass by name.
    ///
    /// Searches the configured eclass directories in order.
    pub async fn source_eclass(&mut self, name: &str) -> Result<()> {
        self.invalidate_baseline();
        let filename = format!("{name}.eclass");
        for dir in &self.eclass_dirs {
            let path: Utf8PathBuf = dir.join(&filename);
            if path.is_file() {
                let params = self.shell.default_exec_params();
                self.shell
                    .source_script(path.as_std_path(), std::iter::empty::<&str>(), &params)
                    .await
                    .map_err(|e| Error::Shell(format!("sourcing eclass {name}: {e}")))?;
                return Ok(());
            }
        }
        Err(Error::Shell(format!("eclass not found: {name}")))
    }

    /// Source a `make.defaults` file.
    ///
    /// Variable assignments (with `${VAR}` expansion) are evaluated in the
    /// shell environment.
    ///
    /// See [PMS 5.2.4](https://projects.gentoo.org/pms/9/pms.html#makedefaults).
    pub async fn source_make_defaults(&mut self, path: &Path) -> Result<()> {
        self.invalidate_baseline();
        let params = self.shell.default_exec_params();
        self.shell
            .source_script(path, std::iter::empty::<&str>(), &params)
            .await
            .map_err(|e| Error::Shell(format!("sourcing make.defaults {}: {e}", path.display())))?;
        Ok(())
    }

    /// Source a per-package environment file (`/etc/portage/package.env` points
    /// at files under `/etc/portage/env/`).
    ///
    /// The file is a bash fragment of variable assignments (with `${VAR}`
    /// expansion, so `FEATURES="${FEATURES} ccache"` composes on top of the
    /// already-configured environment) sourced on top of `make.conf` for the
    /// package being built. Same evaluation as [`source_make_defaults`], named
    /// for clarity at the call site.
    ///
    /// [`source_make_defaults`]: Self::source_make_defaults
    pub async fn source_env_file(&mut self, path: &Path) -> Result<()> {
        self.invalidate_baseline();
        let params = self.shell.default_exec_params();
        self.shell
            .source_script(path, std::iter::empty::<&str>(), &params)
            .await
            .map_err(|e| Error::Shell(format!("sourcing env file {}: {e}", path.display())))?;
        Ok(())
    }

    /// Export every variable currently in the shell's environment, so a real
    /// subprocess an ebuild/eclass spawns directly (`bash
    /// "${FILESDIR}/gentoo.config"`, any raw `$(external-tool)`) inherits it —
    /// not just em's own Rust builtins (`econf`/`emake`), which read brush's
    /// variable table in-process and never needed export at all.
    ///
    /// Real portage doesn't hand-list what to export: `config.environ()`
    /// exports its *entire* settings dict minus a small internal denylist,
    /// because it builds the whole OS-level process environment before the
    /// ebuild's bash even starts. `em` instead `source`s make.conf/profile
    /// into an already-running brush shell as plain (non-exported)
    /// assignments — matching real bash `source` semantics, but meaning
    /// nothing reaches a real child process without an explicit `export`.
    /// Call this right after sourcing make.conf/profile/package.env (see
    /// `apply_profile_env` in `portage-cli/src/ebuild.rs`) so every
    /// profile-derived variable (`CHOST`, `ELIBC`, `MULTILIB_ABIS`,
    /// `DEFAULT_ABI`, …) is covered generically, instead of growing the
    /// hand-written identity-var list in `init_build_env` one silent
    /// breakage at a time (as happened with `CHOST` — invisible to
    /// `openssl`'s `gentoo.config` subshell, which fell back to `uname`
    /// autodetection and picked the *build host's* arch under `--cross`).
    ///
    /// Flips the export bit directly via brush's `ResolvedVarRefMut::
    /// base_var_mut` rather than round-tripping through a generated `export
    /// a b c …` string re-parsed by the interpreter — this is pure metadata
    /// mutation, not shell execution.
    pub fn export_sourced_env(&mut self) -> Result<()> {
        let names: Vec<String> = self
            .shell
            .env()
            .iter()
            .map(|(name, _)| name.clone())
            .filter(|name| !is_bash_internal_var(name))
            .collect();
        for name in &names {
            if let Some(mut var) = self.shell.env_mut().get_mut(name) {
                var.base_var_mut().export();
            }
        }
        Ok(())
    }

    /// Resolve the path of a named eclass by searching the configured eclass directories.
    pub fn eclass_path(&self, name: &str) -> Option<Utf8PathBuf> {
        let filename = format!("{name}.eclass");
        self.eclass_dirs
            .iter()
            .map(|dir| dir.join(&filename))
            .find(|p| p.is_file())
    }

    /// Read a variable from the shell environment.
    pub fn get_var(&self, name: &str) -> Option<String> {
        self.shell.env_str(name).map(|cow| cow.into_owned())
    }

    /// Snapshot all ebuild metadata variables into an `EbuildEnv`.
    ///
    /// Call this after [`source_ebuild`](Self::source_ebuild) to capture the
    /// stable per-package metadata before running any build phases.
    pub fn collect_env(&self) -> crate::build::env::EbuildEnv {
        let get = |name: &str| -> String { self.get_var(name).unwrap_or_default() };
        let get_opt =
            |name: &str| -> Option<String> { self.get_var(name).filter(|s| !s.is_empty()) };
        let split = |name: &str| -> Vec<String> {
            get(name).split_whitespace().map(str::to_owned).collect()
        };

        crate::build::env::EbuildEnv {
            eapi: get("EAPI"),
            slot: {
                let s = get("SLOT");
                if s.is_empty() { "0".to_string() } else { s }
            },
            iuse: split("IUSE"),
            use_flags: split("USE"),
            keywords: split("KEYWORDS"),
            description: get("DESCRIPTION"),
            homepage: get_opt("HOMEPAGE"),
            license: get_opt("LICENSE"),
            restrict: get_opt("RESTRICT"),
            properties: get_opt("PROPERTIES"),
            depend: get_opt("DEPEND"),
            rdepend: get_opt("RDEPEND"),
            bdepend: get_opt("BDEPEND"),
            pdepend: get_opt("PDEPEND"),
            idepend: get_opt("IDEPEND"),
            // DEFINED_PHASES is computed by portage (not set by the ebuild) — derive
            // it from which phase functions the sourced ebuild defines (PMS 7.4),
            // matching the metadata path's computation.
            defined_phases: {
                let mut p: Vec<String> = PHASE_FUNCTIONS
                    .iter()
                    .filter(|(name, _)| self.shell.funcs().get(name).is_some())
                    .map(|(_, phase)| phase.as_str().to_owned())
                    .collect();
                p.sort();
                p
            },
            // The repo name lives in `<repo>/profiles/repo_name`, not a shell var.
            repository: std::fs::read_to_string(
                self.repo_path.join("profiles/repo_name").as_std_path(),
            )
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()),
            inherited: split("INHERITED"),
            features: get_opt("FEATURES"),
            chost: get_opt("CHOST"),
            cbuild: get_opt("CBUILD"),
            cflags: get_opt("CFLAGS"),
            cxxflags: get_opt("CXXFLAGS"),
            ldflags: get_opt("LDFLAGS"),
            rustflags: get_opt("RUSTFLAGS"),
        }
    }

    /// Set a variable in the shell environment.
    fn set_var(&mut self, name: &str, value: &str) {
        let _ = self.shell.set_env_global(
            name,
            ShellVariable::new(ShellValue::String(value.to_string())),
        );
    }

    /// Run a bash script string directly in the shell without writing a temporary file.
    pub async fn run_string(&mut self, script: &str) -> Result<()> {
        let params = self.shell.default_exec_params();
        let source_info = SourceInfo::from("inline");
        self.shell
            .run_string(script, &source_info, &params)
            .await
            .map_err(|e| Error::Shell(format!("run_string: {e}")))?;
        Ok(())
    }

    /// Set the active USE flags for this shell session.
    ///
    /// These flags will be used by the `use()`, `usev()`, `usex()` functions
    /// when sourcing ebuilds and eclasses.
    ///
    /// # Example
    /// ```no_run
    /// use portage_repo::Repository;
    ///
    /// # async fn example() {
    /// let repo = Repository::builder().in_memory_cache().open("/var/db/repos/gentoo").unwrap();
    /// let mut shell = repo.shell().await.unwrap();
    /// shell.set_use_flags(&["ssl", "gtk", "-doc"]).unwrap();
    /// # }
    /// ```
    pub fn set_use_flags(&mut self, flags: &[&str]) -> Result<()> {
        self.invalidate_baseline();
        let mut new_flags = HashSet::new();

        for flag in flags {
            let flag_str = flag.trim();
            if flag_str.is_empty() {
                continue;
            }

            let (flag_name, enabled) = if let Some(stripped) = flag_str.strip_prefix('-') {
                (stripped.to_string(), false)
            } else if let Some(stripped) = flag_str.strip_prefix('+') {
                (stripped.to_string(), true)
            } else {
                (flag_str.to_string(), true)
            };

            if enabled {
                new_flags.insert(flag_name);
            } else {
                new_flags.remove(&flag_name);
            }
        }

        self.use_flags = new_flags;

        // Update the USE environment variable
        let use_flags = self.use_flags_string();
        if !use_flags.is_empty() {
            self.set_var("USE", &use_flags);
        } else {
            self.set_var("USE", "");
        }

        Ok(())
    }

    /// Get the current USE flags as a space-separated string.
    ///
    /// This can be used to set the `USE` environment variable in the shell.
    pub fn use_flags_string(&self) -> String {
        let mut flags: Vec<_> = self.use_flags.iter().cloned().collect();
        flags.sort();
        flags.join(" ")
    }

    /// Resolve the distfiles location: the writable primary plus read-only
    /// fallbacks (`PORTAGE_RO_DISTDIRS`). Order: `$DISTDIR` from the
    /// environment; else the system `/var/cache/distfiles` when writable;
    /// else `~/.cache/distfiles` (created), with the unwritable system
    /// directory kept as a read-only fallback so already-fetched files are
    /// still found by `fetch`-presence checks and `unpack`.
    fn resolved_distdir() -> (String, Vec<String>) {
        const SYSTEM: &str = "/var/cache/distfiles";
        let writable = |dir: &str| {
            std::fs::create_dir_all(dir).is_ok() && {
                let probe = std::path::Path::new(dir)
                    .join(format!(".em-write-probe-{}", std::process::id()));
                let ok = std::fs::write(&probe, b"").is_ok();
                let _ = std::fs::remove_file(&probe);
                ok
            }
        };
        let mut ro: Vec<String> = std::env::var("PORTAGE_RO_DISTDIRS")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let primary = if let Ok(dir) = std::env::var("DISTDIR").map(|d| d.trim().to_string())
            && !dir.is_empty()
        {
            std::fs::create_dir_all(&dir).ok();
            dir
        } else if writable(SYSTEM) {
            SYSTEM.to_string()
        } else {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            let dir = format!("{home}/.cache/distfiles");
            std::fs::create_dir_all(&dir).ok();
            dir
        };
        if primary != SYSTEM && std::path::Path::new(SYSTEM).is_dir() {
            ro.push(SYSTEM.to_string());
        }
        (primary, ro)
    }

    /// Compute `$A` from `$SRC_URI` and inject it into the shell environment.
    ///
    /// `$A` is a PM-provided variable containing the space-separated list of
    /// distfile names required by the ebuild for the currently active USE flags.
    /// It is computed by the PM (not in bash) and must be set before any phase
    /// function runs, since `src_unpack` and `pkg_nofetch` iterate `${A}`.
    ///
    /// USE-conditional groups (`flag? ( ... )`) are evaluated against
    /// `Self::use_flags`; unconditional files are always included.
    pub fn set_a_from_src_uri(&mut self) {
        let src_uri = self.get_var("SRC_URI").unwrap_or_default();
        if src_uri.is_empty() {
            self.set_var("A", "");
            return;
        }
        let Ok(entries) = SrcUriEntry::parse(&src_uri) else {
            self.set_var("A", "");
            return;
        };
        let use_flags = self.use_flags.clone();
        let mut files: Vec<String> = Vec::new();
        collect_src_filenames(&entries, &use_flags, &mut files);
        self.set_var("A", &files.join(" "));
    }

    /// Extract metadata from shell variables into a `CacheEntry`-compatible string
    /// and parse it via portage-metadata.
    fn extract_metadata(&self) -> Result<EbuildMetadata> {
        // Collect (key, value) pairs directly from the shell environment,
        // using Cow<str> to avoid cloning when the value needs no normalization.
        let pairs: Vec<(&str, std::borrow::Cow<str>)> = METADATA_VARS
            .iter()
            .filter_map(|&var| {
                let value = self.shell.env_str(var)?;
                if value.is_empty() {
                    return None;
                }
                // Normalize embedded newlines/tabs to spaces (heredoc values).
                let normalized = if var == "DESCRIPTION"
                    || value.bytes().any(|b| matches!(b, b'\n' | b'\r' | b'\t'))
                {
                    std::borrow::Cow::Owned(itertools::join(value.split_whitespace(), " "))
                } else {
                    value
                };
                if normalized.is_empty() {
                    return None;
                }
                Some((var, normalized))
            })
            .collect();

        let entry = portage_metadata::CacheEntry::from_kv_pairs(
            pairs.iter().map(|(k, v)| (*k, v.as_ref())),
        )?;

        // Compute DEFINED_PHASES by inspecting which phase functions are
        // defined in the shell after sourcing (PMS 7.4).
        let mut defined_phases: Vec<Phase> = PHASE_FUNCTIONS
            .iter()
            .filter(|(name, _)| self.shell.funcs().get(name).is_some())
            .map(|(_, phase)| *phase)
            .collect();
        // Sort alphabetically by short name to match Portage's cache format.
        defined_phases.sort_by_key(|p| p.as_str());

        let mut metadata = entry.metadata;
        metadata.defined_phases = defined_phases;
        Ok(metadata)
    }
}

/// Recursively collect distfile names from a parsed `SRC_URI` tree.
/// Remove GNU make jobserver tokens from a MAKEFLAGS string.
///
/// `--jobserver-auth=R,W` (make ≥ 4.2) and `--jobserver-fds=R,W` (older
/// make) encode file-descriptor numbers that are only valid inside the make
/// process tree that created them.  Any other process that inherits MAKEFLAGS
/// and tries to open those fds will get EBADF or hit a completely unrelated
/// fd.  Strip them unconditionally at build-env initialisation time.
fn strip_jobserver_tokens(flags: &str) -> String {
    let cleaned: Vec<&str> = flags
        .split_whitespace()
        .filter(|tok| !tok.starts_with("--jobserver-auth=") && !tok.starts_with("--jobserver-fds="))
        .collect();
    cleaned.join(" ")
}

/// Whether `name` resolves to a file on `$PATH` (a `type -p`-style lookup, used
/// to gate cross-toolchain selection on `${CHOST}-gcc` actually existing).
fn program_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// Directories listed in an `ld.so.conf`-format file (as written by
/// `env_update`, `maint/env.rs`, or shipped by glibc itself), `include`
/// directives correctly glob-expanded. `env_update` writes fully-qualified,
/// already-prefixed paths (confirmed live: `/opt/pfx2/usr/lib64`, not bare
/// `/usr/lib64`), so — unlike `maint/env.rs`'s own `refresh_ld_cache`, which
/// re-roots a *relative* default list under a root — no prefix re-rooting
/// applies here. Best-effort: an unreadable/missing file (e.g. a fresh
/// prefix's very first phase, before any `env_update` has run) silently
/// yields no directories, same spirit as `refresh_ld_cache`.
/// Bash/brush-internal names [`export_sourced_env`](EbuildShell::export_sourced_env)
/// must skip: dynamic/magic variables bash manages itself (exporting them is
/// either a no-op, nonsensical, or — for the readonly ones — a hard error that
/// would abort the whole `export` statement).
pub(crate) fn is_bash_internal_var(name: &str) -> bool {
    matches!(
        name,
        "_" | "PIPESTATUS"
            | "FUNCNAME"
            | "LINENO"
            | "RANDOM"
            | "SECONDS"
            | "PPID"
            | "SHELLOPTS"
            | "BASHOPTS"
            | "GROUPS"
            | "HISTCMD"
    ) || name.starts_with("BASH_")
}

///
/// USE-conditional groups are evaluated against `use_flags`; unconditional
/// files are always appended.  The `->` arrow rename case is handled by
/// the `Renamed` variant (target filename is used, not the source URL).
fn collect_src_filenames(
    entries: &[SrcUriEntry],
    use_flags: &HashSet<String>,
    files: &mut Vec<String>,
) {
    for entry in entries {
        match entry {
            SrcUriEntry::Uri { filename, .. } => files.push(filename.clone()),
            SrcUriEntry::Renamed { target, .. } => files.push(target.clone()),
            SrcUriEntry::UseConditional {
                flag,
                negated,
                entries,
            } => {
                let flag_set = use_flags.contains(flag.as_str());
                // Include when: (not negated AND flag set) OR (negated AND flag not set).
                if flag_set != *negated {
                    collect_src_filenames(entries, use_flags, files);
                }
            }
            SrcUriEntry::Group(entries) => {
                collect_src_filenames(entries, use_flags, files);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Repository shell factory methods — live here so repo/ stays brush-free.
// ---------------------------------------------------------------------------

impl Repository {
    /// Create an [`EbuildShell`] configured for this repository.
    pub async fn shell(&self) -> Result<EbuildShell> {
        EbuildShell::new(self).await
    }

    /// Create an [`EbuildShell`] with master repository eclass directories.
    ///
    /// The repo's own `eclass/` is searched first; masters follow as fallback,
    /// in reverse `masters` order. This matches portage, where a repo's own
    /// eclass overrides its masters' and later masters override earlier ones
    /// (`eclass_locations = [master1, …, masterN, own]`, last-writer-wins).
    /// See [PMS 4.7](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub async fn shell_with_masters(&self, masters: &[&Repository]) -> Result<EbuildShell> {
        let mut shell = EbuildShell::new(self).await?;
        for master in masters.iter().rev() {
            let dir = master.path().join("eclass");
            if dir.is_dir() {
                shell.add_eclass_dir(dir);
            }
        }
        Ok(shell)
    }

    /// Like [`shell_with_masters`](Self::shell_with_masters) but shares an
    /// eclass AST cache across all created shells.
    pub async fn shell_with_masters_and_cache(
        &self,
        masters: &[&Repository],
        ctx: &SourceContext,
    ) -> Result<EbuildShell> {
        let mut shell = EbuildShell::new_with_cache(self, ctx).await?;
        for master in masters.iter().rev() {
            let dir = master.path().join("eclass");
            if dir.is_dir() {
                shell.add_eclass_dir(dir);
            }
        }
        Ok(shell)
    }

    /// Create an [`EbuildShell`] with a profile's USE configuration applied.
    ///
    /// `profile_rel_path` is relative to the repository's `profiles/` directory.
    /// `make_conf` is an optional `make.conf`-style script sourced after the
    /// profile chain but before `use.force`/`use.mask`.
    ///
    /// See [PMS 5.2](https://projects.gentoo.org/pms/9/pms.html#profiles).
    pub async fn shell_with_profile(
        &self,
        profile_rel_path: &str,
        make_conf: Option<&std::path::Path>,
    ) -> Result<EbuildShell> {
        use crate::repo::profile::ProfileStack;
        let path = self.path().join("profiles").join(profile_rel_path);
        let stack = ProfileStack::build(path.into())?;
        let mut shell = EbuildShell::new(self).await?;
        let confs: Vec<&std::path::Path> = make_conf.into_iter().collect();
        stack.configure_shell(&mut shell, &confs).await?;
        Ok(shell)
    }
}

#[cfg(test)]
mod tests;
