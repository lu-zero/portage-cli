//! `em --info` — an `emerge --info` workalike: resolved profile/build
//! config (CHOST/CFLAGS/FEATURES/USE, with USE_EXPAND groups like
//! `VIDEO_CARDS` broken out onto their own line, exactly as portage does)
//! plus the configured repositories. Useful for bug reports and for
//! comparing against a real `emerge --info` when something like USE_EXPAND
//! derivation looks wrong. `--json` (real emerge has no equivalent) emits
//! the same data as structured JSON instead of the text layout.

use std::collections::BTreeMap;
use std::io::Write as _;

use anstyle::{AnsiColor, Color, Effects, Style};
use anyhow::{Context, Result};
use camino::Utf8Path;
use portage_repo::UseExpand;
use serde::Serialize;

use crate::cli::Cli;

// Real `emerge --info` has no coloring at all (verified: zero ANSI escapes
// even under --color=y) — there's no portage convention to match here, so
// this reuses `em`'s own established -pv palette instead: plain green for
// package/repo-name-shaped text (matching C_PKG), dimmed for labels. USE
// flags reuse query::depgraph::output::colorize_use_flag directly (same
// C_ON/C_OFF bold red/blue as -pv's own USE="..." line) rather than a
// second, duplicated implementation.
const C_PKG: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green)));
const C_LABEL: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green)));
const C_BOLD: Style = Style::new().effects(Effects::BOLD);
const C_DIM: Style = Style::new().effects(Effects::DIMMED);

/// Color each USE token via the same helper `-pv`'s own USE="..." line uses.
fn colorize_flags(flags: &[String]) -> String {
    flags
        .iter()
        .map(|f| crate::query::depgraph::output::colorize_use_flag(f))
        .collect::<Vec<_>>()
        .join(" ")
}

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

#[derive(Serialize)]
struct RepoInfo {
    name: String,
    location: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    masters: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volatile: Option<bool>,
}

#[derive(Serialize)]
struct MemInfo {
    total_kib: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    free_kib: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    swap_total_kib: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    swap_free_kib: Option<u64>,
}

#[derive(Serialize)]
struct Info {
    em_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    has_profile: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    chost: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_uname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mem: Option<MemInfo>,
    repositories: Vec<RepoInfo>,
    /// `"global"` (the plain, non-USE_EXPAND flags) plus one entry per
    /// USE_EXPAND group, keyed by its uppercase variable name (`VIDEO_CARDS`).
    use_flags: BTreeMap<String, Vec<String>>,
    vars: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unset: Vec<String>,
}

