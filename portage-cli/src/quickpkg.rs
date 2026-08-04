//! `em quickpkg` — package installed files into a GPKG under PKGDIR.
//!
//! Portage's `quickpkg(1)` builds a binary package from the live filesystem
//! using the installed package's VDB `CONTENTS` (not a build image). Default
//! behaviour skips `CONFIG_PROTECT` paths (security: live configs may hold
//! secrets); `--include-config y` packs them, and `--include-unmodified-config y`
//! re-includes protected files whose on-disk MD5 still matches CONTENTS.

use std::collections::BTreeSet;
use std::io::Write;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_vdb::{ContentsEntry, ContentsKind, InstalledPackage, Vdb};

use crate::binpkg::{read_make_conf_var, resolve_pkgdir};
use crate::cli::Cli;
use crate::emerge::expand_sets;
use crate::vdb::{find_packages, open_cli_vdb};

/// CLI options for `em quickpkg`.
#[derive(Debug, Clone)]
pub(crate) struct QuickpkgOpts {
    /// Atom / set / VDB-path arguments.
    pub atoms: Vec<String>,
    /// Include all CONFIG_PROTECT files (default: false).
    pub include_config: bool,
    /// When `include_config` is false, still include protected files whose
    /// live MD5 matches the CONTENTS record (default: false).
    pub include_unmodified_config: bool,
}

/// Run `em quickpkg`.
pub(crate) async fn run(globals: &Cli, opts: &QuickpkgOpts) -> Result<()> {
    // Match real quickpkg's default umask (0077) so packages aren't world-readable.
    let prev = rustix::process::umask(rustix::fs::Mode::from_bits_truncate(0o077));
    let result = run_inner(globals, opts).await;
    rustix::process::umask(prev);
    result
}

async fn run_inner(globals: &Cli, opts: &QuickpkgOpts) -> Result<()> {
    let roots = globals.roots();
    let merge_root = roots.merge_root();
    let vdb = open_cli_vdb(globals)?;
    let pkgdir = resolve_pkgdir(globals).await;
    std::fs::create_dir_all(pkgdir.as_std_path())
        .with_context(|| format!("creating PKGDIR {pkgdir}"))?;

    // quickpkg operates on the installed set only — set provenance carries no
    // meaning here, so the expansion is flattened straight back to atoms.
    let expanded: Vec<String> = expand_sets(&opts.atoms, roots.config(), merge_root)
        .into_iter()
        .map(|t| t.atom)
        .collect();
    let protect = ConfigProtectLists::load(globals).await;

    let mut successes: Vec<(String, u64)> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut failed = 0usize;

    // Dedup packages when multiple args match the same CPV.
    let mut seen = BTreeSet::new();

    for arg in &expanded {
        let pkgs = resolve_arg(&vdb, arg, merge_root);
        if pkgs.is_empty() {
            crate::style::warn_line(&format!("no installed package matches '{arg}'"));
            missing.push(arg.clone());
            continue;
        }
        for pkg in pkgs {
            let cpv = pkg.cpv().to_string();
            if !seen.insert(cpv.clone()) {
                continue;
            }
            match package_one(
                &pkg,
                merge_root,
                &pkgdir,
                &protect,
                opts.include_config,
                opts.include_unmodified_config,
            ) {
                Ok((out, size, excluded)) => {
                    println!(">>> Built package for {cpv}");
                    println!("    {out} ({size} bytes)");
                    if !excluded.is_empty() {
                        println!(
                            "    omitted {} CONFIG_PROTECT file(s) (use --include-config y)",
                            excluded.len()
                        );
                    }
                    successes.push((cpv, size));
                }
                Err(e) => {
                    crate::style::error_line(&format!("Failed to package {cpv}: {e:#}"));
                    failed += 1;
                }
            }
        }
    }

    // Refresh the Packages index so `-k`/`-g` consumers see new containers.
    if !successes.is_empty() {
        let chost = read_make_conf_var(globals, "CHOST")
            .await
            .unwrap_or_default();
        match portage_binpkg::index_pkgdir(&pkgdir, &chost) {
            Ok((n, skipped)) => {
                if skipped > 0 {
                    eprintln!(">>> Packages index: {n} entries ({skipped} skipped)");
                } else {
                    eprintln!(">>> Packages index: {n} entries -> {pkgdir}/Packages");
                }
            }
            Err(e) => crate::style::warn_line(&format!("could not refresh Packages index: {e:#}")),
        }
    }

    println!(
        ">>> quickpkg: {} package(s) built, {} missing, {} failed",
        successes.len(),
        missing.len(),
        failed
    );

    if !missing.is_empty() || failed > 0 {
        bail!(
            "quickpkg incomplete ({} missing, {} failed)",
            missing.len(),
            failed
        );
    }
    Ok(())
}

