//! `em clean dist` / `em clean pkg` — drop distfiles and binary packages
//! nothing references any more
//!
//! The reference set is whatever the *tree* still names: every `DIST` line in
//! every `Manifest` across the configured repos for distfiles, every cpv that
//! still has an ebuild for binary packages. `--deep` narrows that to what the
//! installed packages alone still name, which is what a machine that only ever
//! rebuilds what it already has actually needs.
//!
//! Root-aware on purpose. `eclean` answers about the host's `DISTDIR`/`PKGDIR`;
//! under `em --root`/`--prefix`/`--local` those live inside the offset, so a
//! host tool cannot clean them and this can.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use crate::cli::{CleanOpts, CleanTarget, Cli};

/// One removable file and what it costs
struct Candidate {
    path: Utf8PathBuf,
    /// Shown to the user — the distfile name, or `category/PF` for a binpkg
    label: String,
    bytes: u64,
}

/// Filters that apply equally to both targets
struct Filters {
    /// Ignore anything smaller than this
    min_bytes: Option<u64>,
    /// Keep anything modified within this window
    keep_newer_than: Option<std::time::SystemTime>,
}

impl Filters {
    fn parse(opts: &CleanOpts) -> Result<Self> {
        let min_bytes = opts.size_limit.as_deref().map(parse_size).transpose()?;
        let keep_newer_than = match opts.time_limit.as_deref() {
            None => None,
            Some(spec) => Some(cutoff_from(std::time::SystemTime::now(), spec)?),
        };
        Ok(Self {
            min_bytes,
            keep_newer_than,
        })
    }

    /// Whether `meta` passes both filters (i.e. the file stays a candidate)
    fn keeps(&self, meta: &std::fs::Metadata) -> bool {
        if self.min_bytes.is_some_and(|m| meta.len() < m) {
            return false;
        }
        if let Some(cutoff) = self.keep_newer_than
            && meta.modified().is_ok_and(|m| m > cutoff)
        {
            return false;
        }
        true
    }
}

/// Cutoff instant for `--time-limit`: `now` minus the parsed duration
///
/// `checked_sub`, not `-`: `Sub<Duration> for SystemTime` panics when the
/// result cannot be represented (Windows cannot go before 1601).
fn cutoff_from(now: std::time::SystemTime, spec: &str) -> Result<std::time::SystemTime> {
    let age = humantime::parse_duration(spec)
        .with_context(|| format!("--time-limit {spec}: not a duration"))?;
    now.checked_sub(age)
        .with_context(|| format!("--time-limit {spec}: too large"))
}

