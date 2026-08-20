//! crossdev's `load_multilib_env`, ported
//!
//! The cross package `package.env` needs the per-ABI multilib tables so the libc
//! builds for the *target* ABI (e.g. `-mabi=lp64d -march=rv64gc`) instead of
//! inheriting the host `make.conf` CFLAGS (`-mcpu=ampere1a …`), which the libc's
//! flag stripping then reduces to no optimisation — glibc fails with "cannot be
//! compiled without optimization". multilib.eclass is the authority on each
//! arch's ABIs, so — exactly like crossdev — we run its `multilib_env` for a
//! tuple via bash and capture what it emits rather than hardcoding arch tables.

use std::collections::BTreeMap;
use std::process::Command;

use anyhow::{Context, Result, bail};
use camino::Utf8Path;

/// The per-ABI multilib variables multilib.eclass computes for one tuple
pub struct MultilibEnv {
    /// `CFLAGS_<abi>` / `CHOST_<abi>` / `CTARGET_<abi>` / `LDFLAGS_<abi>` /
    /// `LIBDIR_<abi>`, keyed by name.
    vars: BTreeMap<String, String>,
    /// The active ABI list (space-separated) and the default ABI
    multilib_abis: String,
    default_abi: String,
}

impl MultilibEnv {
    /// The first (primary) ABI — crossdev's `TARGET_ABI`
    fn primary_abi(&self) -> &str {
        self.multilib_abis
            .split_whitespace()
            .next()
            .unwrap_or(&self.default_abi)
    }

    /// The default ABI (`DEFAULT_ABI`)
    pub fn default_abi(&self) -> &str {
        &self.default_abi
    }

    /// `LIBDIR_<abi>` for `abi`, if multilib.eclass defined one
    pub fn libdir(&self, abi: &str) -> Option<&str> {
        self.vars.get(&format!("LIBDIR_{abi}")).map(String::as_str)
    }
}

/// Run multilib.eclass's `multilib_env` for `tuple` (crossdev's
/// `load_multilib_env`), returning the per-ABI tables it computes. Shells out to
/// bash exactly as crossdev does — the eclass owns each arch's ABI definitions.
pub fn query(tuple: &str, eclass_dir: &Utf8Path) -> Result<MultilibEnv> {
    let eclass = eclass_dir.join("multilib.eclass");
    if !eclass.is_file() {
        bail!("multilib.eclass not found at {eclass}");
    }
    // Mirrors crossdev's load_multilib_env subshell: stub `inherit`, source the
    // eclass, then run `multilib_env`. The `single_abi` dance collapses the
    // "default" sentinel to the concrete DEFAULT_ABI. CTARGET must be set as a
    // plain assignment *before* sourcing multilib.eclass (not as a one-shot
    // prefix to `multilib_env`): the eclass reads CTARGET at source time to
    // synthesize the `*_default` ABI entries (CHOST_default, CTARGET_default,
    // LIBDIR_default) that multilib-toolchain.eclass relies on. Without them,
    // a cross glibc build resolves CC to the host gcc and dies on the
    // linux-headers version probe. Matches crossdev's `load_multilib_env`.
    let snippet = format!(
        r#"
CTARGET={tuple}
unset ${{!CFLAGS_*}} ${{!CHOST_*}} ${{!CTARGET_*}} ${{!LDFLAGS_*}} ${{!LIBDIR_*}}
inherit() {{ :; }}
die() {{ echo "die: $*" >&2; exit 1; }}
EAPI=7 . "{eclass}" || exit 1
unset DEFAULT_ABI
if [[ ${{MULTILIB_ABIS}} == default ]]; then unset MULTILIB_ABIS; single_abi=true; else single_abi=false; fi
multilib_env "{tuple}"
${{single_abi}} && MULTILIB_ABIS=${{DEFAULT_ABI}}
for v in ${{!CFLAGS_*}} ${{!CHOST_*}} ${{!CTARGET_*}} ${{!LDFLAGS_*}} ${{!LIBDIR_*}}; do
    printf '%s=%s\n' "$v" "${{!v}}"
done
# crossdev: make sure every active ABI has CFLAGS/LIBDIR/LDFLAGS defined.
def_CFLAGS= def_LIBDIR=lib def_LDFLAGS=
for vv in CFLAGS LIBDIR LDFLAGS; do
    d="def_${{vv}}"
    for a in ${{MULTILIB_ABIS}}; do
        _v="${{vv}}_${{a}}"
        [[ ${{!_v+set}} == set ]] && continue
        printf '%s=%s\n' "${{_v}}" "${{!d}}"
    done
done
printf 'MULTILIB_ABIS=%s\n' "${{MULTILIB_ABIS}}"
printf 'DEFAULT_ABI=%s\n' "${{DEFAULT_ABI}}"
"#
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&snippet)
        .output()
        .context("running bash to evaluate multilib_env")?;
    if !out.status.success() {
        bail!(
            "multilib_env for {tuple} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    let mut vars = BTreeMap::new();
    let mut multilib_abis = String::new();
    let mut default_abi = String::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k {
            "MULTILIB_ABIS" => multilib_abis = v.to_string(),
            "DEFAULT_ABI" => default_abi = v.to_string(),
            _ => {
                vars.insert(k.to_string(), v.to_string());
            }
        }
    }
    if default_abi.is_empty() {
        bail!("multilib_env for {tuple} produced no DEFAULT_ABI");
    }
    Ok(MultilibEnv {
        vars,
        multilib_abis,
        default_abi,
    })
}

/// The cross `package.env` body for one package — crossdev's `set_env`
///
/// The per-ABI tables of `host` and `target` are merged (every file carries both); the
/// unprefixed `ABI`/`MULTILIB_ABIS`/`DEFAULT_ABI` is the **target's** for a target package
/// (libc/headers/runtimes — code that runs on `<CTARGET>`) and the **host's** (plus
/// `TARGET_*`) for a host tool (binutils/gcc/the clang wrapper).
pub fn env_block(host: &MultilibEnv, target: &MultilibEnv, target_package: bool) -> String {
    let mut merged = host.vars.clone();
    merged.extend(target.vars.iter().map(|(k, v)| (k.clone(), v.clone())));

    let mut body = String::new();
    for (k, v) in &merged {
        body.push_str(&format!("{k}='{v}'\n"));
    }
    if target_package {
        body.push_str(&format!("ABI='{}'\n", target.primary_abi()));
        body.push_str(&format!("MULTILIB_ABIS='{}'\n", target.multilib_abis));
        body.push_str(&format!("DEFAULT_ABI='{}'\n", target.default_abi));
    } else {
        body.push_str(&format!("TARGET_ABI='{}'\n", target.primary_abi()));
        body.push_str(&format!(
            "TARGET_MULTILIB_ABIS='{}'\n",
            target.multilib_abis
        ));
        body.push_str(&format!("TARGET_DEFAULT_ABI='{}'\n", target.default_abi));
        let host_abi = host.primary_abi();
        body.push_str(&format!("ABI='{host_abi}'\n"));
        body.push_str(&format!("MULTILIB_ABIS='{host_abi}'\n"));
        body.push_str(&format!("DEFAULT_ABI='{host_abi}'\n"));
    }
    body
}