pub(crate) async fn run(cli: &Cli) -> Result<()> {
    let roots = cli.roots();
    let config_root = roots.config().unwrap_or(Utf8Path::new("/"));

    let repo = crate::crossdev::main_repo(cli)?;
    let mut shell = repo.shell().await.context("creating shell")?;
    shell.set_terminal(crate::style::terminal_config());
    let has_profile =
        crate::ebuild::apply_profile_env(&mut shell, roots.config(), roots.config_overlay())
            .await?;

    let profile = crate::select::profile::current_profile(cli, &repo);
    let chost = shell.get_var("CHOST");

    let repositories: Vec<RepoInfo> = match roots.repos_conf() {
        Ok(conf) => {
            let mut out = Vec::new();
            for r in conf.repos() {
                let path = r.location.as_path();
                // `RepoEntry.masters`/`.volatile` are repos.conf-only and
                // almost always empty in practice — masters normally comes
                // from the repo's own metadata/layout.conf, and volatile
                // (when not set explicitly) is inferred from filesystem
                // ownership. Both need the repo actually opened; fall back
                // to the repos.conf-only fields if that fails (e.g. a
                // virtual/alias location).
                let (masters, volatile) = match path.and_then(|p| crate::repo_open::open(p).ok()) {
                    Some(opened) => (
                        opened.layout().masters.clone(),
                        crate::maint::sync::resolve_volatile(r, opened.path().as_std_path()),
                    ),
                    None => (r.masters.clone(), r.volatile.unwrap_or(false)),
                };
                out.push(RepoInfo {
                    name: r.name.clone(),
                    location: path
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(virtual)".to_string()),
                    masters,
                    sync_type: r.sync_type.clone().filter(|t| !t.is_empty()),
                    sync_uri: r.sync_uri.clone().filter(|u| !u.is_empty()),
                    volatile: Some(volatile),
                });
            }
            out
        }
        Err(_) => Vec::new(),
    };

    let use_expand_names = shell.get_var("USE_EXPAND").unwrap_or_default();
    let use_str = shell.get_var("USE").unwrap_or_default();
    let expand = UseExpand::from_var(&use_expand_names);
    let flags: Vec<&str> = use_str.split_whitespace().collect();
    let mut groups = expand.group(flags);
    let mut global: Vec<String> = groups
        .remove("global")
        .unwrap_or_default()
        .into_iter()
        .map(str::to_string)
        .collect();
    global.sort_unstable();
    let mut use_flags: BTreeMap<String, Vec<String>> = BTreeMap::new();
    use_flags.insert("global".to_string(), global);
    for (group, values) in groups {
        if values.is_empty() {
            continue;
        }
        let mut values: Vec<String> = values.into_iter().map(str::to_string).collect();
        values.sort_unstable();
        use_flags.insert(group.to_uppercase(), values);
    }

    let mut vars = BTreeMap::new();
    let mut unset = Vec::new();
    for &name in INFO_VARS {
        match resolve_info_var(&shell, name) {
            Some(v) => {
                vars.insert(name.to_string(), v);
            }
            None => unset.push(name.to_string()),
        }
    }
    vars.insert("PORTAGE_CONFIGROOT".to_string(), config_root.to_string());

    let info = Info {
        em_version: env!("CARGO_PKG_VERSION").to_string(),
        profile,
        has_profile,
        chost,
        system_uname: system_uname(),
        mem: mem_info(),
        repositories,
        use_flags,
        vars,
        unset,
    };

    if cli.merge_flags.json {
        println!("{}", serde_json::to_string_pretty(&info)?);
    } else {
        print_text(&info)?;
    }
    Ok(())
}

fn print_text(info: &Info) -> Result<()> {
    let mut out = anstream::stdout();

    writeln!(
        out,
        "{C_PKG}em {} ({}{}{}){C_PKG:#}",
        info.em_version,
        info.profile.as_deref().unwrap_or("no profile set"),
        info.chost.as_deref().map(|_| ", ").unwrap_or(""),
        info.chost.as_deref().unwrap_or(""),
    )?;
    writeln!(out, "{}", "=".repeat(65))?;
    if !info.has_profile {
        use crate::style::C_WARN;
        writeln!(
            out,
            "{C_WARN}no usable profile at {}/etc/portage/make.profile — showing bare defaults{C_WARN:#}",
            info.vars.get("PORTAGE_CONFIGROOT").map_or("/", |v| v)
        )?;
    }
    if let Some(uname) = &info.system_uname {
        writeln!(out, "{C_DIM}System uname:{C_DIM:#} {uname}")?;
    }
    if let Some(mem) = &info.mem {
        match mem.free_kib {
            Some(free) => writeln!(
                out,
                "{C_DIM}KiB Mem:{C_DIM:#}   {} total,  {free} free",
                mem.total_kib
            )?,
            None => writeln!(out, "{C_DIM}KiB Mem:{C_DIM:#}   {} total", mem.total_kib)?,
        }
        if let (Some(total), Some(free)) = (mem.swap_total_kib, mem.swap_free_kib) {
            writeln!(
                out,
                "{C_DIM}KiB Swap:{C_DIM:#}  {total} total,  {free} free"
            )?;
        }
    }

    writeln!(out, "\n{C_BOLD}Repositories:{C_BOLD:#}\n")?;
    for r in &info.repositories {
        writeln!(out, "{C_PKG}{}{C_PKG:#}", r.name)?;
        writeln!(out, "    {C_DIM}location:{C_DIM:#} {}", r.location)?;
        if !r.masters.is_empty() {
            writeln!(out, "    {C_DIM}masters:{C_DIM:#} {}", r.masters.join(", "))?;
        }
        if let Some(t) = &r.sync_type {
            writeln!(out, "    {C_DIM}sync-type:{C_DIM:#} {t}")?;
        }
        if let Some(u) = &r.sync_uri {
            writeln!(out, "    {C_DIM}sync-uri:{C_DIM:#} {u}")?;
        }
        if let Some(v) = r.volatile {
            // Python-style capitalization, matching real emerge --info's
            // literal `volatile: True`/`volatile: False` (only the text
            // layout — JSON keeps a real bool).
            writeln!(
                out,
                "    {C_DIM}volatile:{C_DIM:#} {}",
                if v { "True" } else { "False" }
            )?;
        }
        writeln!(out)?;
    }

    let global = info
        .use_flags
        .get("global")
        .map(|v| colorize_flags(v))
        .unwrap_or_default();
    let mut use_line = format!("USE=\"{global}\"");
    for (group, values) in &info.use_flags {
        if group == "global" {
            continue;
        }
        use_line.push_str(&format!(
            " {C_LABEL}{group}{C_LABEL:#}=\"{}\"",
            colorize_flags(values)
        ));
    }
    writeln!(out, "{use_line}")?;

    for (name, value) in &info.vars {
        writeln!(out, "{C_LABEL}{name}{C_LABEL:#}=\"{value}\"")?;
    }
    if !info.unset.is_empty() {
        writeln!(out, "{C_DIM}Unset:{C_DIM:#}  {}", info.unset.join(", "))?;
    }

    Ok(())
}