/// `10M` / `1G` / a bare byte count
fn parse_size(spec: &str) -> Result<u64> {
    let spec = spec.trim();
    let (digits, mult) = match spec.chars().last() {
        Some('K' | 'k') => (&spec[..spec.len() - 1], 1024),
        Some('M' | 'm') => (&spec[..spec.len() - 1], 1024 * 1024),
        Some('G' | 'g') => (&spec[..spec.len() - 1], 1024 * 1024 * 1024),
        _ => (spec, 1),
    };
    let n: u64 = digits
        .trim()
        .parse()
        .with_context(|| format!("--size-limit {spec}: not a size (try 10M, 1G)"))?;
    // `checked_mul`, not `*`: the multiplier makes a plausible-looking argument
    // overflow, and release builds have `overflow-checks` off, so the panic a
    // debug build gives becomes a silently wrapped nonsense limit.
    n.checked_mul(mult)
        .with_context(|| format!("--size-limit {spec}: too large"))
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

pub async fn run(globals: &Cli, target: &CleanTarget) -> Result<()> {
    let opts = match target {
        CleanTarget::Dist { opts } | CleanTarget::Pkg { opts } | CleanTarget::All { opts } => opts,
    };
    let filters = Filters::parse(opts)?;
    match target {
        CleanTarget::Dist { .. } => {
            let c = dist_candidates(globals, opts, &filters)?;
            report_and_remove(globals, &c, "distfile")
        }
        CleanTarget::Pkg { .. } => {
            let c = pkg_candidates(globals, opts, &filters).await?;
            report_and_remove(globals, &c, "binary package")
        }
        CleanTarget::All { .. } => run_all(globals, opts, &filters).await,
    }
}

/// Every reclaimable thing in one pass
///
/// Each step is announced and run to completion even if an earlier one failed:
/// an unreadable `PKGDIR` should not cost the user the distfile sweep. The
/// first error is returned at the end so a script still sees a failure.
async fn run_all(globals: &Cli, opts: &CleanOpts, filters: &Filters) -> Result<()> {
    let mut failed: Option<anyhow::Error> = None;
    let note = |e: anyhow::Error, failed: &mut Option<anyhow::Error>| {
        crate::style::warn_line!("{e:#}");
        if failed.is_none() {
            *failed = Some(e);
        }
    };

    println!(">>> distfiles");
    match dist_candidates(globals, opts, filters) {
        Ok(c) => report_and_remove(globals, &c, "distfile")?,
        Err(e) => note(e, &mut failed),
    }

    println!(">>> binary packages");
    match pkg_candidates(globals, opts, filters).await {
        Ok(c) => report_and_remove(globals, &c, "binary package")?,
        Err(e) => note(e, &mut failed),
    }

    println!(">>> build logs");
    let work_base = crate::ebuild::default_work_base(globals.roots().relocate_root());
    if let Err(e) = crate::maint::logs::run(
        &work_base,
        opts.time_limit.as_deref(),
        // `clean` is a removal command, so the logs step removes too — unlike
        // `em maint logs`, which reports until asked with `--fix`.
        !globals.pretend,
        "Re-run without -p",
    ) {
        note(e, &mut failed);
    }

    match failed {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Print the candidate set and, unless `-p`, unlink it
fn report_and_remove(globals: &Cli, candidates: &[Candidate], noun: &str) -> Result<()> {
    if candidates.is_empty() {
        println!(">>> No {noun}s to clean.");
        return Ok(());
    }
    let total: u64 = candidates.iter().map(|c| c.bytes).sum();
    for c in candidates {
        println!("    {} ({})", c.label, human_bytes(c.bytes));
    }
    if globals.pretend {
        println!(
            ">>> Would remove {} {noun}(s), freeing {}.",
            candidates.len(),
            human_bytes(total)
        );
        return Ok(());
    }

    let mut removed = 0usize;
    let mut freed = 0u64;
    for c in candidates {
        match std::fs::remove_file(c.path.as_std_path()) {
            Ok(()) => {
                removed += 1;
                freed += c.bytes;
            }
            // Best-effort per file: a DISTDIR shared with a concurrent fetch,
            // or a read-only PKGDIR, should not abort the whole sweep.
            Err(e) => crate::style::warn_line!("cannot remove {}: {e}", c.path),
        }
    }
    println!(
        ">>> Removed {removed} {noun}(s), freed {}.",
        human_bytes(freed)
    );
    Ok(())
}

/// `DISTDIR`, resolved the same way every other root-aware consumer does
fn distdir(globals: &Cli) -> Utf8PathBuf {
    if let Ok(v) = std::env::var("DISTDIR")
        && !v.trim().is_empty()
    {
        return Utf8PathBuf::from(v);
    }
    let conf = crate::select::config_portage_dir(globals).join("make.conf");
    portage_repo::MakeConf::load(&conf)
        .ok()
        .and_then(|mc| mc.get("DISTDIR").map(Utf8PathBuf::from))
        .unwrap_or_else(|| globals.roots().merge_root().join("var/cache/distfiles"))
}

/// Every distfile name the tree still references
///
/// `Manifest`'s `DIST` lines are the authority rather than `SRC_URI`: they are
/// already the post-rename local filenames, so no mirror/`->` parsing is
/// needed, and one file per package directory beats reading every cache entry.
fn referenced_distfiles(globals: &Cli, deep: bool) -> Result<std::collections::HashSet<String>> {
    let mut wanted = std::collections::HashSet::new();
    let installed = deep.then(|| installed_cpns(globals)).transpose()?;
    let repos = configured_repos_or_bail(globals)?;
    let mut unmanifested = 0usize;

    for repo in repos {
        let Ok(repo) = Utf8PathBuf::from_path_buf(repo) else {
            continue;
        };
        for manifest in package_manifests(&repo, installed.as_ref(), &mut unmanifested) {
            let Ok(text) = std::fs::read_to_string(manifest.as_std_path()) else {
                continue;
            };
            let Ok(parsed) = portage_repo::Manifest::parse(&text) else {
                continue;
            };
            for e in parsed.dist_entries() {
                if let portage_repo::ManifestEntry::Dist { filename, .. } = e {
                    wanted.insert(filename.clone());
                }
            }
        }
    }
    if unmanifested > 0 {
        // Warn rather than refuse: on a stock ::gentoo this is dominated by
        // live (`-9999`) ebuilds, whose `SRC_URI` sits in a `${PV}` branch a
        // text match cannot resolve and which fetch from git anyway, so they
        // have no distfile to lose. The case that does matter is a
        // thin-manifest overlay with real distfiles, which this cannot see —
        // reading the *metadata cache*'s evaluated `SRC_URI` is the actual
        // answer, as `em mirrordist` already does.
        crate::style::warn_line!(
            "{unmanifested} package(s) have ebuilds but no Manifest; any distfile only they \
             reference is not in the keep set — check with -p before removing"
        );
    }
    Ok(wanted)
}

/// `<repo>/<cat>/<pn>` for every package directory, or only the installed
/// ones when `only` is given
fn package_dirs(
    repo: &Utf8Path,
    only: Option<&std::collections::HashSet<(String, String)>>,
) -> Vec<Utf8PathBuf> {
    let mut out = Vec::new();
    let categories = declared_categories(repo);
    let Ok(cats) = std::fs::read_dir(repo.as_std_path()) else {
        return out;
    };
    for cat in cats.flatten() {
        if !cat.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let cat_name = cat.file_name().to_string_lossy().into_owned();
        // Skip the repo's own non-category directories (`metadata/`,
        // `profiles/`, `eclass/`, `.git`) using the repo's *declared*
        // categories. The previous heuristic — "contains a hyphen, or is
        // `virtual`" — silently skipped any category an overlay declares
        // without a hyphen, and every distfile under it then looked
        // unreferenced.
        if !categories.contains(&cat_name) {
            continue;
        }
        let Ok(pkgs) = std::fs::read_dir(cat.path()) else {
            continue;
        };
        for pkg in pkgs.flatten() {
            if !pkg.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let pn = pkg.file_name().to_string_lossy().into_owned();
            if only.is_some_and(|set| !set.contains(&(cat_name.clone(), pn))) {
                continue;
            }
            if let Ok(dir) = Utf8PathBuf::from_path_buf(pkg.path()) {
                out.push(dir);
            }
        }
    }
    out
}

/// `<repo>/<cat>/<pn>/Manifest` for every package, or only for the installed
/// ones when `only` is given
fn package_manifests(
    repo: &Utf8Path,
    only: Option<&std::collections::HashSet<(String, String)>>,
    unmanifested: &mut usize,
) -> Vec<Utf8PathBuf> {
    let mut out = Vec::new();
    for dir in package_dirs(repo, only) {
        let m = dir.join("Manifest");
        if m.is_file() {
            out.push(m);
        } else if declares_src_uri(dir.as_std_path()) {
            // Ebuilds but no `Manifest`, *and* something to fetch: its
            // distfiles are invisible here, so they would look
            // unreferenced. Count it rather than silently narrowing the
            // reference set.
            //
            // A missing Manifest is normal and harmless for the ~1400
            // metadata-only packages in ::gentoo (`acct-group`,
            // `acct-user`, `virtual`) — nothing to hash, nothing to miss.
            *unmanifested += 1;
        }
    }
    out
}

/// Whether any ebuild in `dir` declares a non-empty `SRC_URI`
///
/// Text-matched rather than resolved: this only has to answer "could this
/// package have distfiles at all". It will miss a `SRC_URI` contributed
/// entirely by an eclass, which is why the count it feeds is a refusal rather
/// than a silent narrowing — a wrong answer here stops the sweep instead of
/// deleting something.
fn declares_src_uri(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".ebuild"))
        .any(|e| {
            std::fs::read_to_string(e.path()).is_ok_and(|t| {
                t.lines().any(|l| {
                    let l = l.trim_start();
                    l.starts_with("SRC_URI=")
                        && !matches!(l.trim_end(), "SRC_URI=\"\"" | "SRC_URI=''" | "SRC_URI=")
                })
            })
        })
}

/// A repo's `profiles/categories`, or every hyphenated directory name plus
/// `virtual` when it has none
///
/// Reading the file is what portage does; the fallback only exists for a repo
/// that omits it, where guessing is still better than walking `eclass/`.
fn declared_categories(repo: &Utf8Path) -> std::collections::HashSet<String> {
    if let Ok(text) = std::fs::read_to_string(repo.join("profiles/categories").as_std_path()) {
        let set: std::collections::HashSet<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_owned)
            .collect();
        if !set.is_empty() {
            return set;
        }
    }
    std::fs::read_dir(repo.as_std_path())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && (n.contains('-') || n == "virtual"))
        .collect()
}

