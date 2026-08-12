//! `em glsa` — Gentoo Linux Security Advisories.
//!
//! Mirrors real portage's `portage/glsa.py` (`Glsa`/`get_glsa_list`). GLSAs
//! ship pre-synced as `<repo>/metadata/glsa/glsa-<id>.xml`, so — unlike real
//! `glsa-check`, which can also fetch over the network — this only ever reads
//! the local tree.
//!
//! `Glsa::is_vulnerable` reproduces `glsa.py`'s `Glsa.isVulnerable`: for each
//! `<package>` block whose `arch` matches, take the installed versions that
//! match a `<vulnerable>` range and subtract the ones that also match an
//! `<unaffected>` range (`v_installed.difference(u_installed)` in the
//! Python). What's left is what `getMinUpgrade` would find non-`None` for —
//! `getMinUpgrade` returns `None` **only** when that difference is empty, so
//! the subtraction alone is the whole "is my system affected" answer; no
//! `portdb`/available-versions query is needed for `list`/`check`.
//!
//! `fix` does need one: it asks the main repo, not just the GLSA XML, for the
//! upgrade atom (`>=<cpn>-<unaffected version>`), then feeds those atoms to
//! the same [`crate::emerge_atoms`] path `em`'s other one-shot rebuilds use
//! (`revdep.rs`'s pattern) — a simpler "take the first applicable
//! `<unaffected>` entry" than `getMinUpgrade`'s least-change search among
//! every *available* version, but it converges on the same fixed version for
//! the overwhelmingly common case of a single `<unaffected range="ge">`.

use std::collections::BTreeSet;

use anyhow::{Context, Result, anyhow};
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::{Cpn, Cpv, Version};

use crate::cli::{Cli, GlsaCommand};
use crate::style::{C_ERROR, C_GOOD, C_WARN, einfo_line, warn_line};

#[derive(Clone, Copy)]
enum RangeOp {
    Le,
    Lt,
    Eq,
    Gt,
    Ge,
    /// Revision-scoped variants (`r*`): same base version, revision compared
    /// with the mapped operator. Real portage's `revisionMatch`.
    RLe,
    RLt,
    RGe,
    RGt,
}

fn parse_range_op(s: &str) -> Option<RangeOp> {
    Some(match s {
        "le" => RangeOp::Le,
        "lt" => RangeOp::Lt,
        "eq" => RangeOp::Eq,
        "gt" => RangeOp::Gt,
        "ge" => RangeOp::Ge,
        "rle" => RangeOp::RLe,
        "rlt" => RangeOp::RLt,
        "rge" => RangeOp::RGe,
        "rgt" => RangeOp::RGt,
        _ => return None,
    })
}

struct VersionRange {
    op: RangeOp,
    version: Version,
    slot: Option<String>,
}

fn parse_range(node: roxmltree::Node<'_, '_>) -> Option<VersionRange> {
    let op = parse_range_op(node.attribute("range")?)?;
    let version = Version::parse(collect_text(node).trim()).ok()?;
    let slot = node
        .attribute("slot")
        .map(str::to_string)
        .filter(|s| !s.is_empty() && s != "*");
    Some(VersionRange { op, version, slot })
}

/// Does `cpv`/`slot` (an installed package) match this range?
fn range_matches(range: &VersionRange, cpv: &Cpv, slot: &str) -> bool {
    if let Some(want_slot) = &range.slot
        && want_slot != slot
    {
        return false;
    }
    match range.op {
        RangeOp::Le => cpv.version <= range.version,
        RangeOp::Lt => cpv.version < range.version,
        RangeOp::Eq => cpv.version == range.version,
        RangeOp::Gt => cpv.version > range.version,
        RangeOp::Ge => cpv.version >= range.version,
        RangeOp::RLe | RangeOp::RLt | RangeOp::RGe | RangeOp::RGt => {
            let mut base_cand = cpv.version.clone();
            base_cand.revision = Default::default();
            let mut base_want = range.version.clone();
            base_want.revision = Default::default();
            if base_cand != base_want {
                return false;
            }
            let cand_rev = cpv.version.revision.0;
            let want_rev = range.version.revision.0;
            match range.op {
                RangeOp::RLe => cand_rev <= want_rev,
                RangeOp::RLt => cand_rev < want_rev,
                RangeOp::RGe => cand_rev >= want_rev,
                RangeOp::RGt => cand_rev > want_rev,
                _ => unreachable!(),
            }
        }
    }
}

struct GlsaPackage {
    name: Cpn,
    /// Raw `arch` attribute: `"*"` or a space-separated keyword list.
    arch: String,
    vulnerable: Vec<VersionRange>,
    unaffected: Vec<VersionRange>,
}