/// Resolve one CLI arg to installed packages.
///
/// Accepts:
/// - VDB directory path (`/var/db/pkg/cat/pf` or under `--root`)
/// - portage atoms / bare names (via [`find_packages`])
fn resolve_arg(vdb: &Vdb, arg: &str, merge_root: &Utf8Path) -> Vec<InstalledPackage> {
    if let Some(pkg) = try_vdb_path(vdb, arg, merge_root) {
        return vec![pkg];
    }
    find_packages(vdb, arg)
}

fn try_vdb_path(vdb: &Vdb, arg: &str, merge_root: &Utf8Path) -> Option<InstalledPackage> {
    let path = Utf8Path::new(arg);
    // Absolute path that looks like a VDB entry, or relative under merge root.
    let candidates = [
        path.to_path_buf(),
        merge_root.join(arg.trim_start_matches('/')),
    ];
    for cand in candidates {
        if !cand.is_dir() {
            continue;
        }
        // Expect …/var/db/pkg/CAT/PF (SLOT or CONTENTS present).
        if !(cand.join("SLOT").is_file() || cand.join("CONTENTS").is_file()) {
            continue;
        }
        // Canonicalize so path equality against Vdb entries works.
        let cand_canon = std::fs::canonicalize(cand.as_std_path())
            .ok()
            .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
            .unwrap_or_else(|| cand.clone());
        if let Some(p) = vdb.packages().into_iter().find(|p| {
            p.path() == cand.as_path()
                || p.path() == cand_canon.as_path()
                || std::fs::canonicalize(p.path().as_std_path())
                    .ok()
                    .is_some_and(|q| q == cand_canon.as_std_path())
        }) {
            return Some(p);
        }
        // Path is a VDB-shaped dir but not under this Vdb root — still try
        // atom match from CAT/PF directory names.
        let cat = cand.file_name().and_then(|_| cand.parent()?.file_name())?;
        let pf = cand.file_name()?;
        let atom = format!("={cat}/{pf}");
        let matched = find_packages(vdb, &atom);
        if matched.len() == 1 {
            return Some(matched.into_iter().next().unwrap());
        }
    }
    None
}

fn package_one(
    pkg: &InstalledPackage,
    merge_root: &Utf8Path,
    pkgdir: &Utf8Path,
    protect: &ConfigProtectLists,
    include_config: bool,
    include_unmodified_config: bool,
) -> Result<(Utf8PathBuf, u64, Vec<Utf8PathBuf>)> {
    warn_bindist(pkg);

    let contents = pkg
        .contents()
        .with_context(|| format!("reading CONTENTS for {}", pkg.cpv()))?;

    let staging = tempfile::Builder::new()
        .prefix("em-quickpkg-")
        .tempdir()
        .context("creating quickpkg staging dir")?;
    let image_dir = staging.path().join("image");
    std::fs::create_dir_all(&image_dir)?;

    let mut excluded = Vec::new();
    for entry in &contents {
        if should_exclude_config(
            entry,
            protect,
            include_config,
            include_unmodified_config,
            merge_root,
        ) {
            excluded.push(entry.path.clone());
            continue;
        }
        stage_entry(entry, merge_root, &image_dir)?;
    }

    // Place a marker when config was omitted, matching portage's empty
    // protect-file note (informational only; not required by GLEP 78).
    if !excluded.is_empty() {
        let note = image_dir.join(".quickpkg-config-omitted");
        if let Some(parent) = note.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut f = std::fs::File::create(&note)?;
        writeln!(
            f,
            "# empty file because --include-config=n when `em quickpkg` was used"
        )?;
    }

    let cat = pkg.category();
    let pf = pkg.pf().to_string();
    let build_id = crate::binpkg::next_build_id(pkgdir, cat, &pf);
    let out = pkgdir.join(cat).join(format!("{pf}-{build_id}.gpkg.tar"));
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent.as_std_path())?;
    }

    portage_binpkg::write_gpkg(
        &portage_binpkg::GpkgInput {
            image_dir: &image_dir,
            metadata_dir: pkg.path().as_std_path(),
            basename: &pf,
            signing: None,
        },
        out.as_std_path(),
    )
    .with_context(|| format!("writing binary package {out}"))?;

    let size = std::fs::metadata(out.as_std_path())
        .map(|m| m.len())
        .unwrap_or(0);
    Ok((out, size, excluded))
}

