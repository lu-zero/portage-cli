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

# nonfatal: run command, ignore failure
nonfatal() { "$@"; return 0; }


# EXPORT_FUNCTIONS — implemented as a Rust builtin (pms_builtins.rs)

# ── Tier 2: called at eclass source time ─────────────────────────────

# Debug output (no-ops for metadata extraction)
debug-print()          { :; }
debug-print-function() { :; }
debug-print-section()  { :; }

# User output (no-ops for metadata extraction)
einfo()   { :; }
einfon()  { :; }
ewarn()   { :; }
eerror()  { :; }
elog()    { :; }
eqawarn() { :; }
ebegin()  { :; }
eend()    { return "${1:-0}"; }

# ── Tier 3: has / use / in_iuse — implemented as Rust builtins ───────
# (registered in shell.rs via pms_builtins.rs)

# ── Tier 4: package query stubs ──────────────────────────────────────

has_version()  { return 1; }
best_version() { echo ""; return 1; }

# ── Tier 6: build/install command stubs ──────────────────────────────

econf()   { :; }
emake()   { :; }
einstall() { :; }
unpack()  { :; }
eapply()  { :; }
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

# Install commands
dobin()     { :; }
newbin()    { :; }
dosbin()    { :; }
newsbin()   { :; }
doins()     { :; }
newins()    { :; }
doexe()     { :; }
newexe()    { :; }
dolib.a()   { :; }
dolib.so()  { :; }
dodir()     { :; }
keepdir()   { :; }
dodoc()     { :; }
newdoc()    { :; }
doman()     { :; }
newman()    { :; }
doheader()  { :; }
newheader() { :; }
dosym()     { :; }
doinitd()   { :; }
doconfd()   { :; }
fperms()    { :; }
fowners()   { :; }
docompress() { :; }
dostrip()    { :; }
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
