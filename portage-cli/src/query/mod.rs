pub mod check;
pub mod depends;
pub mod depgraph;
pub mod has;
pub mod hasuse;
pub mod keywords;
pub mod list;
pub mod meta;
pub mod uses;
pub mod which;

use std::str::FromStr;

use anyhow::{Context, anyhow};
use portage_repo::Repository;
use portage_vdb::Vdb;

use crate::style::{C_BOLD, C_COUNT, C_PKG, C_WARN};

/// How to handle ambiguous bare package names (matching multiple categories).
#[derive(Clone, Copy, Debug)]
pub enum ResolveMode {
    /// Report ambiguity as an error listing the candidates. If exactly one
    /// candidate is installed, the error names it and suggests `-u`.
    Error,
    /// Prefer the installed package (if exactly one matches). Otherwise error
    /// (same as [`Error`](Self::Error), hint included). A note is printed
    /// when disambiguation occurs.
    PreferInstalled,
    /// Interactively prompt for which candidate was meant (`--ask`) — real
    /// emerge has no equivalent (it just hard-errors); this is `em`'s own
    /// escape hatch for the case `Error`'s hint points at. Falls through to
    /// `Error`'s hard-error-with-hint on EOF or an invalid answer.
    Ask,
}

/// Resolve a raw atom string, expanding bare package names via the repo.
///
/// * `cat/pkg` — parsed as a standard atom.
/// * bare `name` — looked up in the repository.
///
/// Ambiguity handling depends on [`ResolveMode`]:
/// - [`Error`](ResolveMode::Error) — error listing candidates (naming the
///   installed one and suggesting `-u`, if exactly one is installed).
/// - [`PreferInstalled`](ResolveMode::PreferInstalled) — if exactly one
///   candidate is installed, use it (with a note); otherwise same as `Error`.
/// - [`Ask`](ResolveMode::Ask) — interactively prompt for a choice; same as
///   `Error` if that doesn't resolve it (EOF, invalid answer).
pub fn resolve_atom(
    repo: &Repository,
    vdb: Option<&Vdb>,
    mode: ResolveMode,
    raw: &str,
) -> anyhow::Result<portage_atom::Dep> {
    if raw.contains('/') {
        return portage_atom::Dep::from_str(raw).with_context(|| format!("bad atom '{raw}'"));
    }
    let cpns = repo.find_cpns(raw);
    match cpns.as_slice() {
        [] => Err(anyhow!(
            "no package found for '{raw}' — try specifying the category (e.g. cat/{raw})"
        )),
        [cpn] => portage_atom::Dep::from_str(&cpn.to_string())
            .with_context(|| format!("bad resolved atom '{cpn}'")),
        candidates => resolve_ambiguous(vdb, mode, raw, candidates),
    }
}

