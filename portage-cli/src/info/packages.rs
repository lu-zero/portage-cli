//! The installed-package part of `em --info <atom>`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{Read as _, Write};
use std::str::FromStr as _;

use anyhow::{Result, bail};
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Dep};
use portage_metadata::Keyword;
use portage_resolve::repo::ResolvedPolicy;
use portage_vdb::InstalledPackage;
use serde::Serialize;

use super::{C_LABEL, C_PKG, RULE_WIDTH, colorize_flags};
use crate::cli::{Applet, Cli};

/// Build settings compared against the current configuration.
const BUILD_VARS: [&str; 5] = ["CHOST", "CFLAGS", "CXXFLAGS", "FEATURES", "LDFLAGS"];

/// What Portage calls a package with no recorded repository.
const UNKNOWN_REPO: &str = "__unknown__";

/// One installed package in the report.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(super) struct PackageInfo {
    cpv: String,
    repository: String,
    use_flags: Vec<UseGroup>,
    /// Recorded build settings that differ from the current configuration.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    differs: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unset: Vec<String>,
}

/// `USE` or one `USE_EXPAND` variable, with `-off` and `(forced)` tokens.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct UseGroup {
    name: String,
    flags: Vec<String>,
}

/// The installed packages `cli`'s atoms name. A bad atom fails before any report is printed.
pub(super) fn matching(cli: &Cli) -> Result<Vec<InstalledPackage>> {
    let atoms = cli.atoms();
    if atoms.is_empty() {
        return Ok(Vec::new());
    }
    let installed = crate::vdb::open_cli_vdb(cli)
        .map(|vdb| vdb.packages().collect_vec())
        .unwrap_or_default();
    let roots = cli.roots();
    let main = crate::crossdev::main_repo(cli)?;
    let set = crate::repo_open::repo_set_from_conf(main, &roots, true);
    let mut targets = Vec::new();
    for raw in atoms {
        targets.extend(target_atoms(raw, &installed, &roots)?);
    }
    let mut out = Vec::new();
    for raw in &targets {
        let dep = expand_atom(raw, &installed, &|name| set.find_cpns(name))?;
        let known = installed.iter().any(|pkg| pkg.cpv().cpn == dep.cpn)
            || !set.find_cpns(&dep.cpn.to_string()).is_empty();
        if !known {
            let root = roots.merge_root();
            let suffix = if root.as_str() == "/" {
                String::new()
            } else {
                format!(" for {root}")
            };
            bail!("there are no ebuilds to satisfy \"{raw}\"{suffix}");
        }
        let mut matched: Vec<_> = installed
            .iter()
            .filter(|pkg| installed_matches(&dep, pkg))
            .cloned()
            .collect();
        matched.sort_by(|a, b| a.cpv().cmp(b.cpv()));
        out.extend(matched);
    }
    Ok(out)
}

/// `raw`, or the atoms of the set it names. Bare `world` and `system` are sets too.
fn target_atoms(
    raw: String,
    installed: &[InstalledPackage],
    roots: &portage_resolve::Roots,
) -> Result<Vec<String>> {
    let name = match raw.strip_prefix('@') {
        Some(name) => name,
        None if matches!(raw.as_str(), "world" | "system") => raw.as_str(),
        None => return Ok(vec![raw]),
    };
    if name == "installed" {
        let cpns: BTreeSet<String> = installed.iter().map(|p| p.cpv().cpn.to_string()).collect();
        return Ok(cpns.into_iter().collect());
    }
    let deps = if name == "security" {
        crate::glsa::security_atoms_from_roots(roots)?
    } else {
        crate::maint::world::resolve_set(roots.config(), roots.merge_root(), name)?
    };
    let atoms: BTreeSet<String> = deps.iter().map(ToString::to_string).collect();
    Ok(atoms.into_iter().collect())
}

