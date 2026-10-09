//! Building a GPKG container from a finished build tree (`em --buildpkg`)
//!
//! Packages the shell's post-`src_install` image (`$ED`) plus a VDB-shaped
//! metadata directory into a `portage-binpkg` container, signing the Manifest
//! when a key is configured. The build-id numbering that decides the output
//! filename lives in [`crate::binpkg::next_build_id`].

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::Ebuild;

use super::{
    ConfigProtect, Walk, WalkResult, capture_environment, merge_spec_from_env, open_or_create_vdb,
    rewrite_d_symlinks, walk_image, write_environment_bz2,
};

/// The image subtree that gets post-processed and merged: the shell's `ED`
/// (`image/${EPREFIX}`, set by `init_build_env`), falling back to
/// `work_root/image` when `ED` is unset or empty. With `EPREFIX=""` this is
/// the plain image dir, so host / `--prefix` builds are unchanged.
pub(crate) fn ed_image_dir(shell: &portage_repo::EbuildShell, work_root: &Utf8Path) -> Utf8PathBuf {
    shell
        .get_var("ED")
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|| work_root.join("image"))
}

/// Package the image as `src_install` left it, without touching the live ROOT/VDB
///
/// Used by `-b` before the merge and by `-B` instead of one. Packing after the
/// merge would capture what `pkg_preinst` did to the image, and a binary
/// install runs `pkg_preinst` again.
///
/// CONTENTS and metadata are computed as a merge would — `walk_image` +
/// `Vdb::register` — against a scratch VDB under `work_root/temp`.
pub(crate) async fn build_binpkg_standalone(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    work_root: &Utf8Path,
    root: &Utf8Path,
    build_env_key: &str,
) -> Result<Utf8PathBuf> {
    shell.apply_iuse_effective();
    let env = shell.collect_env();
    let env_dump = capture_environment(shell, work_root).await;
    let image_dir = ed_image_dir(shell, work_root);
    let cp = ConfigProtect::from_shell(shell);

    // CONTENTS records installed paths (`/usr/bin/foo`) whatever the
    // destination; a path that does not exist keeps config protection out of it.
    let scratch_dest = work_root.join("temp/buildpkgonly-dest");
    let WalkResult { contents, size, .. } = walk_image(
        &image_dir,
        &work_root.join("image"),
        &scratch_dest,
        &cp,
        rewrite_d_symlinks(&env),
        Walk::Scan { digests: true },
    )?;

    let scratch_vdb_root = work_root.join("temp/buildpkgonly-vdb");
    let vdb = open_or_create_vdb(&scratch_vdb_root)?;
    let counter = vdb.next_counter()?;
    let build_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let elf = crate::elfscan::scan_image(&image_dir);
    let spec = merge_spec_from_env(
        env,
        ebuild.cpv().clone(),
        contents,
        elf,
        size,
        build_time,
        counter,
    );
    let installed = vdb.register(&spec)?;

    let pf = format!("{}-{}", ebuild.name(), ebuild.version());
    let ebuild_dest = installed.path().join(format!("{pf}.ebuild"));
    if let Err(e) = std::fs::copy(ebuild.path(), ebuild_dest.as_std_path()) {
        crate::style::warn_line!("could not copy ebuild into package metadata: {e}");
    }
    if let Ok(ref data) = env_dump
        && let Err(e) = write_environment_bz2(&installed, data)
    {
        crate::style::warn_line!("could not write environment.bz2: {e}");
    }

    let key_file = installed.path().join(portage_binpkg::BUILD_ENV_KEY_FIELD);
    let key = portage_binpkg::encode_build_env_key(build_env_key);
    std::fs::write(key_file.as_std_path(), format!("{key}\n"))
        .with_context(|| format!("writing {key_file}"))?;

    write_binpkg(shell, ebuild, work_root, root, installed.path())
}