/// Resolve one `INFO_VARS` entry the way real `emerge --info` actually would,
/// not just `shell.get_var`:
///
/// - `LANG`: real portage reads this from the literal process environment,
///   not make.conf/profile — and `ProfileStack`'s shell deliberately does
///   *not* inherit the ambient environment (a build-reproducibility choice
///   elsewhere in the codebase), so `shell.get_var("LANG")` alone would
///   always report it unset even when the shell running `em` clearly has one.
/// - `DISTDIR`/`PKGDIR`/`PORTAGE_TMPDIR`/`GENTOO_MIRRORS`: real
///   `make.globals`-only defaults (confirmed: unset in this host's
///   make.conf, yet `emerge --info` resolves them) — `ProfileStack` never
///   sources `make.globals` into the shell at all
///   (`ebuild::gentoo_mirrors_list`'s doc comment), so these need the same
///   env → shell → `make.globals` fallback `em`'s own fetch/binpkg code
///   already uses for them, via `ebuild::elog_setting`.
/// - Everything else: plain `shell.get_var`.
fn resolve_info_var(shell: &portage_repo::EbuildShell, name: &str) -> Option<String> {
    match name {
        "LANG" => std::env::var("LANG").ok().filter(|v| !v.is_empty()),
        "DISTDIR" | "PKGDIR" | "PORTAGE_TMPDIR" | "GENTOO_MIRRORS" => {
            let v = crate::ebuild::elog_setting(shell, name);
            (!v.is_empty()).then_some(v)
        }
        _ => shell.get_var(name),
    }
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

/// `/proc/meminfo` total/available mem *and* swap, in KiB (Linux-only,
/// matching this project's Linux focus — see `no-emerge-equivalents-in-help`
/// and the `--prefix`/macOS support notes elsewhere: full parity isn't the
/// goal on non-Linux hosts). `None` when `/proc/meminfo` isn't readable.
fn mem_info() -> Option<MemInfo> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut free = None;
    let mut swap_total = None;
    let mut swap_free = None;
    for line in content.lines() {
        let parse = |rest: &str| rest.split_whitespace().next().and_then(|v| v.parse().ok());
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = parse(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            free = parse(rest);
        } else if let Some(rest) = line.strip_prefix("SwapTotal:") {
            swap_total = parse(rest);
        } else if let Some(rest) = line.strip_prefix("SwapFree:") {
            swap_free = parse(rest);
        }
    }
    total.map(|total_kib| MemInfo {
        total_kib,
        free_kib: free,
        swap_total_kib: swap_total,
        swap_free_kib: swap_free,
    })
}