/// Parses `raw`, giving a short name its category from the installed packages, then the repos.
fn expand_atom(
    raw: &str,
    installed: &[InstalledPackage],
    repo_cpns: &dyn Fn(&str) -> Vec<Cpn>,
) -> Result<Dep> {
    let invalid = || anyhow::anyhow!("'{raw}' is not a valid package atom");
    if raw.starts_with('!') {
        return Err(invalid());
    }
    if raw.contains('/') {
        return Dep::parse(raw).map_err(|_| invalid());
    }
    let at = raw
        .find(|c| !matches!(c, '<' | '>' | '=' | '~'))
        .ok_or_else(invalid)?;
    let (operator, rest) = raw.split_at(at);
    let placeholder = Dep::parse(&format!("{operator}null/{rest}")).map_err(|_| invalid())?;
    let name = placeholder.cpn.package.as_ref();
    let installed_cpns: Vec<Cpn> = installed
        .iter()
        .map(|pkg| pkg.cpv().cpn)
        .filter(|cpn| cpn.package.as_ref() == name)
        .collect();
    let cpn = match pick_cpn(raw, installed_cpns)? {
        Some(cpn) => Some(cpn),
        None => pick_cpn(raw, repo_cpns(name))?,
    };
    match cpn {
        Some(cpn) => {
            Dep::parse(&format!("{operator}{}/{rest}", cpn.category)).map_err(|_| invalid())
        }
        None => Ok(placeholder),
    }
}

/// The package a short name means: a real package wins over a virtual or account of that name.
fn pick_cpn(raw: &str, cpns: Vec<Cpn>) -> Result<Option<Cpn>> {
    let mut names: Vec<(String, Cpn)> =
        cpns.into_iter().map(|cpn| (cpn.to_string(), cpn)).collect();
    names.sort_by(|a, b| a.0.cmp(&b.0));
    names.dedup_by(|a, b| a.0 == b.0);
    if names.len() <= 1 {
        return Ok(names.pop().map(|(_, cpn)| cpn));
    }
    let shadow = |cpn: &Cpn| {
        matches!(
            cpn.category.as_ref(),
            "acct-group" | "acct-user" | "virtual"
        )
    };
    if !names.iter().any(|(_, cpn)| shadow(cpn)) {
        let names: Vec<&str> = names.iter().map(|(name, _)| name.as_str()).collect();
        bail!(
            "the short ebuild name \"{raw}\" is ambiguous, use one of: {}",
            names.join(", ")
        );
    }
    Ok(names
        .iter()
        .rev()
        .find(|(_, cpn)| !shadow(cpn))
        .map(|(_, cpn)| *cpn))
}

fn installed_matches(dep: &Dep, pkg: &InstalledPackage) -> bool {
    if !dep.matches_cpv(pkg.cpv(), pkg.slot().ok().as_ref()) {
        return false;
    }
    if dep
        .repo
        .is_some_and(|repo| pkg.repository().ok().flatten() != Some(repo))
    {
        return false;
    }
    let Some(use_deps) = &dep.use_deps else {
        return true;
    };
    let enabled = pkg.use_flags().unwrap_or_default();
    let iuse: Vec<Interned<DefaultInterner>> = pkg
        .iuse()
        .unwrap_or_default()
        .iter()
        .map(Interned::from)
        .collect();
    use_deps
        .iter()
        .all(|use_dep| portage_resolve::use_dep_satisfied(use_dep, &enabled, &iuse))
}

/// How USE is grouped and ordered for display.
struct UseLayout<'a> {
    expand: &'a [String],
    hidden: &'a [String],
    alphabetical: bool,
}

/// Describes each of `packages` against the configuration `var` reads.
pub(super) async fn describe(
    cli: &Cli,
    packages: &[InstalledPackage],
    var: &dyn Fn(&str) -> Option<String>,
) -> Vec<PackageInfo> {
    if packages.is_empty() {
        return Vec::new();
    }
    let roots = cli.roots();
    let policy = match crate::crossdev::main_repo(cli) {
        Ok(repo) => portage_resolve::use_env::build_use_env(
            &repo,
            roots.config(),
            roots.config_overlay(),
            None,
            None,
        )
        .await
        .ok()
        .map(|env| ResolvedPolicy::from_use_env(env, &cli.arch())),
        Err(_) => None,
    };
    let (expand, hidden) = policy.as_ref().map_or((&[][..], &[][..]), |(_, extras)| {
        (&extras.expand[..], &extras.expand_hidden[..])
    });
    let layout = UseLayout {
        expand,
        hidden,
        alphabetical: matches!(&cli.applet, Some(Applet::Emerge(a)) if a.display.alphabetical),
    };
    let arch = var("ARCH");
    let resolved = policy.as_ref().map(|(resolved, _)| resolved);
    packages
        .iter()
        .map(|pkg| describe_one(pkg, resolved, &layout, arch.as_deref(), var))
        .collect()
}