fn should_exclude_config(
    entry: &ContentsEntry,
    protect: &ConfigProtectLists,
    include_config: bool,
    include_unmodified_config: bool,
    merge_root: &Utf8Path,
) -> bool {
    if include_config {
        return false;
    }
    // Only regular files (and maybe symlinks) are "config" content worth skipping.
    if !matches!(entry.kind, ContentsKind::Obj) {
        return false;
    }
    if !protect.is_protected(&entry.path) {
        return false;
    }
    if include_unmodified_config && let Some(orig) = entry.md5.as_deref() {
        let live = live_path(merge_root, &entry.path);
        if let Ok(data) = std::fs::read(live.as_std_path()) {
            let cur = format!("{:x}", md5::compute(&data));
            if cur.eq_ignore_ascii_case(orig) {
                return false; // unmodified protected config — include
            }
        }
    }
    true
}

fn stage_entry(
    entry: &ContentsEntry,
    merge_root: &Utf8Path,
    image_dir: &std::path::Path,
) -> Result<()> {
    let rel = strip_root_prefix(&entry.path);
    let dest = image_dir.join(rel.as_str());
    let src = live_path(merge_root, &entry.path);

    match entry.kind {
        ContentsKind::Dir => {
            std::fs::create_dir_all(&dest).with_context(|| format!("mkdir {}", dest.display()))?;
        }
        ContentsKind::Obj => {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if !src.exists() {
                crate::style::warn_line(&format!("missing file (skipped): {src}"));
                return Ok(());
            }
            // Prefer hardlink (cheap, preserves content); fall back to copy.
            if std::fs::hard_link(src.as_std_path(), &dest).is_err() {
                std::fs::copy(src.as_std_path(), &dest)
                    .with_context(|| format!("copy {src} -> {}", dest.display()))?;
                // Preserve mode bits on the copy path only (hardlink shares them).
                if let Ok(meta) = std::fs::metadata(src.as_std_path()) {
                    let _ = std::fs::set_permissions(&dest, meta.permissions());
                }
            }
        }
        ContentsKind::Sym => {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let target_owned: String = entry
                .target
                .as_ref()
                .map(|t| t.to_string())
                .or_else(|| {
                    // Fall back to reading the live link.
                    std::fs::read_link(src.as_std_path())
                        .ok()
                        .and_then(|p| p.into_os_string().into_string().ok())
                })
                .unwrap_or_default();
            if target_owned.is_empty() {
                crate::style::warn_line(&format!("empty symlink target (skipped): {}", entry.path));
                return Ok(());
            }
            // Remove and recreate if a previous hardlink attempt left something.
            let _ = std::fs::remove_file(&dest);
            std::os::unix::fs::symlink(&target_owned, &dest)
                .with_context(|| format!("symlink {} -> {}", dest.display(), target_owned))?;
        }
        ContentsKind::Fifo | ContentsKind::Dev => {
            // Rare in packages; skip rather than require root to recreate nodes.
            crate::style::warn_line(&format!("skipping {:?} entry {}", entry.kind, entry.path));
        }
    }
    Ok(())
}

fn live_path(merge_root: &Utf8Path, contents_path: &Utf8Path) -> Utf8PathBuf {
    let rel = strip_root_prefix(contents_path);
    if merge_root.as_str() == "/" {
        Utf8PathBuf::from("/").join(rel.as_str())
    } else {
        merge_root.join(rel.as_str())
    }
}

fn strip_root_prefix(path: &Utf8Path) -> Utf8PathBuf {
    let s = path.as_str();
    Utf8PathBuf::from(s.trim_start_matches('/'))
}

fn warn_bindist(pkg: &InstalledPackage) {
    let iuse = pkg.iuse().unwrap_or_default();
    let use_flags = pkg.use_flags().unwrap_or_default();
    let iuse_plain: BTreeSet<_> = iuse
        .iter()
        .map(|f| f.trim_start_matches(['+', '-']))
        .collect();
    if iuse_plain.contains("bindist") && !use_flags.iter().any(|f| f == "bindist") {
        eprintln!(" * {}: package was emerged with USE=-bindist!", pkg.cpv());
        eprintln!(
            " * {}: it might not be legal to redistribute this.",
            pkg.cpv()
        );
    }
    if let Ok(Some(restrict)) = pkg.field("RESTRICT")
        && restrict.split_whitespace().any(|t| t == "bindist")
    {
        eprintln!(" * {}: package has RESTRICT=bindist!", pkg.cpv());
        eprintln!(
            " * {}: it might not be legal to redistribute this.",
            pkg.cpv()
        );
    }
}