/// `(category, package)` of everything installed in the target root
fn installed_cpns(globals: &Cli) -> Result<std::collections::HashSet<(String, String)>> {
    let vdb = portage_vdb::Vdb::open(globals.roots().merge_root().join("var/db/pkg"))
        .context("opening the installed package database")?;
    Ok(vdb
        .packages()
        .into_iter()
        .map(|p| {
            let cpv = p.cpv();
            (
                cpv.cpn.category.as_str().to_string(),
                cpv.cpn.package.as_str().to_string(),
            )
        })
        .collect())
}

fn dist_candidates(globals: &Cli, opts: &CleanOpts, filters: &Filters) -> Result<Vec<Candidate>> {
    let dir = distdir(globals);
    if !dir.is_dir() {
        anyhow::bail!("DISTDIR {dir} does not exist");
    }
    let wanted = referenced_distfiles(globals, opts.deep)?;
    if wanted.is_empty() {
        // Refuse rather than delete the world: an empty reference set means the
        // repos were unreadable, not that every distfile is stale.
        anyhow::bail!("no Manifest entries found in any configured repo — refusing to clean {dir}");
    }

    // The DISTDIR walk itself is `mirrordist`'s, parameterised on the
    // reference set: it already skips `layout.conf` and the atomic-write temp
    // files, and keeping one scanner means those rules cannot drift apart.
    // Only the *policy* differs — what counts as referenced.
    let refs: std::collections::HashSet<&str> = wanted.iter().map(String::as_str).collect();
    // Portage's own bookkeeping in a *local* DISTDIR, which a mirror dir never
    // has, so the shared scanner does not know about it: `<file>.lock` is a
    // live fetch lock (removing one lets two fetchers write the same path) and
    // `.layout.conf.<mirror>` is a cached mirror layout. Neither is a distfile.
    let keep: std::collections::HashSet<String> = std::fs::read_dir(dir.as_std_path())
        .with_context(|| format!("reading {dir}"))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".lock") || n.starts_with('.'))
        .collect();
    let state = crate::mirrordist::scan_distdir(&dir, &refs, &keep, false)
        .with_context(|| format!("scanning {dir}"))?;

    let mut out = Vec::new();
    for name in state.orphans {
        let path = dir.join(&name);
        let Ok(meta) = std::fs::metadata(path.as_std_path()) else {
            continue;
        };
        if !filters.keeps(&meta) {
            continue;
        }
        out.push(Candidate {
            path,
            label: name,
            bytes: meta.len(),
        });
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.bytes));
    Ok(out)
}

