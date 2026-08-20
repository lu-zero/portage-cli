//! Rendering for `em use` — colorized `VAR="..."` lines and old→new diffs,
//! reusing [`colorize_use_flag`], the same bold red/blue enabled/disabled
//! palette `-pv` output and `em --info` already use, so all three read the
//! same way. The variable name itself (`USE`, `VIDEO_CARDS`, …) is plain
//! green ([`crate::style::C_LABEL`]), matching how every other applet colors
//! a field label.

use std::io::Write as _;

use anyhow::{Context, Result};
use camino::Utf8Path;
use portage_repo::MakeConf;

use crate::cli;
use crate::query::depgraph::output::colorize_use_flag;
use crate::style::C_LABEL;

/// Colorize a raw, space-separated USE-shaped string via [`colorize_use_flag`]
fn colorize_flags(raw: &str) -> String {
    raw.split_whitespace()
        .map(colorize_use_flag)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Print the current `VAR="..."` line at `path`, colorized
pub(super) fn show(path: &Utf8Path, var: &str) -> Result<()> {
    let mc = MakeConf::load(path).with_context(|| format!("reading {path}"))?;

    let mut out = anstream::stdout();
    match mc.get(var) {
        Some(val) => writeln!(out, "{C_LABEL}{var}{C_LABEL:#}=\"{}\"", colorize_flags(val)),
        None => writeln!(out, "{C_LABEL}{var}{C_LABEL:#} not set in {path}"),
    }
    .ok();
    Ok(())
}

/// Print `VAR="old" -> "new"`, each side colorized, so a write (or
/// `--dry-run` preview) shows the resulting value the same way it will read
/// back.
pub(super) fn print_diff(var: &str, old_value: &str, new_value: &str) {
    let mut out = anstream::stdout();
    if old_value == new_value {
        writeln!(
            out,
            "{C_LABEL}{var}{C_LABEL:#}=\"{}\" (unchanged)",
            colorize_flags(new_value)
        )
        .ok();
        return;
    }
    writeln!(
        out,
        "{C_LABEL}{var}{C_LABEL:#}=\"{}\"",
        colorize_flags(old_value)
    )
    .ok();
    writeln!(out, "  -> \"{}\"", colorize_flags(new_value)).ok();
}

/// Which `profiles/use*.desc` table(s) an `-i` lookup searches — mirrors
/// euse's `$SCOPE` (`-g`/`-l-desc`; neither searches both, global first,
/// matching euse's own unset-scope behavior).
#[derive(Clone, Copy)]
enum InfoScope {
    Both,
    Global,
    Local,
}

impl InfoScope {
    fn from_flags(global: bool, local: bool) -> Self {
        match (global, local) {
            (true, false) => InfoScope::Global,
            (false, true) => InfoScope::Local,
            _ => InfoScope::Both,
        }
    }

    fn wants_global(self) -> bool {
        !matches!(self, InfoScope::Local)
    }

    fn wants_local(self) -> bool {
        !matches!(self, InfoScope::Global)
    }
}

/// Show descriptions for `flags` from the active repo's `profiles/use.desc`
/// (global) and `use.local.desc` (per-package, searched across every
/// package — like euse, a local match is not limited to one package). With
/// `flags` empty, lists every known flag in scope instead (euse's own
/// `args="${*:-*}"` default). `global`/`local` narrow the search to just one
/// side; with neither, both are searched, global first.
pub(super) fn show_info(cli: &cli::Cli, flags: &[String], global: bool, local: bool) -> Result<()> {
    let repo = crate::crossdev::main_repo(cli).context("opening main repo")?;
    let use_db = repo.use_db().unwrap_or_default();
    let scope = InfoScope::from_flags(global, local);

    let mut out = anstream::stdout();

    if flags.is_empty() {
        if scope.wants_global() {
            for (flag, desc) in use_db.global() {
                writeln!(out, "{C_LABEL}{flag}{C_LABEL:#}  {desc}").ok();
            }
        }
        if scope.wants_local() {
            for cpn in use_db.packages_with_local_flags() {
                let Some(local_flags) = use_db.local_flags(cpn) else {
                    continue;
                };
                for (flag, desc) in local_flags {
                    writeln!(out, "{C_LABEL}{cpn}:{flag}{C_LABEL:#}  {desc}").ok();
                }
            }
        }
        return Ok(());
    }

    for flag in flags {
        let mut found = false;
        if scope.wants_global()
            && let Some(desc) = use_db.describe_global(flag)
        {
            writeln!(out, "{C_LABEL}{flag}{C_LABEL:#}  {desc}").ok();
            found = true;
        }
        if scope.wants_local() {
            for cpn in use_db.packages_with_local_flags() {
                if let Some(desc) = use_db.local_flags(cpn).and_then(|m| m.get(flag)) {
                    writeln!(out, "{C_LABEL}{cpn}:{flag}{C_LABEL:#}  {desc}").ok();
                    found = true;
                }
            }
        }
        if !found {
            writeln!(out, "{C_LABEL}{flag}{C_LABEL:#}  (no description)").ok();
        }
    }
    Ok(())
}

/// Sorted, uppercased USE_EXPAND variable names the active profile actually
/// declares — resolved from the profile stack, so a variable the repo
/// merely has a `profiles/desc/*.desc` file for but no active profile pulls
/// in is left out. Falls back to every desc-file name in the repo when no
/// profile resolves at all, so the listing degrades rather than going empty.
async fn expand_var_names(cli: &cli::Cli) -> Result<Vec<String>> {
    let repo = crate::crossdev::main_repo(cli).context("opening main repo")?;
    let roots = cli.roots();

    let mut shell = repo.shell().await.context("creating shell")?;
    let has_profile =
        crate::ebuild::apply_profile_env(&mut shell, roots.config(), roots.config_overlay())
            .await
            .context("resolving active profile")?;

    let mut names: Vec<String> = if has_profile {
        shell
            .get_var("USE_EXPAND")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_uppercase)
            .collect()
    } else {
        repo.use_expand_names()
            .context("reading USE_EXPAND variable names")?
            .into_iter()
            .map(|n| n.to_uppercase())
            .collect()
    };
    names.sort_unstable();
    names.dedup();
    Ok(names)
}