/// Handle an ambiguous bare name that matched multiple categories.
fn resolve_ambiguous(
    vdb: Option<&Vdb>,
    mode: ResolveMode,
    raw: &str,
    candidates: &[portage_atom::Cpn],
) -> anyhow::Result<portage_atom::Dep> {
    let installed = vdb.and_then(|vdb| pick_installed(vdb, candidates));

    if let ResolveMode::PreferInstalled = mode
        && let Some((dep, cpn)) = &installed
    {
        eprintln!(
            "note: '{C_BOLD}{raw}{C_BOLD:#}' is ambiguous ({}); using installed {C_PKG}{cpn}{C_PKG:#}",
            candidates
                .iter()
                .map(|c| format!("{C_PKG}{c}{C_PKG:#}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        return Ok(dep.clone());
    }

    if let ResolveMode::Ask = mode
        && let Some(dep) = ask_which_candidate(vdb, raw, candidates)
    {
        return Ok(dep);
    }

    let names: Vec<String> = candidates
        .iter()
        .map(|c| format!("{C_PKG}{c}{C_PKG:#}"))
        .collect();
    // Even under Error (or a failed Ask), tell the user the one thing that
    // would have resolved it — real emerge just dumps the candidate list and
    // leaves you to retype the full atom; naming the installed one and the
    // flag that would pick it is strictly more helpful (found live 2026-08-04).
    let hint = installed
        .map(|(_, cpn)| format!("\n    {C_PKG}{cpn}{C_PKG:#} is installed — pass -u to update it"))
        .unwrap_or_default();
    Err(anyhow!(
        "'{C_BOLD}{raw}{C_BOLD:#}' is ambiguous, matching: {}{hint}",
        names.join(", ")
    ))
}

/// Whether `cpn` has an installed entry in `vdb`.
fn is_installed(vdb: &Vdb, cpn: &portage_atom::Cpn) -> bool {
    vdb.category(cpn.category.as_ref()).is_some_and(|cat| {
        cat.packages()
            .into_iter()
            .any(|p| p.cpn().package.as_ref() == cpn.package.as_ref())
    })
}

/// Find the single installed candidate among `candidates`.
/// Returns `None` if zero or more than one are installed.
fn pick_installed<'a>(
    vdb: &Vdb,
    candidates: &'a [portage_atom::Cpn],
) -> Option<(portage_atom::Dep, &'a portage_atom::Cpn)> {
    let installed: Vec<&portage_atom::Cpn> = candidates
        .iter()
        .filter(|cpn| is_installed(vdb, cpn))
        .collect();
    match installed.as_slice() {
        [cpn] => portage_atom::Dep::from_str(&cpn.to_string())
            .ok()
            .map(|dep| (dep, *cpn)),
        _ => None,
    }
}

/// Interactively resolve an ambiguous bare name (`--ask`): list `candidates`
/// numbered, marking any installed one, and prompt for a choice — real emerge
/// has no equivalent, it just hard-errors (`ResolveMode::Error`'s job here).
/// Empty input picks the sole installed candidate if there is exactly one;
/// any other unrecognised answer (including EOF) returns `None` so the
/// caller falls through to the hard ambiguity error.
fn ask_which_candidate(
    vdb: Option<&Vdb>,
    raw: &str,
    candidates: &[portage_atom::Cpn],
) -> Option<portage_atom::Dep> {
    use std::io::Write;

    let default = vdb.and_then(|vdb| pick_installed(vdb, candidates));
    let mut out = anstream::stderr();
    let _ = writeln!(
        out,
        "{C_WARN}!!!{C_WARN:#} '{C_BOLD}{raw}{C_BOLD:#}' is ambiguous:"
    );
    for (i, cpn) in candidates.iter().enumerate() {
        let tag = match vdb {
            Some(vdb) if is_installed(vdb, cpn) => " (installed)",
            _ => "",
        };
        let _ = writeln!(
            out,
            "  {C_COUNT}{}{C_COUNT:#}) {C_PKG}{cpn}{C_PKG:#}{tag}",
            i + 1
        );
    }
    let _ = match &default {
        Some((_, cpn)) => write!(out, "Which package did you mean? [{C_PKG}{cpn}{C_PKG:#}] "),
        None => write!(out, "Which package did you mean? "),
    };
    let _ = out.flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).ok()? == 0 {
        return None;
    }
    let choice = line.trim();
    if choice.is_empty() {
        return default.map(|(dep, _)| dep);
    }
    choice
        .parse::<usize>()
        .ok()
        .filter(|&n| n >= 1 && n <= candidates.len())
        .and_then(|n| portage_atom::Dep::from_str(&candidates[n - 1].to_string()).ok())
}

/// Resolve multiple raw atom strings, expanding bare names via the repo.
///
/// Same disambiguation rules as [`resolve_atom`]. Failed resolutions are
/// printed as warnings and skipped.
pub fn resolve_atoms(
    raw: &[String],
    repo: &Repository,
    vdb: Option<&Vdb>,
    mode: ResolveMode,
) -> Vec<portage_atom::Dep> {
    let mut out = Vec::with_capacity(raw.len());
    for s in raw {
        match resolve_atom(repo, vdb, mode, s) {
            Ok(dep) => out.push(dep),
            Err(e) => crate::style::warn_line(&format!("{e}")),
        }
    }
    out
}

/// Resolve `raw` to an atom, then return every ebuild in `ebuilds` it matches,
/// sorted oldest-to-newest by version. Shared by the per-package `query`
/// commands (`keywords`/`meta`/`uses`); each caller reports its own message on
/// an empty result.
pub fn matching_ebuilds<'a>(
    repo: &Repository,
    vdb: Option<&Vdb>,
    mode: ResolveMode,
    ebuilds: &'a [portage_repo::Ebuild],
    raw: &str,
) -> anyhow::Result<Vec<&'a portage_repo::Ebuild>> {
    let dep = resolve_atom(repo, vdb, mode, raw)?;
    let mut matches: Vec<_> = ebuilds
        .iter()
        .filter(|e| which::dep_matches_cpv(&dep, e.cpv()))
        .collect();
    matches.sort_by(|a, b| a.cpv().version.cmp(&b.cpv().version));
    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal repo on disk with the given `(category, package)` pairs.
    fn make_repo(packages: &[(&str, &str)]) -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();

        let mut categories = std::collections::BTreeSet::new();
        for (cat, pkg) in packages {
            categories.insert(cat.to_string());
            std::fs::create_dir_all(dir.path().join(cat).join(pkg)).unwrap();
        }
        std::fs::write(
            dir.path().join("profiles").join("categories"),
            categories.into_iter().collect::<Vec<_>>().join("\n"),
        )
        .unwrap();

        let repo = Repository::builder()
            .in_memory_cache()
            .open(dir.path())
            .unwrap();
        (dir, repo)
    }

    /// Build a minimal VDB on disk with the given `(category, package, version)` entries.
    fn make_vdb(packages: &[(&str, &str, &str)]) -> (tempfile::TempDir, Vdb) {
        let dir = tempfile::tempdir().unwrap();
        for (cat, pkg, ver) in packages {
            let pkg_dir = dir.path().join(cat).join(format!("{pkg}-{ver}"));
            std::fs::create_dir_all(&pkg_dir).unwrap();
            std::fs::write(pkg_dir.join("CATEGORY"), cat).unwrap();
            std::fs::write(pkg_dir.join("PF"), format!("{pkg}-{ver}")).unwrap();
        }
        let vdb = Vdb::open(camino::Utf8Path::new(dir.path().to_str().unwrap())).unwrap();
        (dir, vdb)
    }

    // --- resolve_atom: basic cases ---

    #[test]
    fn resolve_atom_cat_pkg() {
        let (_dir, repo) = make_repo(&[("sys-apps", "foo")]);
        let dep = resolve_atom(&repo, None, ResolveMode::Error, "sys-apps/foo").unwrap();
        assert_eq!(dep.cpn.category.as_ref(), "sys-apps");
        assert_eq!(dep.cpn.package.as_ref(), "foo");
    }

    #[test]
    fn resolve_atom_bare_name_unique() {
        let (_dir, repo) = make_repo(&[("sys-apps", "foo")]);
        let dep = resolve_atom(&repo, None, ResolveMode::Error, "foo").unwrap();
        assert_eq!(dep.cpn.category.as_ref(), "sys-apps");
        assert_eq!(dep.cpn.package.as_ref(), "foo");
    }

    #[test]
    fn resolve_atom_bare_name_not_found() {
        let (_dir, repo) = make_repo(&[("sys-apps", "foo")]);
        let err = resolve_atom(&repo, None, ResolveMode::Error, "bar").unwrap_err();
        assert!(err.to_string().contains("no package found"));
    }

    // --- resolve_atom: Error mode ---

    #[test]
    fn resolve_atom_error_mode_ambiguous() {
        let (_dir, repo) = make_repo(&[("sys-apps", "foo"), ("app-misc", "foo")]);
        let err = resolve_atom(&repo, None, ResolveMode::Error, "foo").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
        assert!(err.to_string().contains("sys-apps/foo"));
        assert!(err.to_string().contains("app-misc/foo"));
    }

    #[test]
    fn resolve_atom_error_mode_hints_the_installed_candidate() {
        // Error mode still hard-errors (a mutating command shouldn't
        // silently guess), but names the one installed candidate and points
        // at -u — regression test for the 2026-08-04 UX request.
        let (_rdir, repo) = make_repo(&[("sys-apps", "foo"), ("app-misc", "foo")]);
        let (_vdir, vdb) = make_vdb(&[("sys-apps", "foo", "1.0")]);
        let err = resolve_atom(&repo, Some(&vdb), ResolveMode::Error, "foo").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
        assert!(err.to_string().contains("sys-apps/foo"));
        assert!(err.to_string().contains("is installed"));
        assert!(err.to_string().contains("-u"));
    }

    #[test]
    fn resolve_atom_error_mode_no_hint_when_none_installed() {
        let (_dir, repo) = make_repo(&[("sys-apps", "foo"), ("app-misc", "foo")]);
        let err = resolve_atom(&repo, None, ResolveMode::Error, "foo").unwrap_err();
        assert!(!err.to_string().contains("is installed"));
    }

    #[test]
    fn resolve_atom_error_mode_ignores_vdb() {
        let (_rdir, repo) = make_repo(&[("sys-apps", "foo"), ("app-misc", "foo")]);
        let (_vdir, vdb) = make_vdb(&[("sys-apps", "foo", "1.0")]);
        let err = resolve_atom(&repo, Some(&vdb), ResolveMode::Error, "foo").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
    }

    // --- resolve_atom: PreferInstalled mode ---

    #[test]
    fn resolve_atom_prefer_installed_single() {
        let (_rdir, repo) = make_repo(&[("sys-apps", "foo"), ("app-misc", "foo")]);
        let (_vdir, vdb) = make_vdb(&[("sys-apps", "foo", "1.0")]);

        let dep = resolve_atom(&repo, Some(&vdb), ResolveMode::PreferInstalled, "foo").unwrap();
        assert_eq!(dep.cpn.category.as_ref(), "sys-apps");
    }

    #[test]
    fn resolve_atom_prefer_installed_multiple_installed() {
        let (_rdir, repo) = make_repo(&[("sys-apps", "foo"), ("app-misc", "foo")]);
        let (_vdir, vdb) = make_vdb(&[("sys-apps", "foo", "1.0"), ("app-misc", "foo", "2.0")]);

        let err = resolve_atom(&repo, Some(&vdb), ResolveMode::PreferInstalled, "foo").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
    }

    #[test]
    fn resolve_atom_prefer_installed_none_installed() {
        let (_rdir, repo) = make_repo(&[("sys-apps", "foo"), ("app-misc", "foo")]);
        let (_vdir, vdb) = make_vdb(&[]);

        let err = resolve_atom(&repo, Some(&vdb), ResolveMode::PreferInstalled, "foo").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
    }

    #[test]
    fn resolve_atom_prefer_installed_no_vdb_falls_back_to_error() {
        let (_rdir, repo) = make_repo(&[("sys-apps", "foo"), ("app-misc", "foo")]);

        let err = resolve_atom(&repo, None, ResolveMode::PreferInstalled, "foo").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
    }

    // --- resolve_atoms: bulk ---

    #[test]
    fn resolve_atoms_mixed_input() {
        let (_dir, repo) = make_repo(&[("sys-apps", "foo"), ("app-misc", "bar")]);
        let atoms = vec!["sys-apps/foo".to_string(), "bar".to_string()];
        let result = resolve_atoms(&atoms, &repo, None, ResolveMode::Error);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn resolve_atoms_skips_invalid() {
        let (_dir, repo) = make_repo(&[("sys-apps", "foo")]);
        let atoms = vec!["foo".to_string(), "nonexistent".to_string()];
        let result = resolve_atoms(&atoms, &repo, None, ResolveMode::Error);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].cpn.package.as_ref(), "foo");
    }
}
