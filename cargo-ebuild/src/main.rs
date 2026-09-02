use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use clap::Parser;

use cargo_ebuild::{cargo as cargomod, ebuild, fetch, vendor};

#[derive(Parser)]
#[command(
    name = "pycargoebuild-rs",
    version,
    about = "Gentoo ebuild + crate tarball from Cargo.lock (pycargoebuild replacement, separate from em)"
)]
struct Cli {
    /// Cargo project dirs (contains Cargo.toml/Cargo.lock) — pycargoebuild supports multiple for combined LICENSE
    #[arg(default_value = ".")]
    directories: Vec<PathBuf>,

    /// DISTDIR to fetch crates into (default: from make.conf, then /var/cache/distfiles)
    #[arg(short = 'd', long)]
    distdir: Option<PathBuf>,

    /// Write ebuild to this file (default {name}-{version}.ebuild)
    #[arg(short = 'o', long)]
    output: Option<String>,

    /// Update CRATES/GIT_CRATES in existing ebuild
    #[arg(short = 'i', long)]
    input: Option<PathBuf>,

    /// Pack crates into tarball instead of CRATES= ( --crate-tarball )
    #[arg(short = 'c', long)]
    crate_tarball: bool,

    /// Tarball path (default {distdir}/{name}-{version}-crates.tar.xz)
    #[arg(long)]
    crate_tarball_path: Option<String>,

    #[arg(long, default_value = "cargo_home/gentoo")]
    crate_tarball_prefix: String,

    #[arg(long)]
    no_write_crate_tarball: bool,

    /// Do not include crate LICENSE
    #[arg(short = 'L', long)]
    no_license: bool,

    /// File with SPDX->Gentoo mapping (default: main repo's metadata/license-mapping.conf)
    #[arg(short = 'l', long)]
    license_mapping: Option<PathBuf>,

    /// Add USE flags for Cargo features (like pycargoebuild -e)
    #[arg(short = 'e', long)]
    features: bool,

    /// Force overwrite output
    #[arg(short = 'f', long)]
    force: bool,

    /// Do not run pkgdev manifest
    #[arg(short = 'M', long)]
    no_manifest: bool,
}

/// `DISTDIR`: `-d` flag, then `DISTDIR` env, then `make.conf`, then the
/// portage default.
fn resolve_distdir(cli_distdir: Option<PathBuf>) -> PathBuf {
    cli_distdir
        .or_else(|| std::env::var_os("DISTDIR").map(PathBuf::from))
        .or_else(|| {
            portage_repo::MakeConf::load_default()
                .ok()
                .and_then(|mc| mc.get("DISTDIR").map(PathBuf::from))
        })
        .unwrap_or_else(|| PathBuf::from("/var/cache/distfiles"))
}

/// License-mapping path: `-l` flag, then the main repo's
/// `metadata/license-mapping.conf` (via `repos.conf`), then the portage
/// default location.
fn resolve_license_mapping_path(cli_path: Option<PathBuf>) -> PathBuf {
    cli_path
        .or_else(|| {
            portage_repo::ReposConf::load().ok().and_then(|rc| {
                rc.main_repo()
                    .and_then(|r| r.location.as_path())
                    .map(|p| p.join("metadata/license-mapping.conf"))
            })
        })
        .unwrap_or_else(|| PathBuf::from("/var/db/repos/gentoo/metadata/license-mapping.conf"))
}