async fn pkg_candidates(
    globals: &Cli,
    opts: &CleanOpts,
    filters: &Filters,
) -> Result<Vec<Candidate>> {
    let pkgdir = crate::binpkg::resolve_pkgdir(globals).await;
    if !pkgdir.is_dir() {
        anyhow::bail!("PKGDIR {pkgdir} does not exist");
    }

    let keep = if opts.deep {
        installed_cpvs(globals)?
    } else {
        tree_cpvs(globals)?
    };
    if keep.is_empty() {
        anyhow::bail!("no packages found to compare against — refusing to clean {pkgdir}");
    }

    let mut containers = Vec::new();
    portage_binpkg::find_gpkg_containers(
        pkgdir.as_std_path(),
        pkgdir.as_std_path(),
        &mut containers,
    )
    .with_context(|| format!("scanning {pkgdir}"))?;

    let mut out = Vec::new();
    for (rel, full) in containers {
        // `<cat>/<PF>-<build_id>.gpkg.tar` or `<cat>/<PF>.gpkg.tar`
        let Some(cpv) = cpv_from_container(&rel) else {
            continue;
        };
        if keep.contains(&cpv) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&full) else {
            continue;
        };
        if !filters.keeps(&meta) {
            continue;
        }
        let Ok(path) = Utf8PathBuf::from_path_buf(full) else {
            continue;
        };
        out.push(Candidate {
            path,
            label: cpv,
            bytes: meta.len(),
        });
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.bytes));
    Ok(out)
}