fn arch_matches(arch_field: &str, current: &str) -> bool {
    arch_field == "*" || arch_field.split_whitespace().any(|a| a == current)
}

pub(crate) struct Glsa {
    pub id: String,
    pub title: String,
    pub synopsis: String,
    packages: Vec<GlsaPackage>,
}

fn collect_text(node: roxmltree::Node<'_, '_>) -> String {
    let mut buf = String::new();
    for child in node.children() {
        if child.is_text() {
            if let Some(text) = child.text() {
                buf.push_str(text);
            }
        } else if child.is_element() {
            buf.push_str(&collect_text(child));
        }
    }
    buf.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_glsa(xml: &str) -> Result<Glsa> {
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(xml, opts)
        .map_err(|e| anyhow!("parsing GLSA xml: {e}"))?;
    let root = doc.root_element();
    let id = root.attribute("id").unwrap_or_default().to_string();
    let title = root
        .children()
        .find(|n| n.has_tag_name("title"))
        .map(collect_text)
        .unwrap_or_default();
    let synopsis = root
        .children()
        .find(|n| n.has_tag_name("synopsis"))
        .map(collect_text)
        .unwrap_or_default();
    let affected = root
        .children()
        .find(|n| n.has_tag_name("affected"))
        .context("GLSA missing <affected>")?;

    let mut packages = Vec::new();
    for pkg_node in affected.children().filter(|n| n.has_tag_name("package")) {
        let Some(name_attr) = pkg_node.attribute("name") else {
            continue;
        };
        let Ok(name) = Cpn::parse(name_attr) else {
            continue;
        };
        let arch = pkg_node.attribute("arch").unwrap_or("*").to_string();
        let vulnerable = pkg_node
            .children()
            .filter(|n| n.has_tag_name("vulnerable"))
            .filter_map(parse_range)
            .collect();
        let unaffected = pkg_node
            .children()
            .filter(|n| n.has_tag_name("unaffected"))
            .filter_map(parse_range)
            .collect();
        packages.push(GlsaPackage {
            name,
            arch,
            vulnerable,
            unaffected,
        });
    }

    Ok(Glsa {
        id,
        title,
        synopsis,
        packages,
    })
}

/// Installed versions (with slot) that this GLSA's `<package>` blocks say are
/// vulnerable-and-not-unaffected, per PMS-matching `arch`. Real portage's
/// `v_installed.difference(u_installed)`, grouped by which package/slot it
/// applies to.
fn affected(glsa: &Glsa, installed: &[(Cpv, String)], arch: &str) -> Vec<(Cpn, String)> {
    let mut out = Vec::new();
    for pkg in &glsa.packages {
        if !arch_matches(&pkg.arch, arch) {
            continue;
        }
        for (cpv, slot) in installed {
            if cpv.cpn != pkg.name {
                continue;
            }
            let vulnerable = pkg.vulnerable.iter().any(|r| range_matches(r, cpv, slot));
            let unaffected = pkg.unaffected.iter().any(|r| range_matches(r, cpv, slot));
            if vulnerable && !unaffected {
                out.push((pkg.name, slot.clone()));
            }
        }
    }
    out.sort_by(|a, b| (a.0.to_string(), &a.1).cmp(&(b.0.to_string(), &b.1)));
    out.dedup();
    out
}

/// Whether the system is affected by `glsa` and it hasn't been marked
/// resolved already ([`read_injected`]) — real portage's `Glsa.isInjected`
/// gate on top of `isVulnerable`.
fn is_vulnerable(
    glsa: &Glsa,
    installed: &[(Cpv, String)],
    arch: &str,
    injected: &BTreeSet<String>,
) -> bool {
    !injected.contains(&glsa.id) && !affected(glsa, installed, arch).is_empty()
}

/// `>=<cpn>-<version>[:slot]` for the first applicable `<unaffected>` entry
/// per affected `(cpn, slot)` — see the module doc's note on why this isn't
/// `getMinUpgrade`'s full least-change search.
fn fix_atoms(glsa: &Glsa, installed: &[(Cpv, String)], arch: &str) -> Vec<String> {
    let mut atoms = BTreeSet::new();
    for (cpn, slot) in affected(glsa, installed, arch) {
        let Some(pkg) = glsa.packages.iter().find(|p| p.name == cpn) else {
            continue;
        };
        let target = pkg
            .unaffected
            .iter()
            .find(|r| r.slot.as_deref().is_none_or(|s| s == slot));
        if let Some(target) = target {
            atoms.insert(format!(">={cpn}-{}", target.version));
        } else {
            warn_line!("{}: no fixed version known for {cpn}:{slot}", glsa.id);
        }
    }
    atoms.into_iter().collect()
}

fn glsa_dir(repo_path: &Utf8Path) -> Utf8PathBuf {
    repo_path.join("metadata/glsa")
}

/// Where `glsa_injected` lives for `eroot` — same bare-host/managed-root
/// split as [`crate::news::news_lib_dir`]: real portage's
/// `<EROOT>/var/lib/portage/glsa_injected` (`PRIVATE_PATH`) under a managed
/// `--root`/`--prefix`/`--local` target, XDG state on the bare host, so
/// `em glsa fix` marking a GLSA as applied doesn't need root just to
/// remember that.
fn injected_path(eroot: &Utf8Path) -> Utf8PathBuf {
    if eroot.as_str() == "/" {
        crate::xdg::glsa_state_dir().join("glsa_injected")
    } else {
        eroot.join("var/lib/portage/glsa_injected")
    }
}

/// GLSA ids previously marked applied (real portage's `get_applied_glsas`).
fn read_injected(eroot: &Utf8Path) -> BTreeSet<String> {
    std::fs::read_to_string(injected_path(eroot))
        .ok()
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Mark `ids` as applied, merging with whatever's already recorded (real
/// portage's `Glsa.inject`, one call per id — batched here since `fix`
/// always has the whole set up front).
fn mark_injected(eroot: &Utf8Path, ids: &[String]) -> Result<()> {
    let path = injected_path(eroot);
    let mut all = read_injected(eroot);
    all.extend(ids.iter().cloned());
    let body: String = all.iter().map(|i| format!("{i}\n")).collect();
    crate::util::write_atomic(&path, body.as_bytes()).with_context(|| format!("writing {path}"))
}

/// Every GLSA id available in the repo, sorted.
fn list_ids(repo_path: &Utf8Path) -> Result<Vec<String>> {
    let dir = glsa_dir(repo_path);
    let entries = std::fs::read_dir(dir.as_std_path()).with_context(|| format!("reading {dir}"))?;
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| {
            name.strip_prefix("glsa-")
                .and_then(|n| n.strip_suffix(".xml"))
                .map(str::to_string)
        })
        .collect();
    ids.sort();
    Ok(ids)
}

fn load(repo_path: &Utf8Path, id: &str) -> Result<Glsa> {
    let path = glsa_dir(repo_path).join(format!("glsa-{id}.xml"));
    let xml =
        std::fs::read_to_string(path.as_std_path()).with_context(|| format!("reading {path}"))?;
    parse_glsa(&xml).with_context(|| format!("parsing {path}"))
}

fn effective_arch(globals: &Cli) -> String {
    let make_conf = crate::select::config_portage_dir(globals).join("make.conf");
    if let Ok(conf) = portage_repo::MakeConf::load(&make_conf)
        && let Some(arch) = conf.get("ARCH").filter(|a| !a.is_empty())
    {
        return arch.to_string();
    }
    globals.arch.as_str().to_string()
}

fn installed_packages(globals: &Cli) -> Vec<(Cpv, String)> {
    crate::vdb::open_cli_vdb(globals)
        .map(|vdb| {
            vdb.packages()
                .into_iter()
                .map(|p| (p.cpv().clone(), p.slot_main().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default()
}

fn run_list(globals: &Cli) -> Result<()> {
    let repo_path = Utf8PathBuf::from(globals.repo_path());
    let ids = list_ids(&repo_path)?;
    let arch = effective_arch(globals);
    let installed = installed_packages(globals);
    let eroot = globals.outer_roots().merge_root().to_owned();
    let injected = read_injected(&eroot);

    let mut affected_count = 0usize;
    for id in &ids {
        let glsa = match load(&repo_path, id) {
            Ok(g) => g,
            Err(e) => {
                warn_line!("{id}: {e:#}");
                continue;
            }
        };
        if is_vulnerable(&glsa, &installed, &arch, &injected) {
            affected_count += 1;
            println!("{C_WARN}[A]{C_WARN:#} {id} {}", glsa.title);
        } else {
            println!("[N] {id} {}", glsa.title);
        }
    }
    println!();
    println!(
        "{affected_count} of {} GLSA(s) affect this system",
        ids.len()
    );
    Ok(())
}

fn run_check(globals: &Cli, ids: &[String]) -> Result<()> {
    let repo_path = Utf8PathBuf::from(globals.repo_path());
    let targets = if ids.is_empty() {
        list_ids(&repo_path)?
    } else {
        ids.to_vec()
    };
    let arch = effective_arch(globals);
    let installed = installed_packages(globals);
    let eroot = globals.outer_roots().merge_root().to_owned();
    let injected = read_injected(&eroot);

    let mut affected_count = 0usize;
    for id in &targets {
        let glsa = load(&repo_path, id)?;
        if is_vulnerable(&glsa, &installed, &arch, &injected) {
            affected_count += 1;
            println!("{C_ERROR}*{C_ERROR:#} {id} ({}) affected", glsa.title);
            println!("    {}", glsa.synopsis);
        } else if !ids.is_empty() {
            // Explicit id list: report the non-affected ones too.
            println!("{C_GOOD}-{C_GOOD:#} {id} ({}) not affected", glsa.title);
        }
    }
    if affected_count == 0 {
        einfo_line!("no affected GLSAs found");
    } else {
        println!();
        println!("{affected_count} GLSA(s) affect this system");
    }
    Ok(())
}

async fn run_fix(globals: &Cli, ids: &[String]) -> Result<()> {
    let repo_path = Utf8PathBuf::from(globals.repo_path());
    let targets = if ids.is_empty() {
        list_ids(&repo_path)?
    } else {
        ids.to_vec()
    };
    let arch = effective_arch(globals);
    let installed = installed_packages(globals);
    let eroot = globals.outer_roots().merge_root().to_owned();
    let injected = read_injected(&eroot);

    let mut atoms = BTreeSet::new();
    let mut fixed_ids = Vec::new();
    for id in &targets {
        if injected.contains(id) {
            continue;
        }
        let glsa = load(&repo_path, id)?;
        let glsa_atoms = fix_atoms(&glsa, &installed, &arch);
        if !glsa_atoms.is_empty() {
            fixed_ids.push(id.clone());
            atoms.extend(glsa_atoms);
        }
    }

    if atoms.is_empty() {
        einfo_line!("no affected GLSAs with a known fix found");
        return Ok(());
    }

    einfo_line!(
        "fixing {} GLSA(s): {}",
        fixed_ids.len(),
        fixed_ids.join(", ")
    );

    let atoms: Vec<String> = atoms.into_iter().collect();
    let mut merge_flags = globals.merge_flags.clone();
    merge_flags.oneshot = true;
    merge_flags.complete_graph = true;

    crate::emerge_atoms(
        globals,
        &atoms,
        crate::EmergeOpts {
            use_override: &[],
            nodeps: false,
            depgraph_flags: None,
            merge_flags: Some(merge_flags),
            use_outer_eroot: false,
            target_only_installed_view: false,
            update_world: false,
            is_resume: false,
            activity: None,
            activity_session: Default::default(),
            extra_aliases: &[],
        },
    )
    .await?;

    // `--pretend` doesn't actually merge anything — don't record GLSAs as
    // resolved for a run that resolved nothing.
    if globals.pretend {
        return Ok(());
    }
    mark_injected(&eroot, &fixed_ids)
}

pub(crate) async fn run(command: &Option<GlsaCommand>, globals: &Cli) -> Result<()> {
    match command {
        None | Some(GlsaCommand::List) => run_list(globals),
        Some(GlsaCommand::Check { ids }) => run_check(globals, ids),
        Some(GlsaCommand::Fix { ids }) => run_fix(globals, ids).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APACHE_GLSA: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE glsa SYSTEM "http://www.gentoo.org/dtd/glsa.dtd">
<glsa id="200310-03">
  <title>Apache: multiple buffer overflows</title>
  <synopsis>Multiple stack-based buffer overflows.</synopsis>
  <affected>
    <package name="www-servers/apache" auto="yes" arch="*">
      <unaffected range="ge">1.3.29</unaffected>
      <vulnerable range="lt">1.3.29</vulnerable>
    </package>
  </affected>
</glsa>"#;

    fn cpv(s: &str) -> Cpv {
        Cpv::parse(s).unwrap()
    }

    fn no_injections() -> BTreeSet<String> {
        BTreeSet::new()
    }

    #[test]
    fn parses_id_title_and_ranges() {
        let glsa = parse_glsa(APACHE_GLSA).unwrap();
        assert_eq!(glsa.id, "200310-03");
        assert_eq!(glsa.title, "Apache: multiple buffer overflows");
        assert_eq!(glsa.packages.len(), 1);
        assert_eq!(glsa.packages[0].vulnerable.len(), 1);
        assert_eq!(glsa.packages[0].unaffected.len(), 1);
    }

    #[test]
    fn old_installed_version_is_vulnerable() {
        let glsa = parse_glsa(APACHE_GLSA).unwrap();
        let installed = vec![(cpv("www-servers/apache-1.3.27"), "0".to_string())];
        assert!(is_vulnerable(&glsa, &installed, "amd64", &no_injections()));
    }

    #[test]
    fn fixed_installed_version_is_not_vulnerable() {
        let glsa = parse_glsa(APACHE_GLSA).unwrap();
        let installed = vec![(cpv("www-servers/apache-1.3.29"), "0".to_string())];
        assert!(!is_vulnerable(&glsa, &installed, "amd64", &no_injections()));
    }

    #[test]
    fn uninstalled_package_is_not_vulnerable() {
        let glsa = parse_glsa(APACHE_GLSA).unwrap();
        let installed = vec![(cpv("www-servers/nginx-1.20.0"), "0".to_string())];
        assert!(!is_vulnerable(&glsa, &installed, "amd64", &no_injections()));
    }

    #[test]
    fn fix_atoms_targets_the_unaffected_version() {
        let glsa = parse_glsa(APACHE_GLSA).unwrap();
        let installed = vec![(cpv("www-servers/apache-1.3.27"), "0".to_string())];
        let atoms = fix_atoms(&glsa, &installed, "amd64");
        assert_eq!(atoms, vec![">=www-servers/apache-1.3.29".to_string()]);
    }

    #[test]
    fn revision_scoped_range_matches_same_base_version_only() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE glsa SYSTEM "http://www.gentoo.org/dtd/glsa.dtd">
<glsa id="200001-01">
  <title>t</title>
  <synopsis>s</synopsis>
  <affected>
    <package name="app-misc/foo" auto="yes" arch="*">
      <unaffected range="rge">1.2.3-r5</unaffected>
      <vulnerable range="rlt">1.2.3-r5</vulnerable>
    </package>
  </affected>
</glsa>"#;
        let glsa = parse_glsa(xml).unwrap();
        // Same base version, lower revision: vulnerable.
        let low_rev = vec![(cpv("app-misc/foo-1.2.3-r2"), "0".to_string())];
        assert!(is_vulnerable(&glsa, &low_rev, "amd64", &no_injections()));
        // Same base version, revision at the fix: not vulnerable.
        let fixed = vec![(cpv("app-misc/foo-1.2.3-r5"), "0".to_string())];
        assert!(!is_vulnerable(&glsa, &fixed, "amd64", &no_injections()));
        // Different base version entirely: the r*-range never matches.
        let other_version = vec![(cpv("app-misc/foo-1.9.0"), "0".to_string())];
        assert!(!is_vulnerable(
            &glsa,
            &other_version,
            "amd64",
            &no_injections()
        ));
    }

    #[test]
    fn arch_restriction_excludes_non_matching_hosts() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE glsa SYSTEM "http://www.gentoo.org/dtd/glsa.dtd">
<glsa id="200001-02">
  <title>t</title>
  <synopsis>s</synopsis>
  <affected>
    <package name="app-misc/foo" auto="yes" arch="x86">
      <unaffected range="ge">2.0</unaffected>
      <vulnerable range="lt">2.0</vulnerable>
    </package>
  </affected>
</glsa>"#;
        let glsa = parse_glsa(xml).unwrap();
        let installed = vec![(cpv("app-misc/foo-1.0"), "0".to_string())];
        assert!(is_vulnerable(&glsa, &installed, "x86", &no_injections()));
        assert!(!is_vulnerable(&glsa, &installed, "amd64", &no_injections()));
    }

    #[test]
    fn injected_glsa_is_no_longer_reported_vulnerable() {
        let glsa = parse_glsa(APACHE_GLSA).unwrap();
        let installed = vec![(cpv("www-servers/apache-1.3.27"), "0".to_string())];
        assert!(is_vulnerable(&glsa, &installed, "amd64", &no_injections()));
        let mut injected = BTreeSet::new();
        injected.insert(glsa.id.clone());
        assert!(!is_vulnerable(&glsa, &installed, "amd64", &injected));
    }

    #[test]
    fn injected_path_uses_xdg_state_for_the_bare_host() {
        let dir = injected_path(Utf8Path::new("/"));
        assert!(
            dir.ends_with("em/glsa/glsa_injected"),
            "expected an XDG state path, got {dir}"
        );
    }

    #[test]
    fn injected_path_uses_the_real_path_under_a_managed_root() {
        let dir = injected_path(Utf8Path::new("/tmp/some-root"));
        assert_eq!(dir.as_str(), "/tmp/some-root/var/lib/portage/glsa_injected");
    }
}