fn crate_tarball_path(template: &str, distdir: &Path, name: &str, version: &str) -> PathBuf {
    PathBuf::from(
        template
            .replace("{distdir}", &distdir.display().to_string())
            .replace("{name}", name)
            .replace("{version}", version),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let distdir = resolve_distdir(cli.distdir.clone());
    let mapping_path = resolve_license_mapping_path(cli.license_mapping.clone());

    // 1. Gather crates + pkg metas for each directory (pycargoebuild supports multiple for combined LICENSE)
    let mut pkg_metas = Vec::new();
    let mut first_lock: Option<PathBuf> = None;
    let mut lock_paths = Vec::new();
    for dir in &cli.directories {
        let lock = cargomod::find_lock(dir)
            .with_context(|| format!("Cargo.lock not found under {}", dir.display()))?;
        if first_lock.is_none() {
            first_lock = Some(lock.clone());
        }
        lock_paths.push(lock.clone());
        let pkg = cargomod::package_from_toml(&dir.join("Cargo.toml"))
            .or_else(|_| cargomod::package_from_toml(&lock.parent().unwrap().join("Cargo.toml")))
            .with_context(|| format!("reading Cargo.toml under {}", dir.display()))?;
        pkg_metas.push(pkg);
    }
    let mut crates: Vec<cargo_ebuild::Crate> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for lock in &lock_paths {
        let cs = cargomod::crates_from_lockfile(lock)
            .with_context(|| format!("parsing Cargo.lock {}", lock.display()))?;
        for c in cs {
            let key = (c.name().to_string(), c.version().to_string(), c.filename());
            if seen.insert(key) {
                crates.push(c);
            }
        }
    }
    let pkg = pkg_metas.first().cloned().context("no pkg")?;
    let first_lock = first_lock.unwrap();
    // Handle multiple dirs: combined LICENSE = AND of each pkg.license like pycargoebuild __main__.py:289-295
    let pkg = if pkg_metas.len() > 1 && !cli.no_license {
        let combined = pkg_metas
            .iter()
            .filter_map(|p| p.license.as_deref())
            .map(|s| format!("( {s} )"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut p = pkg.clone();
        p.license = if combined.is_empty() {
            None
        } else {
            Some(combined)
        };
        p
    } else if cli.no_license {
        let mut p = pkg.clone();
        p.license = None;
        p
    } else {
        pkg
    };

    // 2. Where the ebuild (and, if requested, the crate tarball) will land —
    // checked *before* any fetching/vendoring work, matching pycargoebuild's
    // own order (`__main__.py:297-339`): don't do minutes of network/vendor
    // work only to then refuse to overwrite an existing file.
    let outfile = if let Some(inp) = &cli.input {
        cli.output
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| inp.clone())
    } else {
        if pkg_metas.len() > 1 {
            eprintln!(
                "warning: multiple directories passed, all metadata except LICENSE will be taken from first package {}",
                pkg.name
            );
        }
        let template = cli
            .output
            .clone()
            .unwrap_or_else(|| "{name}-{version}.ebuild".to_string());
        PathBuf::from(
            template
                .replace("{name}", &pkg.name)
                .replace("{version}", &pkg.version),
        )
    };
    if !cli.force && outfile.exists() {
        anyhow::bail!(
            "{} exists already, pass -f to overwrite it",
            outfile.display()
        );
    }

    let tarball_path_template = cli
        .crate_tarball_path
        .clone()
        .unwrap_or_else(|| "{distdir}/{name}-{version}-crates.tar.xz".to_string());
    let tarball_path =
        crate_tarball_path(&tarball_path_template, &distdir, &pkg.name, &pkg.version);
    if cli.crate_tarball && !cli.no_write_crate_tarball && !cli.force && tarball_path.exists() {
        anyhow::bail!(
            "{} exists already, pass -f to overwrite it",
            tarball_path.display()
        );
    }

    // 3. Fetch + verify every crate archive — unconditional, same as
    // pycargoebuild (`__main__.py:315-323`): the tarball path needs the
    // archives on disk to resolve GIT_CRATES subdirs even when CRATES= isn't
    // used, and the non-tarball path needs them there for SRC_URI itself.
    std::fs::create_dir_all(&distdir)
        .with_context(|| format!("creating DISTDIR {}", distdir.display()))?;
    let distdir_utf8 = Utf8PathBuf::from_path_buf(distdir.clone())
        .map_err(|p| anyhow::anyhow!("DISTDIR is not valid UTF-8: {}", p.display()))?;
    fetch::fetch_crates(&crates, &distdir_utf8)
        .await
        .context("fetching crates")?;
    fetch::verify_crates(&crates, &distdir).context("verifying crates")?;

    // 4. Optionally pack crates into a tarball via `cargo vendor` (existence
    // already checked above).
    let crate_tarball_name = if cli.crate_tarball {
        if !cli.no_write_crate_tarball {
            if let Some(parent) = tarball_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let manifest = cli.directories[0].join("Cargo.toml");
            let manifest = if manifest.is_file() {
                manifest
            } else {
                first_lock.parent().unwrap().join("Cargo.toml")
            };
            let extra_manifests: Vec<PathBuf> = cli.directories[1..]
                .iter()
                .map(|d| d.join("Cargo.toml"))
                .collect();
            let vendor_dir = distdir.join(&cli.crate_tarball_prefix);
            vendor::vendor_to_tarball(&manifest, &extra_manifests, &vendor_dir, &tarball_path)
                .context("vendoring crates")?;
            eprintln!("crate tarball written to {}", tarball_path.display());
        }
        Some(
            tarball_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        )
    } else {
        None
    };

    // License mapping handling
    let license_overrides: Option<std::collections::HashMap<String, String>> = None; // TODO: picks up pycargoebuild.toml [license-overrides]
    // 5. Render ebuild via minijinja (keeps EBUILD_TEMPLATE verbatim)
    let ebuild_str = if let Some(inp) = &cli.input {
        let existing =
            std::fs::read_to_string(inp).with_context(|| format!("reading {}", inp.display()))?;
        ebuild::update_ebuild_with_distdir(
            &existing,
            &pkg,
            &crates,
            &distdir,
            &mapping_path,
            license_overrides.as_ref(),
            !cli.no_license,
            crate_tarball_name.as_deref(),
        )?
    } else {
        ebuild::render_ebuild_with_distdir(
            &pkg,
            &crates,
            crate_tarball_name.as_deref(),
            env!("CARGO_PKG_VERSION"),
            &distdir,
            &mapping_path,
            license_overrides.as_ref(),
            !cli.no_license,
            cli.features,
        )?
    };

    std::fs::write(&outfile, &ebuild_str)
        .with_context(|| format!("writing {}", outfile.display()))?;
    // pkgdev manifest — only if Manifest exists and not --no-manifest
    if !cli.no_manifest
        && outfile
            .parent()
            .map(|p| p.join("Manifest").exists())
            .unwrap_or(false)
    {
        match std::process::Command::new("pkgdev")
            .arg("manifest")
            .current_dir(outfile.parent().unwrap())
            .status()
        {
            Ok(s) if !s.success() => eprintln!("warning: pkgdev manifest failed"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("warning: pkgdev not found, Manifest will not be updated")
            }
            _ => {}
        }
    }
    println!("{}", outfile.display());
    Ok(())
}