/// CONFIG_PROTECT / CONFIG_PROTECT_MASK for quickpkg (no ebuild shell needed).
struct ConfigProtectLists {
    protect: Vec<String>,
    mask: Vec<String>,
}

impl ConfigProtectLists {
    async fn load(globals: &Cli) -> Self {
        let mut protect = read_list(globals, "CONFIG_PROTECT").await;
        if !protect.iter().any(|p| p == "/etc") {
            protect.push("/etc".to_string());
        }
        Self {
            protect,
            mask: read_list(globals, "CONFIG_PROTECT_MASK").await,
        }
    }

    fn is_protected(&self, obj: &Utf8Path) -> bool {
        let obj = obj.as_str();
        longest_match(&self.protect, obj) > longest_match(&self.mask, obj)
    }
}

async fn read_list(globals: &Cli, var: &str) -> Vec<String> {
    read_make_conf_var(globals, var)
        .await
        .unwrap_or_default()
        .split_whitespace()
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn longest_match(list: &[String], obj: &str) -> usize {
    list.iter()
        .filter(|p| obj == p.as_str() || obj.starts_with(&format!("{p}/")))
        .map(String::len)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_vdb::ContentsEntry;

    fn write_pkg(vdb_root: &std::path::Path, root: &std::path::Path, cat: &str, pf: &str) {
        let vdir = vdb_root.join(cat).join(pf);
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SLOT"), "0\n").unwrap();
        std::fs::write(vdir.join("EAPI"), "8\n").unwrap();
        std::fs::write(vdir.join("USE"), "\n").unwrap();
        std::fs::write(vdir.join("IUSE"), "\n").unwrap();
        std::fs::write(vdir.join("CATEGORY"), format!("{cat}\n")).unwrap();
        std::fs::write(vdir.join("PF"), format!("{pf}\n")).unwrap();

        let bin = root.join("usr/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let payload = b"hello-quickpkg\n";
        std::fs::write(bin.join("hello"), payload).unwrap();
        let md5 = format!("{:x}", md5::compute(payload));
        let contents = format!("dir /usr\ndir /usr/bin\nobj /usr/bin/hello {md5} 0\n");
        std::fs::write(vdir.join("CONTENTS"), contents).unwrap();
    }

    #[test]
    fn package_one_builds_gpkg_from_vdb_and_root() {
        // Spawns `tar`/`zstd` (via `portage_binpkg::write_gpkg`), which
        // resolve themselves off the process-wide `PATH` — see
        // `crate::test_support::path_lock`'s doc comment for the real,
        // consistently-reproducing CI failure this fixes.
        let _path_lock = crate::test_support::path_lock();

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let vdb_root = root.join("var/db/pkg");
        let pkgdir = tmp.path().join("pkgdir");
        std::fs::create_dir_all(&pkgdir).unwrap();
        write_pkg(&vdb_root, &root, "app-misc", "hello-1.0");

        let vdb = Vdb::open(Utf8PathBuf::from_path_buf(vdb_root.clone()).unwrap()).unwrap();
        let pkg = vdb.packages().into_iter().next().unwrap();
        let merge_root = Utf8PathBuf::from_path_buf(root).unwrap();
        let pkgdir_u = Utf8PathBuf::from_path_buf(pkgdir).unwrap();
        let protect = ConfigProtectLists {
            protect: vec!["/etc".into()],
            mask: vec![],
        };

        let (out, size, excluded) =
            package_one(&pkg, &merge_root, &pkgdir_u, &protect, false, false).unwrap();
        assert!(out.as_str().ends_with(".gpkg.tar"));
        assert!(size > 0);
        assert!(excluded.is_empty());
        assert!(out.is_file());

        // Metadata round-trip: reader sees CATEGORY/PF from the VDB dir.
        let meta = portage_binpkg::read_metadata(out.as_std_path()).unwrap();
        assert_eq!(meta.get("CATEGORY").map(String::as_str), Some("app-misc"));
        assert_eq!(meta.get("PF").map(String::as_str), Some("hello-1.0"));
    }

    #[test]
    fn config_protect_excludes_etc_obj_by_default() {
        let protect = ConfigProtectLists {
            protect: vec!["/etc".into()],
            mask: vec![],
        };
        let entry = ContentsEntry {
            kind: ContentsKind::Obj,
            path: Utf8PathBuf::from("/etc/foo.conf"),
            md5: Some("deadbeef".into()),
            mtime: Some(0),
            target: None,
        };
        let root = Utf8Path::new("/");
        assert!(should_exclude_config(&entry, &protect, false, false, root));
        assert!(!should_exclude_config(&entry, &protect, true, false, root));
    }
}
