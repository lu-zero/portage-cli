//! `em select profile` — cross-aware `eselect profile` workalike.
//!
//! `list` / `show` / `set`. The cross-aware difference: `set` accepts **any**
//! profile path (or a list number) and links it *without* validating it against
//! the host architecture — `eselect profile` refuses a foreign-arch profile,
//! which is exactly what a cross sysroot needs.
//!
//! Target the sysroot with the global `--config-root`
//! (e.g. `em --config-root /usr/<CTARGET> select profile set <path>`).
// Design note: profile-linking for cross targets is documented in
// todo/crossdev-target.md.

use std::io::Write as _;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::{MakeConf, ProfileDesc, ReposConf, Repository};

use super::config_portage_dir;
use crate::cli::{Cli, ProfileAction};
use crate::style::{C_STAR, profile_status};

/// The architecture profiles are filtered to: `ARCH` from the effective
/// `make.conf` (a cross sysroot may pin it), else the global `--arch` (the host
/// by default).
fn effective_arch(globals: &Cli) -> String {
    let make_conf = config_portage_dir(globals).join("make.conf");
    if let Ok(conf) = MakeConf::load(&make_conf)
        && let Some(arch) = conf.get("ARCH").filter(|a| !a.is_empty())
    {
        return arch.to_string();
    }
    globals.arch.as_str().to_string()
}

/// Profiles matching `arch` (the eselect-like filter), in `profiles.desc` order.
fn profiles_for(repo: &Repository, arch: &str) -> Result<Vec<ProfileDesc>> {
    Ok(repo
        .profiles_desc()
        .context("reading profiles.desc")?
        .into_iter()
        .filter(|d| d.arch().as_str() == arch)
        .collect())
}

pub fn run(action: &ProfileAction, globals: &Cli) -> Result<()> {
    match action {
        ProfileAction::List => list(globals),
        ProfileAction::Show => show(globals),
        ProfileAction::Set { target } => set(globals, target),
    }
}

/// The repo whose `profiles/` we list/link from — the configured main repo
/// (usually `gentoo`).
fn main_repo() -> Result<Repository> {
    let conf = ReposConf::load().context("reading repos.conf")?;
    let entry = conf
        .main_repo()
        .or_else(|| conf.find("gentoo"))
        .context("no main repo configured in repos.conf")?;
    crate::repo_open::open(
        entry
            .location
            .as_path()
            .unwrap_or(std::path::Path::new(".")),
    )
    .with_context(|| {
        format!(
            "opening main repo at {}",
            entry
                .location
                .as_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(virtual)".to_string())
        )
    })
}

/// Where `make.profile` lives for this invocation.
fn make_profile_link(globals: &Cli) -> Utf8PathBuf {
    config_portage_dir(globals).join("make.profile")
}

/// The profile path the current `make.profile` points at, relative to the repo's
/// `profiles/` dir (so it can be matched against `profiles.desc`).
fn current_profile(globals: &Cli, repo: &Repository) -> Option<String> {
    let link = make_profile_link(globals);
    // Canonicalize the link itself so a relative symlink (`../../var/db/…`, as
    // eselect writes) resolves against its own directory, not the CWD.
    let target = link.canonicalize_utf8().ok()?;
    let profiles = repo.path().join("profiles").canonicalize_utf8().ok()?;
    target.strip_prefix(&profiles).ok().map(Utf8Path::to_string)
}

fn list(globals: &Cli) -> Result<()> {
    let repo = main_repo()?;
    let arch = effective_arch(globals);
    let descs = profiles_for(&repo, &arch)?;
    let current = current_profile(globals, &repo);
    let mut out = anstream::stdout();

    // Calculate the width needed for the number column (e.g., "[10]" needs 4 chars)
    let max_num = descs.len();
    let num_width = max_num.to_string().len();

    for (i, d) in descs.iter().enumerate() {
        // Path stays plain for legibility; only the status and — for the current
        // profile — the list number and the `*` marker are coloured.
        let st = profile_status(d.status());
        let is_current = current.as_deref() == Some(d.path());
        let n = i + 1;
        let num = if is_current {
            format!("{C_STAR}[{:>width$}]{C_STAR:#}", n, width = num_width)
        } else {
            format!("[{:>width$}]", n, width = num_width)
        };
        let mark = if is_current {
            format!(" {C_STAR}*{C_STAR:#}")
        } else {
            String::new()
        };
        writeln!(
            out,
            "  {num}  {}  ({st}{}{st:#}){mark}",
            d.path(),
            d.status()
        )
        .ok();
    }
    Ok(())
}

fn show(globals: &Cli) -> Result<()> {
    let repo = main_repo()?;
    match current_profile(globals, &repo) {
        Some(p) => println!("{p}"),
        None => println!("(no profile set at {})", make_profile_link(globals)),
    }
    Ok(())
}

fn set(globals: &Cli, target: &str) -> Result<()> {
    let repo = main_repo()?;

    // Resolve a list number (1-based, against the same arch-filtered list `list`
    // shows) to a profile path; otherwise treat the argument as a profile path
    // directly (the cross-aware case — no host-arch validation).
    let profile_path = if let Ok(n) = target.parse::<usize>() {
        let descs = profiles_for(&repo, &effective_arch(globals))?;
        let idx = n.checked_sub(1).context("profile numbers start at 1")?;
        descs
            .get(idx)
            .with_context(|| format!("profile number {n} out of range (1..={})", descs.len()))?
            .path()
            .to_string()
    } else {
        target.to_string()
    };

    let profile_dir = repo.path().join("profiles").join(&profile_path);
    if !profile_dir.is_dir() {
        bail!("profile '{profile_path}' not found at {profile_dir}");
    }

    let link = make_profile_link(globals);
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
    }
    // Replace any existing symlink/file. Absolute target so it resolves the same
    // from a sysroot/offset (matching crossdev-stages, not eselect's relative
    // link) — important for cross sysroots.
    match std::fs::symlink_metadata(&link) {
        Ok(_) => {
            std::fs::remove_file(&link).with_context(|| format!("removing existing {link}"))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("stat {link}")),
    }
    std::os::unix::fs::symlink(&profile_dir, &link)
        .with_context(|| format!("linking {link} -> {profile_dir}"))?;

    println!(">>> profile set: {profile_path}");
    println!("    {link} -> {profile_dir}");
    Ok(())
}