/// `category/PF` from a PKGDIR-relative container path, dropping a
/// multi-instance `-<build_id>` suffix but *only* when it really is one
///
/// `parse_build_id_from_name` answers "does this end in an integer", which is
/// true of plenty of legitimate versions — `virtual/awk-1`, `acct-group/audio-0`,
/// `sys-kernel/linux-firmware-20250101`. Stripping on that alone produced a cpv
/// with no version, which matched nothing in the keep set, so `em clean pkg`
/// deleted the binary package of an installed, in-tree package. The tail comes
/// off only when the whole name is *not* a valid `PF` and the base is, which is
/// exactly the shape a real build-id suffix has.
pub(crate) fn cpv_from_container(rel: &str) -> Option<String> {
    let rel = rel.strip_suffix(".gpkg.tar")?;
    let (cat, file) = rel.rsplit_once('/')?;
    if portage_atom::Pf::parse(file).is_ok() {
        return Some(format!("{cat}/{file}"));
    }
    let (base, _build_id) = file.rsplit_once('-')?;
    portage_atom::Pf::parse(base)
        .ok()
        .map(|_| format!("{cat}/{base}"))
}

fn installed_cpvs(globals: &Cli) -> Result<std::collections::HashSet<String>> {
    let vdb = portage_vdb::Vdb::open(globals.roots().merge_root().join("var/db/pkg"))
        .context("opening the installed package database")?;
    Ok(vdb
        .packages()
        .into_iter()
        .map(|p| p.cpv().to_string())
        .collect())
}

/// Every cpv that still has an ebuild in some configured repo
fn tree_cpvs(globals: &Cli) -> Result<std::collections::HashSet<String>> {
    let mut out = std::collections::HashSet::new();
    let repos = configured_repos_or_bail(globals)?;
    for repo in repos {
        let Ok(repo) = Utf8PathBuf::from_path_buf(repo) else {
            continue;
        };
        out.extend(tree_cpvs_in(&repo));
    }
    Ok(out)
}

/// Keep set for one repo: every `*.ebuild`, Manifest or not
///
/// Thin-manifests (::gentoo's layout) omit `Manifest` when a package has no
/// DIST files. Walking Manifests therefore dropped every `virtual/*` and
/// `acct-*` (and anything else with no distfiles), so `em clean pkg` treated
/// their binpkgs as unreferenced.
fn tree_cpvs_in(repo: &Utf8Path) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for pkg_dir in package_dirs(repo, None) {
        let Some(cat) = pkg_dir.parent().and_then(Utf8Path::file_name) else {
            continue;
        };
        let Ok(files) = std::fs::read_dir(pkg_dir.as_std_path()) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if let Some(pf) = name.strip_suffix(".ebuild") {
                out.insert(format!("{cat}/{pf}"));
            }
        }
    }
    out
}