/// List every USE_EXPAND variable from [`expand_var_names`], each with its
/// current make.conf value if set.
pub(super) async fn list_expand(cli: &cli::Cli, path: &Utf8Path) -> Result<()> {
    let vars = expand_var_names(cli).await?;
    let mc = MakeConf::load(path).with_context(|| format!("reading {path}"))?;
    let mut out = anstream::stdout();
    for var in vars {
        match mc.get(&var) {
            Some(val) if !val.trim().is_empty() => {
                writeln!(out, "{C_LABEL}{var}{C_LABEL:#}=\"{}\"", colorize_flags(val)).ok();
            }
            _ => {
                writeln!(out, "{C_LABEL}{var}{C_LABEL:#} (unset)").ok();
            }
        };
    }
    Ok(())
}

/// Bare `em use` (no flags at all): show `USE`, then every USE_EXPAND
/// variable from [`expand_var_names`] that currently has a value in
/// make.conf — skipping unset ones (use `-L`/`--list-expand` to browse every
/// known variable, set or not).
pub(super) async fn show_summary(cli: &cli::Cli, path: &Utf8Path) -> Result<()> {
    show(path, "USE")?;

    let vars = expand_var_names(cli).await?;
    let mc = MakeConf::load(path).with_context(|| format!("reading {path}"))?;
    let mut out = anstream::stdout();
    for var in vars {
        if let Some(val) = mc.get(&var)
            && !val.trim().is_empty()
        {
            writeln!(out, "{C_LABEL}{var}{C_LABEL:#}=\"{}\"", colorize_flags(val)).ok();
        }
    }
    Ok(())
}