/// Shared GPKG-writing core: pack `image_dir` (`${D}`) + `metadata_dir` (a
/// VDB-shaped directory -- the real VDB entry for a normal `-b` merge via
/// [`build_binpkg_standalone`]) into a GPKG under `PKGDIR`.
fn write_binpkg(
    shell: &portage_repo::EbuildShell,
    ebuild: &Ebuild,
    work_root: &Utf8Path,
    root: &Utf8Path,
    metadata_dir: &Utf8Path,
) -> Result<Utf8PathBuf> {
    let cat = ebuild.category();
    let pf = format!("{}-{}", ebuild.name(), ebuild.version());
    let image_dir = ed_image_dir(shell, work_root);
    // `ED` is `image/${EPREFIX}` under `--prefix`/`--local`. Packages that
    // install nothing (every `virtual/*`, many `*-toolchain-symlinks`) never
    // create that nested path — `walk_image` treats a missing dir as empty and
    // merges fine, but `tar -C ED` fails with `Cannot open: No such file or
    // directory`. Live 2026-08-07: systemic `--buildpkg` failure under
    // `--prefix --target -b`. Create an empty ED so the GPKG image member is
    // a valid empty tree (same as a no-op install).
    if !image_dir.exists() {
        std::fs::create_dir_all(image_dir.as_std_path())
            .with_context(|| format!("creating empty image dir {image_dir} for --buildpkg"))?;
    }
    // PKGDIR precedence: $PKGDIR env (portage honours it) → the shell's resolved
    // value (make.conf/make.globals) → the default. Must agree with the
    // consumer's `binpkg::resolve_pkgdir` — including its root-awareness:
    // `root.join("var/cache/binpkgs")` needs no separate host-vs-root branch,
    // since it already reduces to the real system's `/var/cache/binpkgs` when
    // `root` is `/`. See `resolve_pkgdir`'s doc comment for why a non-host
    // root must never fall back to that real, root-owned system path.
    let pkgdir = std::env::var("PKGDIR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            shell
                .get_var("PKGDIR")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|| root.join("var/cache/binpkgs"));
    let build_id = crate::binpkg::next_build_id(&pkgdir, cat, &pf);
    let out = pkgdir.join(cat).join(format!("{pf}-{build_id}.gpkg.tar"));

    // FEATURES=binpkg-signing: sign the Manifest (clearsign) + a detached
    // .sig per metadata/image member, matching real portage's own gpkg
    // signing scheme (see `portage_binpkg::gpg`'s module doc for the
    // deliberate secret-key-as-file-path simplification vs real gpg-agent).
    let features: std::collections::HashSet<String> = shell
        .get_var("FEATURES")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let signing_key = if features.contains("binpkg-signing") {
        Some(resolve_binpkg_signing_key(shell, root)?)
    } else {
        None
    };
    portage_binpkg::write_gpkg(
        &portage_binpkg::GpkgInput {
            image_dir: image_dir.as_std_path(),
            metadata_dir: metadata_dir.as_std_path(),
            basename: &pf,
            signing: signing_key.as_ref(),
        },
        out.as_std_path(),
    )
    .with_context(|| format!("writing binary package {out}"))?;
    // Keep `Packages` coherent for `-k`/`-g` without a separate
    // `em maint binhost` (quickpkg already reindexes; normal `-b`/`-B` did not).
    let chost = shell.get_var("CHOST").unwrap_or_default();
    if let Err(e) = portage_binpkg::index_pkgdir(&pkgdir, &chost) {
        crate::style::warn_line!("could not refresh Packages index after {out}: {e:#}");
    }
    Ok(out)
}

/// Resolve `BINPKG_GPG_SIGNING_KEY` (+ `_GPG_HOME`/`_DIGEST`/passphrase
/// vars) into a loaded [`portage_binpkg::gpg::SigningKey`] for
/// `FEATURES=binpkg-signing`. **Redefined** here vs real portage: a path to
/// an armored secret-key file, not a gpg keyring key-ID — this project has
/// no gpg-agent/pinentry to resolve a keyring ID against (see
/// `portage_binpkg::gpg`'s module doc).
///
/// A relative path resolves against `BINPKG_GPG_SIGNING_GPG_HOME`,
/// defaulting to `<root>/etc/portage/gnupg`.
fn resolve_binpkg_signing_key(
    shell: &portage_repo::EbuildShell,
    root: &Utf8Path,
) -> Result<portage_binpkg::gpg::SigningKey> {
    let digest = shell
        .get_var("BINPKG_GPG_SIGNING_DIGEST")
        .and_then(|s| s.parse().ok())
        .unwrap_or(portage_binpkg::gpg::HashAlgorithm::Sha512);
    let key_var = shell
        .get_var("BINPKG_GPG_SIGNING_KEY")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "FEATURES=binpkg-signing requires BINPKG_GPG_SIGNING_KEY (path to an armored OpenPGP secret key)"
            )
        })?;
    let mut key_path = Utf8PathBuf::from(key_var.trim());
    if key_path.is_relative() {
        let home = shell
            .get_var("BINPKG_GPG_SIGNING_GPG_HOME")
            .map(Utf8PathBuf::from)
            .unwrap_or_else(|| root.join("etc/portage/gnupg"));
        key_path = home.join(key_path);
    }
    // No pinentry/gpg-agent here — the passphrase (if any) comes from the
    // environment. `_PASSPHRASE_FILE` (an em-only addition, documented as
    // such) wins over the bare env var: it avoids leaving the passphrase
    // readable via `/proc/<pid>/environ`.
    let passphrase = std::env::var("BINPKG_GPG_SIGNING_KEY_PASSPHRASE_FILE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|f| {
            std::fs::read_to_string(&f)
                .with_context(|| format!("reading {f}"))
                .map(|s| s.trim_end().to_string())
        })
        .transpose()?
        .or_else(|| std::env::var("BINPKG_GPG_SIGNING_KEY_PASSPHRASE").ok())
        .unwrap_or_default();
    portage_binpkg::gpg::SigningKey::load(key_path.as_std_path(), &passphrase, digest)
        .with_context(|| format!("loading GPG signing key {key_path}"))
}
