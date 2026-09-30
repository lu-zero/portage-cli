use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use usage::Cli;

use cargo_eb::ebuild::{self, Deps, Input};
use cargo_eb::{archive, cargo as cargomod, fetch, vendor};

#[derive(Debug, Cli)]
#[usage(
    bin = "cargo-eb",
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

    /// Write a new ebuild here (default ${P}.ebuild)
    #[usage(short, long, value_name = "FILE", value_hint = usage::ValueHint::FilePath)]
    output: Option<Utf8PathBuf>,

    /// Vendor tarball path (default DISTDIR/${P}-crates.tar.xz)
    #[usage(long, value_name = "FILE", value_hint = usage::ValueHint::FilePath)]
    tarball: Option<Utf8PathBuf>,

    /// Emit CRATES=/GIT_CRATES= instead of a vendor tarball
    #[usage(long, conflicts("tarball"))]
    no_tarball: bool,

    /// DISTDIR (make.conf, then /var/cache/distfiles)
    #[usage(short = 'd', long, value_name = "DIR", value_hint = usage::ValueHint::DirPath)]
    distdir: Option<Utf8PathBuf>,

    /// SPDX → Gentoo mapping (default: main repo license-mapping.conf)
    #[usage(short = 'l', long, value_name = "FILE", value_hint = usage::ValueHint::FilePath)]
    license_mapping: Option<Utf8PathBuf>,

    /// Overwrite existing ebuild/tarballs
    #[usage(short, long)]
    force: bool,

    /// Pack a source snapshot DISTDIR/${P}.tar.xz (default when the crate is not a release)
    #[usage(long)]
    snapshot: bool,

    /// Do not pack a source snapshot, even for unpublished/pre-release crates
    #[usage(long, conflicts("snapshot"))]
    no_snapshot: bool,
}

fn resolve_distdir(cli_distdir: Option<Utf8PathBuf>) -> PathBuf {
    cli_distdir
        .map(Utf8PathBuf::into_std_path_buf)
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
        .map(Utf8PathBuf::into_std_path_buf)
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

fn file_name(path: &Path) -> Result<&str> {
    path.file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("{} has no UTF-8 file name", path.display()))
}

/// Keep `(crate, license)` pairs with an SPDX license, warning about the rest.
fn spdx_licenses(licenses: Vec<(String, Option<String>)>) -> Vec<(String, String)> {
    licenses
        .into_iter()
        .filter_map(|(krate, lic)| {
            if lic.is_none() {
                tracing::warn!(
                    "{krate} only has license-file; review it and add it to LICENSE by hand"
                );
            }
            Some((krate, lic?))
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .without_time()
        .with_target(false)
        .init();
    let cli = Args::parse();
    let dir = cli.path.as_std_path();
    let prepared =
        cargomod::prepare_package(dir).with_context(|| format!("preparing {}", dir.display()))?;
    let pkg = &prepared.package;
    let p = pkg.gentoo_p()?;
    let distdir = resolve_distdir(cli.distdir);
    let mapping_path = resolve_license_mapping_path(cli.license_mapping);

    let fresh = cli.update.is_none();
    let outfile = match (cli.output, &cli.update) {
        (Some(out), _) => {
            refuse_existing(out.as_std_path(), cli.force)?;
            out.into_std_path_buf()
        }
        (None, Some(update)) => update.clone().into_std_path_buf(),
        (None, None) => {
            let out = PathBuf::from(format!("{p}.ebuild"));
            refuse_existing(&out, cli.force)?;
            out
        }
    };

    let want_snapshot = cli.snapshot || (!cli.no_snapshot && !pkg.is_release());
    let snapshot_path = want_snapshot.then(|| distdir.join(format!("{p}.tar.xz")));
    let tarball_path = (!cli.no_tarball).then(|| {
        cli.tarball
            .map(Utf8PathBuf::into_std_path_buf)
            .unwrap_or_else(|| distdir.join(format!("{p}-crates.tar.xz")))
    });
    if fresh {
        for path in [&snapshot_path, &tarball_path].into_iter().flatten() {
            refuse_existing(path, cli.force)?;
        }
    }

    let crates;
    let (deps, crate_licenses) = match &tarball_path {
        Some(path) => {
            let vendored =
                vendor::vendor_to_tarball(&prepared, path).context("vendoring crates")?;
            let deps = Deps::Tarball {
                name: file_name(path)?,
                extra_sources: vendored.extra_sources,
            };
            (deps, spdx_licenses(vendored.licenses))
        }
        None => {
            std::fs::create_dir_all(&distdir)
                .with_context(|| format!("creating DISTDIR {}", distdir.display()))?;
            let distdir_utf8 = camino::Utf8Path::from_path(&distdir)
                .with_context(|| format!("DISTDIR {} is not valid UTF-8", distdir.display()))?;
            crates = cargomod::crates_from_lockfile(&prepared.lock)
                .with_context(|| format!("parsing {}", prepared.lock.display()))?;
            fetch::fetch_crates(&crates, distdir_utf8)
                .await
                .context("fetching crates")?;
            fetch::verify_crates(&crates, &distdir).context("verifying crates")?;
            let licenses = crates
                .iter()
                .map(|c| {
                    let name = format!("{}-{}", c.name(), c.version());
                    (name, cargomod::license_from_crate(c, &distdir))
                })
                .collect();
            let deps = Deps::Crates {
                crates: &crates,
                distdir: &distdir,
            };
            (deps, spdx_licenses(licenses))
        }
    };

    if let Some(path) = &snapshot_path {
        archive::pack_source_tarball(&prepared.source_root, &p, path)
            .context("packing source snapshot")?;
    }
    let member_dir = match prepared.member_dir.as_deref().filter(|_| want_snapshot) {
        Some(d) => Some(
            d.to_str()
                .with_context(|| format!("{} is not valid UTF-8", d.display()))?,
        ),
        None => None,
    };

    let input = Input {
        pkg,
        deps,
        source_tarball: snapshot_path.as_deref().map(file_name).transpose()?,
        member_dir,
        mapping_path: &mapping_path,
        crate_licenses: &crate_licenses,
    };
    let ebuild_str = match &cli.update {
        Some(existing) => {
            let text =
                std::fs::read_to_string(existing).with_context(|| format!("reading {existing}"))?;
            ebuild::update_ebuild(&text, &input)?
        }
        None => ebuild::render_ebuild(&input)?,
    };

    std::fs::write(&outfile, &ebuild_str)
        .with_context(|| format!("writing {}", outfile.display()))?;
    println!("{}", outfile.display());
    Ok(())
}