fn describe_one(
    pkg: &InstalledPackage,
    policy: Option<&ResolvedPolicy>,
    layout: &UseLayout<'_>,
    arch: Option<&str>,
    var: &dyn Fn(&str) -> Option<String>,
) -> PackageInfo {
    let mut enabled: BTreeSet<String> = pkg
        .use_flags()
        .unwrap_or_default()
        .iter()
        .map(|flag| flag.to_string())
        .collect();
    if let Some(arch) = arch {
        enabled.remove(arch);
    }
    let iuse: BTreeSet<String> = pkg
        .iuse()
        .unwrap_or_default()
        .iter()
        .map(|flag| flag.name().to_string())
        .collect();
    let pinned = policy.map_or_else(BTreeSet::new, |policy| {
        pinned_flags(pkg, policy, &enabled, &iuse)
    });
    let recorded = recorded_settings(pkg);
    let mut differs = BTreeMap::new();
    let mut unset = Vec::new();
    for name in BUILD_VARS {
        match recorded.get(name) {
            None => unset.push(name.to_owned()),
            Some(value) => {
                let current = var(name).unwrap_or_default();
                if !value.split_whitespace().eq(current.split_whitespace()) {
                    differs.insert(name.to_owned(), value.clone());
                }
            }
        }
    }
    PackageInfo {
        cpv: pkg.cpv().to_string(),
        repository: pkg
            .repository()
            .ok()
            .flatten()
            .map_or_else(|| UNKNOWN_REPO.to_owned(), |repo| repo.to_string()),
        use_flags: use_groups(&enabled, &iuse, &pinned, layout),
        differs,
        unset,
    }
}

/// The package's flags under the profile's `use.force` and `use.mask`.
fn pinned_flags(
    pkg: &InstalledPackage,
    policy: &ResolvedPolicy,
    enabled: &BTreeSet<String>,
    iuse: &BTreeSet<String>,
) -> BTreeSet<String> {
    let slot = pkg.slot().ok();
    let keywords: Vec<Keyword> = pkg
        .keywords()
        .unwrap_or_default()
        .iter()
        .filter_map(|keyword| Keyword::from_str(keyword).ok())
        .collect();
    let stable =
        policy
            .accept_keywords
            .is_stable(&keywords, pkg.cpv(), slot.as_ref().map(|slot| slot.slot));
    let candidates: HashSet<_> = enabled
        .iter()
        .chain(iuse)
        .map(|flag| Interned::intern(flag))
        .collect();
    let (forced, masked) =
        policy
            .force_mask
            .effective(pkg.cpv(), slot.as_ref(), stable, &candidates);
    forced
        .iter()
        .chain(&masked)
        .map(ToString::to_string)
        .collect()
}

fn use_groups(
    enabled: &BTreeSet<String>,
    iuse: &BTreeSet<String>,
    pinned: &BTreeSet<String>,
    layout: &UseLayout<'_>,
) -> Vec<UseGroup> {
    let mut expand: Vec<&String> = layout.expand.iter().collect();
    expand.sort();
    let all: BTreeSet<&String> = enabled.iter().chain(iuse).collect();
    let mut grouped = BTreeSet::new();
    let mut extra = Vec::new();
    for name in expand {
        let prefix = format!("{}_", name.to_lowercase());
        let members: Vec<(&str, &String)> = all
            .iter()
            .filter_map(|flag| Some((flag.strip_prefix(&prefix)?, *flag)))
            .collect();
        grouped.extend(members.iter().map(|(_, flag)| *flag));
        let shown = !layout.hidden.iter().any(|h| h.eq_ignore_ascii_case(name));
        if shown && !members.is_empty() {
            extra.push(UseGroup {
                name: name.clone(),
                flags: render_flags(&members, enabled, pinned, layout.alphabetical),
            });
        }
    }
    let plain: Vec<(&str, &String)> = all
        .iter()
        .filter(|flag| !grouped.contains(**flag))
        .map(|flag| (flag.as_str(), *flag))
        .collect();
    let mut out = vec![UseGroup {
        name: "USE".to_owned(),
        flags: render_flags(&plain, enabled, pinned, layout.alphabetical),
    }];
    out.extend(extra);
    out
}

/// Enabled flags, then disabled, each by name; one list by name when `alphabetical`.
fn render_flags(
    members: &[(&str, &String)],
    enabled: &BTreeSet<String>,
    pinned: &BTreeSet<String>,
    alphabetical: bool,
) -> Vec<String> {
    let mut members: Vec<_> = members
        .iter()
        .map(|&(shown, flag)| (!enabled.contains(flag) && !alphabetical, shown, flag))
        .collect();
    members.sort();
    members
        .into_iter()
        .map(|(_, shown, flag)| {
            let token = if enabled.contains(flag) {
                shown.to_owned()
            } else {
                format!("-{shown}")
            };
            if pinned.contains(flag) {
                format!("({token})")
            } else {
                token
            }
        })
        .collect()
}

