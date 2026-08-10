//! `em --info` — an `emerge --info` workalike: resolved profile/build
//! config (CHOST/CFLAGS/FEATURES/USE, with USE_EXPAND groups like
//! `VIDEO_CARDS` broken out onto their own line, exactly as portage does)
//! plus the configured repositories. Useful for bug reports and for
//! comparing against a real `emerge --info` when something like USE_EXPAND
//! derivation looks wrong.

use std::io::Write as _;

use anyhow::{Context, Result};
use camino::Utf8Path;
use portage_repo::UseExpand;

use crate::cli::Cli;

/// The fixed, non-verbose var list real `emerge --info` prints (its
/// `myvars` list in `_emerge/actions.py:action_info`, trimmed to what `em`
/// actually resolves — deprecated PORTDIR/SYNC-era vars and the
/// bzip2/rsync-specific ones em has no equivalent for are dropped). `USE`
/// is handled separately (it also carries the USE_EXPAND groups).
const INFO_VARS: &[&str] = &[
    "ACCEPT_KEYWORDS",
    "ACCEPT_LICENSE",
    "CBUILD",
    "CFLAGS",
    "CHOST",
    "CONFIG_PROTECT",
    "CONFIG_PROTECT_MASK",
    "CXXFLAGS",
    "DISTDIR",
    "EMERGE_DEFAULT_OPTS",
    "FCFLAGS",
    "FEATURES",
    "FFLAGS",
    "GENTOO_MIRRORS",
    "LANG",
    "LDFLAGS",
    "MAKEOPTS",
    "PKGDIR",
    "PORTAGE_TMPDIR",
    "SHELL",
];

pub(crate) async fn run(cli: &Cli) -> Result<()> {
    let roots = cli.roots();
    let config_root = roots.config().unwrap_or(Utf8Path::new("/"));

    let repo = crate::crossdev::main_repo(cli)?;
    let mut shell = repo.shell().await.context("creating shell")?;
    shell.set_terminal(crate::style::terminal_config());
    let has_profile =
        crate::ebuild::apply_profile_env(&mut shell, roots.config(), roots.config_overlay())
            .await?;

    let mut out = anstream::stdout();

    let profile = crate::select::profile::current_profile(cli, &repo);
    let chost = shell.get_var("CHOST");
    writeln!(
        out,
        "em {} ({}{}{})",
        env!("CARGO_PKG_VERSION"),
        profile.as_deref().unwrap_or("no profile set"),
        chost.as_deref().map(|_| ", ").unwrap_or(""),
        chost.as_deref().unwrap_or(""),
    )?;
    writeln!(out, "{}", "=".repeat(65))?;
    if !has_profile {
        writeln!(
            out,
            "no usable profile at {config_root}/etc/portage/make.profile — showing bare defaults"
        )?;
    }
    if let Some(uname) = system_uname() {
        writeln!(out, "System uname: {uname}")?;
    }
    if let Some(mem) = mem_line() {
        writeln!(out, "{mem}")?;
    }

    writeln!(out, "\nRepositories:\n")?;
    if let Ok(conf) = roots.repos_conf() {
        for r in conf.repos() {
            writeln!(out, "{}", r.name)?;
            if let Some(p) = r.location.as_path() {
                writeln!(out, "    location: {}", p.display())?;
            } else {
                writeln!(out, "    location: (virtual)")?;
            }
            if !r.masters.is_empty() {
                writeln!(out, "    masters: {}", r.masters.join(", "))?;
            }
            if let Some(t) = r.sync_type.as_deref().filter(|t| !t.is_empty()) {
                writeln!(out, "    sync-type: {t}")?;
            }
            if let Some(u) = r.sync_uri.as_deref().filter(|u| !u.is_empty()) {
                writeln!(out, "    sync-uri: {u}")?;
            }
            if let Some(v) = r.volatile {
                writeln!(out, "    volatile: {v}")?;
            }
            writeln!(out)?;
        }
    }

    let use_expand_names = shell.get_var("USE_EXPAND").unwrap_or_default();
    let use_str = shell.get_var("USE").unwrap_or_default();
    let expand = UseExpand::from_var(&use_expand_names);
    let flags: Vec<&str> = use_str.split_whitespace().collect();
    let mut groups = expand.group(flags);
    let mut global: Vec<&str> = groups.remove("global").unwrap_or_default();
    global.sort_unstable();
    let mut use_line = format!("USE=\"{}\"", global.join(" "));
    // `group()`'s keys are lowercase prefixes (e.g. "video_cards"); real
    // portage's USE_EXPAND vars are uppercase (VIDEO_CARDS="...").
    for (group, mut values) in groups {
        if values.is_empty() {
            continue;
        }
        values.sort_unstable();
        use_line.push_str(&format!(
            " {}=\"{}\"",
            group.to_uppercase(),
            values.join(" ")
        ));
    }
    writeln!(out, "{use_line}")?;

    let mut unset = Vec::new();
    for name in INFO_VARS {
        match shell.get_var(name) {
            Some(v) => writeln!(out, "{name}=\"{v}\"")?,
            None => unset.push(*name),
        }
    }
    writeln!(out, "PORTAGE_CONFIGROOT=\"{config_root}\"")?;
    if !unset.is_empty() {
        writeln!(out, "Unset:  {}", unset.join(", "))?;
    }

    Ok(())
}

/// `Linux-<release>-<machine>`-shaped, matching the style real `emerge
/// --info`'s `platform.platform()` line uses — via `uname`, not a new crate
/// dependency for a single diagnostic line. `None` if `uname` isn't on
/// `PATH` (never fatal: `--info` still prints everything else).
fn system_uname() -> Option<String> {
    let output = std::process::Command::new("uname")
        .args(["-s", "-r", "-m"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let parts: Vec<&str> = std::str::from_utf8(&output.stdout)
        .ok()?
        .split_whitespace()
        .collect();
    let [sysname, release, machine] = parts.as_slice() else {
        return None;
    };
    Some(format!("{sysname}-{release}-{machine}"))
}

/// `KiB Mem:` line from `/proc/meminfo` (Linux-only, matching this
/// project's Linux focus — see `no-emerge-equivalents-in-help` and the
/// `--prefix`/macOS support notes elsewhere: full parity isn't the goal on
/// non-Linux hosts). `None` when `/proc/meminfo` isn't readable.
fn mem_line() -> Option<String> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut free = None;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest.split_whitespace().next()?.parse::<u64>().ok();
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            free = rest.split_whitespace().next()?.parse::<u64>().ok();
        }
    }
    match (total, free) {
        (Some(t), Some(f)) => Some(format!("KiB Mem:   {t} total,  {f} free")),
        (Some(t), None) => Some(format!("KiB Mem:   {t} total")),
        _ => None,
    }
}
