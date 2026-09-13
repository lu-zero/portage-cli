use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use usage::Cli;

use cargo_ebuild::{cargo as cargomod, ebuild, fetch, vendor};

#[derive(Debug, Cli)]
#[usage(
    bin = "cargo-ebuild",
    version,
    about = "Gentoo ebuild + cargo.eclass vendor tarball from a Cargo package"
)]
struct Args {
    /// Package directory (Cargo.lock or Cargo.toml)
    #[usage(default = ".", value_name = "PATH", value_hint = usage::ValueHint::DirPath)]
    path: Utf8PathBuf,

    /// Rewrite generated bits of an existing ebuild
    #[usage(long, value_name = "FILE", value_hint = usage::ValueHint::FilePath)]
    update: Option<Utf8PathBuf>,

    /// Write a new ebuild here (default {name}-{version}.ebuild)
    #[usage(short, long, value_name = "FILE", value_hint = usage::ValueHint::FilePath)]
    output: Option<Utf8PathBuf>,

    /// Vendor tarball path (default {name}-{version}-crates.tar.xz)
    #[usage(long, value_name = "FILE", value_hint = usage::ValueHint::FilePath)]
    tarball: Option<Utf8PathBuf>,

    /// Emit CRATES=/GIT_CRATES= instead of a vendor tarball
    #[usage(long)]
    no_tarball: bool,

    /// DISTDIR (make.conf, then /var/cache/distfiles)
    #[usage(short = 'd', long, value_name = "DIR", value_hint = usage::ValueHint::DirPath)]
    distdir: Option<Utf8PathBuf>,

    /// SPDX → Gentoo mapping (default: main repo license-mapping.conf)
    #[usage(short = 'l', long, value_name = "FILE", value_hint = usage::ValueHint::FilePath)]
    license_mapping: Option<Utf8PathBuf>,

    /// Overwrite existing ebuild/tarball
    #[usage(short, long)]
    force: bool,
}

fn resolve_distdir(cli_distdir: Option<Utf8PathBuf>) -> PathBuf {
    cli_distdir
        .map(|p| p.into_std_path_buf())
        .or_else(|| std::env::var_os("DISTDIR").map(PathBuf::from))
        .or_else(|| {
            portage_repo::MakeConf::load_default()
                .ok()
                .and_then(|mc| mc.get("DISTDIR").map(PathBuf::from))
        })
        .unwrap_or_else(|| PathBuf::from("/var/cache/distfiles"))
}

fn resolve_license_mapping_path(cli_path: Option<Utf8PathBuf>) -> PathBuf {
    cli_path
        .map(|p| p.into_std_path_buf())
        .or_else(|| {
            portage_repo::ReposConf::load().ok().and_then(|rc| {
                rc.main_repo()
                    .and_then(|r| r.location.as_path())
                    .map(|p| p.join("metadata/license-mapping.conf"))
            })
        })
        .unwrap_or_else(|| PathBuf::from("/var/db/repos/gentoo/metadata/license-mapping.conf"))
}

fn refuse_existing(path: &Path, force: bool) -> Result<()> {
    if !force && path.exists() {
        anyhow::bail!("{} exists already, pass -f to overwrite it", path.display());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Args::parse();
    let dir = cli.path.as_std_path();
    let distdir = resolve_distdir(cli.distdir.clone());
    let mapping_path = resolve_license_mapping_path(cli.license_mapping.clone());

    let lock = cargomod::ensure_lockfile(dir)
        .with_context(|| format!("lockfile for {}", dir.display()))?;
    let manifest = dir.join("Cargo.toml");
    let manifest = if manifest.is_file() {
        manifest
    } else {
        lock.parent().unwrap_or(dir).join("Cargo.toml")
    };
    let pkg = cargomod::package_from_toml(&manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let crates = cargomod::crates_from_lockfile(&lock)
        .with_context(|| format!("parsing {}", lock.display()))?;

    let outfile = if let Some(out) = cli.output {
        out.into_std_path_buf()
    } else if let Some(update) = &cli.update {
        update.clone().into_std_path_buf()
    } else {
        PathBuf::from(format!("{}-{}.ebuild", pkg.name, pkg.version))
    };
    if cli.update.is_none() {
        refuse_existing(&outfile, cli.force)?;
    }

    let want_tarball = !cli.no_tarball;
    let tarball_path = cli
        .tarball
        .map(|p| p.into_std_path_buf())
        .unwrap_or_else(|| PathBuf::from(format!("{}-{}-crates.tar.xz", pkg.name, pkg.version)));
    if want_tarball && cli.update.is_none() {
        refuse_existing(&tarball_path, cli.force)?;
    }

    let (crate_tarball_name, crate_license_spdx) = if want_tarball {
        std::fs::create_dir_all(&distdir)
            .with_context(|| format!("creating DISTDIR {}", distdir.display()))?;
        let vendor_dir = distdir.join("cargo_home").join("gentoo");
        vendor::vendor_to_tarball(&manifest, &[], &vendor_dir, &tarball_path)
            .context("vendoring crates")?;
        (
            Some(
                tarball_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ),
            cargomod::licenses_from_vendor_dir(&vendor_dir),
        )
    } else {
        std::fs::create_dir_all(&distdir)
            .with_context(|| format!("creating DISTDIR {}", distdir.display()))?;
        let distdir_utf8 = camino::Utf8PathBuf::from_path_buf(distdir.clone())
            .map_err(|p| anyhow::anyhow!("DISTDIR is not valid UTF-8: {}", p.display()))?;
        fetch::fetch_crates(&crates, &distdir_utf8)
            .await
            .context("fetching crates")?;
        fetch::verify_crates(&crates, &distdir).context("verifying crates")?;
        (
            None,
            crates
                .iter()
                .filter_map(|c| cargomod::license_from_crate(c, &distdir))
                .collect(),
        )
    };

    let ebuild_str = if let Some(inp) = &cli.update {
        let existing = std::fs::read_to_string(inp).with_context(|| format!("reading {inp}"))?;
        ebuild::update_ebuild(ebuild::UpdateInput {
            existing: &existing,
            pkg: &pkg,
            crates: &crates,
            crate_tarball: crate_tarball_name.as_deref(),
            distdir: &distdir,
            mapping_path: &mapping_path,
            crate_license_spdx: &crate_license_spdx,
        })?
    } else {
        ebuild::render_ebuild(ebuild::RenderInput {
            pkg: &pkg,
            crates: &crates,
            crate_tarball: crate_tarball_name.as_deref(),
            prog_version: env!("CARGO_PKG_VERSION"),
            distdir: &distdir,
            mapping_path: &mapping_path,
            crate_license_spdx: &crate_license_spdx,
        })?
    };

    std::fs::write(&outfile, &ebuild_str)
        .with_context(|| format!("writing {}", outfile.display()))?;
    println!("{}", outfile.display());
    Ok(())
}