fn recorded_settings(pkg: &InstalledPackage) -> BTreeMap<String, String> {
    let Ok(file) = std::fs::File::open(pkg.path().join("environment.bz2")) else {
        return BTreeMap::new();
    };
    let mut bytes = Vec::new();
    if bzip2::read::BzDecoder::new(file)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return BTreeMap::new();
    }
    assigned_vars(&String::from_utf8_lossy(&bytes))
}

/// [`BUILD_VARS`] assigned at the top level of a saved shell environment.
fn assigned_vars(text: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let body = line
            .strip_prefix("declare ")
            .map(|rest| match rest.strip_prefix('-') {
                Some(flagged) => flagged.split_once(' ').map_or("", |(_, body)| body),
                None => rest,
            })
            .or_else(|| line.strip_prefix("export "))
            .unwrap_or(line);
        let Some((name, value)) = body.split_once('=') else {
            continue;
        };
        if !BUILD_VARS.contains(&name) {
            continue;
        }
        let value = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let mut text = value[1..].to_owned();
                while !closes(&text, quote) {
                    let Some(next) = lines.next() else {
                        break;
                    };
                    text.push('\n');
                    text.push_str(next);
                }
                text.strip_suffix(quote).map_or(text.clone(), str::to_owned)
            }
            _ => value.to_owned(),
        };
        found.insert(name.to_owned(), value);
    }
    found
}

fn closes(text: &str, quote: char) -> bool {
    text.strip_suffix(quote)
        .is_some_and(|rest| quote == '\'' || !rest.ends_with('\\'))
}

/// Writes the `Package Settings` section; nothing when no package matched.
pub(super) fn print(out: &mut impl Write, packages: &[PackageInfo]) -> Result<()> {
    if packages.is_empty() {
        return Ok(());
    }
    writeln!(out)?;
    super::write_title(out, "Package Settings")?;
    writeln!(out, "{}", "=".repeat(RULE_WIDTH))?;
    for pkg in packages {
        writeln!(
            out,
            "\n{C_PKG}{}::{}{C_PKG:#} was built with the following:",
            pkg.cpv, pkg.repository
        )?;
        let groups: Vec<String> = pkg
            .use_flags
            .iter()
            .map(|group| format!("{}=\"{}\"", group.name, colorize_tokens(&group.flags)))
            .collect();
        writeln!(out, "{}", groups.join(" "))?;
        for (name, value) in &pkg.differs {
            writeln!(out, "{C_LABEL}{name}{C_LABEL:#}=\"{value}\"")?;
        }
        if !pkg.unset.is_empty() {
            writeln!(out, "Unset: {}", pkg.unset.join(", "))?;
        }
    }
    Ok(())
}

