//! Portage-specific bash function definitions for the embedded shell
//!
//! Real ebuilds and eclasses expect a set of Portage-provided functions
//! (`inherit`, `die`, `EXPORT_FUNCTIONS`, etc.) to exist at source time.
//! Rather than implementing each as a Rust builtin, we define them as
//! bash shell functions via [`brush_core::Shell::run_string`].
//!
//! See [PMS 10](https://projects.gentoo.org/pms/9/pms.html#eclasses)
//! and [PMS 12](https://projects.gentoo.org/pms/9/pms.html#available-commands) for the
//! functions an ebuild/eclass may call.

use brush_core::{Shell, SourceInfo};

use crate::error::{Error, Result};

/// Register all Portage-specific shell functions in the given shell
///
/// This must be called once during [`crate::EbuildShell::new`] before
/// any ebuild or eclass is sourced.
pub(crate) async fn register(shell: &mut Shell) -> Result<()> {
    let params = shell.default_exec_params();
    let source_info = SourceInfo::from("portage-builtins");
    shell
        .run_string(PORTAGE_FUNCTIONS, &source_info, &params)
        .await
        .map_err(|e| Error::Shell(format!("registering portage builtins: {e}")))?;
    Ok(())
}

/// All Portage-specific bash function definitions
///
/// Concatenated into a single script that is evaluated once at shell init
/// time.
const PORTAGE_FUNCTIONS: &str = r#"
# ── Bash options required by PMS / Portage ───────────────────────────
# Portage's ebuild.sh enables these before sourcing any ebuild or eclass.
# extglob is required for many eclasses; nullglob and dotglob are also set.
shopt -s extglob
shopt -s nullglob
shopt -s dotglob

# ── Tier 1: critical for eclass/ebuild sourcing ──────────────────────

# die — implemented as a Rust builtin (pms_builtins.rs)

# nonfatal — implemented as a Rust builtin (commands::NonfatalCommand,
# registered unconditionally in shell.rs). A bash function of the same name
# always shadows a same-named builtin in this shell (same class of bug as
# eapply's — see todo/eapply-stub-shadows-real-builtin.md), and the real
# builtin's job is to scope PORTAGE_NONFATAL=1 around its argument so
# econf/emake/die's `-n` path can see it; a plain "$@" stub never sets that,
# so `nonfatal econf ...` never actually suppressed the self-die it exists
# for. It's also phase-body-only like eapply/econf/emake — never reachable
# during metadata-only sourcing — so unlike Tier 2 below, it needs no stub
# at all rather than an unset -f entry.

# EXPORT_FUNCTIONS — implemented as a Rust builtin (pms_builtins.rs)

# ── Tier 2: called at eclass source time ─────────────────────────────

# Debug output (no-ops for metadata extraction)
debug-print()          { :; }
debug-print-function() { :; }
debug-print-section()  { :; }

# einfo/einfon/ewarn/eerror/elog/eqawarn/ebegin/eend, has_version/
# best_version, econf/emake/einstall/unpack/eapply, docompress/dostrip, and
# the do*/new* install helpers are DUAL_MODE builtins — see
# commands::dual_mode::set_tool_mode. No bash stub here; that mechanism
# registers a real Rust no-op builtin for metadata mode instead, so there
# is nothing for the real builtin to ever get shadowed by.

eapply_user() { :; }
default() { :; }
default_src_unpack()    { :; }
default_src_prepare()   { :; }
default_src_configure() { :; }
default_src_compile()   { :; }
default_src_install()   { :; }
default_src_test()      { :; }

# Directory / option setters
into()     { :; }
insinto()  { :; }
exeinto()  { :; }
docinto()  { :; }
insopts()  { :; }
exeopts()  { :; }

# Install commands with no Rust builtin (permanent no-ops)
doinitd()   { :; }
doconfd()   { :; }
edo()        { :; }

# Unprivileged install tolerance (no fakeroot): eclasses run `chown 0:0`/`chgrp`
# in src_install (e.g. toolchain.eclass `chown -R 0:0 "${LIBPATH}" || die`). As
# non-root that fails with EPERM and aborts the build, but for a user-owned
# Gentoo Prefix install root ownership is meaningless. Attempt the real command;
# tolerate failure only when we are not root (mirroring fakeroot), so a genuine
# privileged-build error still propagates. `id -u` runs only on failure.
chown() { command chown "$@" || { [[ ${EUID:-$(id -u)} -ne 0 ]] && return 0; return 1; }; }
chgrp() { command chgrp "$@" || { [[ ${EUID:-$(id -u)} -ne 0 ]] && return 0; return 1; }; }
"#;