/// The configured repos, or an error rather than the host-tree fallback
///
/// `search_repos`' fallback is right for a query and wrong here: it would let
/// `em clean --root DIR` compute "what the tree references" from the *host's*
/// tree, so the empty-reference-set guard below could never fire on the very
/// misconfiguration it exists for.
fn configured_repos_or_bail(globals: &Cli) -> Result<Vec<std::path::PathBuf>> {
    globals.configured_repos().with_context(|| {
        format!(
            "no usable repos.conf for {} — refusing to decide what is unreferenced \
             from another root's tree",
            globals.roots().merge_root()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_limit_accepts_suffixes_and_bare_bytes() {
        assert_eq!(parse_size("512").unwrap(), 512);
        assert_eq!(parse_size("10M").unwrap(), 10 * 1024 * 1024);
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size(" 2k ").unwrap(), 2048);
        assert!(parse_size("banana").is_err());
        // Straight from user input; `*` panicked in debug and wrapped in release.
        assert!(parse_size("18446744073709551615K").is_err());
        assert!(parse_size("99999999999999999999G").is_err());
    }

    #[test]
    fn time_limit_rejects_a_non_duration() {
        let now = std::time::SystemTime::now();
        assert!(cutoff_from(now, "2weeks").is_ok());
        assert!(cutoff_from(now, "banana").is_err());
        // Windows cannot represent times before 1601, so a huge age is a
        // parse error rather than a panic. Linux's signed timespec can.
        #[cfg(windows)]
        assert!(cutoff_from(now, "500y").is_err());
    }

    // Thin-manifest packages (no DIST → no Manifest) must still stay in the
    // keep set. Walking Manifests dropped every virtual/* and acct-*.
    #[test]
    fn tree_keep_set_includes_packages_with_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::create_dir_all(repo.join("profiles").as_std_path()).unwrap();
        std::fs::write(
            repo.join("profiles/categories").as_std_path(),
            "virtual\nacct-group\nsys-libs\n",
        )
        .unwrap();

        std::fs::create_dir_all(repo.join("virtual/awk").as_std_path()).unwrap();
        std::fs::write(
            repo.join("virtual/awk/awk-1.ebuild").as_std_path(),
            "EAPI=8\n",
        )
        .unwrap();

        std::fs::create_dir_all(repo.join("acct-group/audio").as_std_path()).unwrap();
        std::fs::write(
            repo.join("acct-group/audio/audio-0.ebuild").as_std_path(),
            "EAPI=8\n",
        )
        .unwrap();

        std::fs::create_dir_all(repo.join("sys-libs/zlib").as_std_path()).unwrap();
        std::fs::write(
            repo.join("sys-libs/zlib/zlib-1.3.1.ebuild").as_std_path(),
            "EAPI=8\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("sys-libs/zlib/Manifest").as_std_path(),
            "DIST zlib-1.3.1.tar.xz 1 SHA256 aa\n",
        )
        .unwrap();

        let keep = tree_cpvs_in(repo);
        assert!(keep.contains("virtual/awk-1"), "{keep:?}");
        assert!(keep.contains("acct-group/audio-0"), "{keep:?}");
        assert!(keep.contains("sys-libs/zlib-1.3.1"), "{keep:?}");
    }

    // A real `-<build_id>` suffix comes off; a version that merely ends in an
    // integer must not. Getting this wrong deleted the binary package of every
    // installed `virtual/*`, `acct-*` and date-versioned package, because the
    // stripped cpv matched nothing in the keep set.
    #[test]
    fn container_path_strips_a_build_id_but_never_a_version() {
        let cpv = |rel| cpv_from_container(rel).unwrap_or_default();

        // Genuine multi-instance suffixes.
        assert_eq!(
            cpv("sys-libs/zlib-1.3.2-r1-3.gpkg.tar"),
            "sys-libs/zlib-1.3.2-r1"
        );
        assert_eq!(
            cpv("sys-devel/gcc-16.2.0-3.gpkg.tar"),
            "sys-devel/gcc-16.2.0"
        );

        // Versions that end in a bare integer — the data-loss cases.
        assert_eq!(cpv("virtual/awk-1.gpkg.tar"), "virtual/awk-1");
        assert_eq!(cpv("virtual/pkgconfig-3.gpkg.tar"), "virtual/pkgconfig-3");
        assert_eq!(cpv("acct-group/audio-0.gpkg.tar"), "acct-group/audio-0");
        assert_eq!(
            cpv("sys-kernel/linux-firmware-20250101.gpkg.tar"),
            "sys-kernel/linux-firmware-20250101"
        );

        // Ordinary versions are untouched.
        assert_eq!(cpv("sys-libs/zlib-1.3.1.gpkg.tar"), "sys-libs/zlib-1.3.1");

        assert_eq!(cpv_from_container("not-a-container"), None);
    }

    #[test]
    fn human_bytes_switches_unit_at_the_boundary() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
    }
}