fn colorize_tokens(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(
            |token| match token.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
                Some(inner) => format!("({})", colorize_flags(&[inner.to_owned()])),
                None => colorize_flags(std::slice::from_ref(token)),
            },
        )
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(flags: &[&str]) -> BTreeSet<String> {
        flags.iter().map(|flag| (*flag).to_owned()).collect()
    }

    fn vdb_with(
        packages: &[(&str, &str, &str, &str)],
    ) -> (tempfile::TempDir, Vec<InstalledPackage>) {
        let tmp = tempfile::tempdir().unwrap();
        for (cat, pf, use_flags, iuse) in packages {
            let dir = tmp.path().join("var/db/pkg").join(cat).join(pf);
            std::fs::create_dir_all(&dir).unwrap();
            for (file, body) in [
                ("SLOT", "0"),
                ("EAPI", "8"),
                ("USE", use_flags),
                ("IUSE", iuse),
                ("repository", "gentoo"),
            ] {
                std::fs::write(dir.join(file), body).unwrap();
            }
        }
        let root = camino::Utf8PathBuf::try_from(tmp.path().join("var/db/pkg")).unwrap();
        let installed = portage_vdb::Vdb::open(root)
            .unwrap()
            .packages()
            .collect_vec();
        (tmp, installed)
    }

    fn cpn(raw: &str) -> Cpn {
        Dep::parse(raw).unwrap().cpn
    }

    #[test]
    fn use_groups_follow_emerge_order_and_hide_groups() {
        let expand = ["PYTHON_TARGETS".to_owned(), "ELIBC".to_owned()];
        let hidden = ["ELIBC".to_owned()];
        let layout = UseLayout {
            expand: &expand,
            hidden: &hidden,
            alphabetical: false,
        };
        let groups = use_groups(
            &set(&["zlib", "elibc_glibc", "python_targets_python3_13"]),
            &set(&[
                "alpha",
                "zlib",
                "python_targets_python3_12",
                "python_targets_python3_13",
            ]),
            &set(&["alpha"]),
            &layout,
        );
        assert_eq!(groups[0].flags, ["zlib", "(-alpha)"]);
        assert_eq!(groups[1].name, "PYTHON_TARGETS");
        assert_eq!(groups[1].flags, ["python3_13", "-python3_12"]);
        assert_eq!(groups.len(), 2);

        let layout = UseLayout {
            alphabetical: true,
            ..layout
        };
        let groups = use_groups(
            &set(&["zlib"]),
            &set(&["alpha", "zlib"]),
            &set(&[]),
            &layout,
        );
        assert_eq!(groups[0].flags, ["-alpha", "zlib"]);
    }

    #[test]
    fn assigned_vars_reads_only_top_level_assignments() {
        let found = assigned_vars(
            "declare -x CFLAGS=\"-O2\ndeclare -x -pipe\"\nexport CHOST=x86\nLDFLAGS='-Wl'\n\
             declare -- FEATURES=\"a \\\" b\"\nf ()\n{\n    CXXFLAGS=\"in-function\"\n}\n",
        );
        assert_eq!(found["CFLAGS"], "-O2\ndeclare -x -pipe");
        assert_eq!(found["CHOST"], "x86");
        assert_eq!(found["LDFLAGS"], "-Wl");
        assert_eq!(found["FEATURES"], "a \\\" b");
        assert!(!found.contains_key("CXXFLAGS"));
    }

    #[test]
    fn short_names_expand_from_installed_then_repos() {
        let (_tmp, installed) = vdb_with(&[
            ("sys-libs", "zlib-1.3", "", ""),
            ("app-misc", "dup-1", "", ""),
            ("dev-libs", "dup-1", "", ""),
        ]);
        let repos = |_: &str| vec![cpn("dev-libs/other")];
        assert_eq!(
            expand_atom("=zlib-1.3", &installed, &repos)
                .unwrap()
                .to_string(),
            "=sys-libs/zlib-1.3"
        );
        assert_eq!(
            expand_atom("other", &installed, &repos).unwrap().cpn,
            cpn("dev-libs/other")
        );
        let err = expand_atom("dup", &installed, &repos)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("app-misc/dup") && err.contains("dev-libs/dup"),
            "{err}"
        );
        for bad in ["@", "!sys-libs/zlib", "=sys-libs/zlib"] {
            assert!(expand_atom(bad, &installed, &repos).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_real_package_wins_over_a_virtual_of_the_same_name() {
        let (_tmp, installed) = vdb_with(&[
            ("sys-libs", "zlib-1.3", "", ""),
            ("virtual", "zlib-1", "", ""),
            ("virtual", "only-1", "", ""),
        ]);
        let repos = |_: &str| vec![cpn("dev-libs/only")];
        assert_eq!(
            expand_atom("zlib", &installed, &repos).unwrap().cpn,
            cpn("sys-libs/zlib")
        );
        assert_eq!(
            expand_atom("only", &installed, &repos).unwrap().cpn,
            cpn("virtual/only")
        );
        let shadows = vec![cpn("virtual/both"), cpn("acct-user/both")];
        assert_eq!(pick_cpn("both", shadows).unwrap(), None);
    }

    #[test]
    fn matches_honour_repo_and_use_constraints() {
        let (_tmp, installed) = vdb_with(&[("sys-libs", "zlib-1.3", "ssl", "ssl minizip")]);
        let pkg = &installed[0];
        for (atom, expected) in [
            ("sys-libs/zlib", true),
            ("sys-libs/zlib::gentoo", true),
            ("sys-libs/zlib::other", false),
            ("sys-libs/zlib[ssl]", true),
            ("sys-libs/zlib[minizip]", false),
            ("sys-libs/zlib[-minizip]", true),
            (">sys-libs/zlib-2", false),
        ] {
            assert_eq!(
                installed_matches(&Dep::parse(atom).unwrap(), pkg),
                expected,
                "{atom}"
            );
        }
    }
}
