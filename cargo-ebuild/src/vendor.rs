//! Vendor via `cargo vendor`, then pack the result into the `cargo_home/gentoo`
//! tarball `-c/--crate-tarball` writes.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// Run `cargo vendor --offline --versioned-dirs <vendor_dir>` for
/// `manifest_path`, additionally syncing every `extra_manifests` entry via
/// repeated `-s/--sync` (cargo's own multi-workspace vendoring, used for
/// `-c` with multiple project directories). Errors clearly if `cargo vendor`
/// isn't available — no fallback to hand-roll.
pub fn vendor_to_tarball(
    manifest_path: &Path,
    extra_manifests: &[PathBuf],
    vendor_dir: &Path,
    tarball_path: &Path,
) -> Result<()> {
    // Ensure vendor_dir parent exists; `cargo vendor` will create it
    if let Some(parent) = vendor_dir.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let mut base_args = vec![
        "vendor".to_string(),
        "--versioned-dirs".to_string(),
        "--manifest-path".to_string(),
        manifest_path.display().to_string(),
    ];
    for extra in extra_manifests {
        base_args.push("--sync".to_string());
        base_args.push(extra.display().to_string());
    }
    base_args.push(vendor_dir.display().to_string());

    // Try `cargo vendor` with versioned-dirs; respect CARGO env. Try --offline first, then online.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut offline_args = base_args.clone();
    offline_args.insert(1, "--offline".to_string());
    let attempts = [offline_args, base_args];

    let mut last_err = None;
    for args in attempts {
        let output = Command::new(&cargo).args(&args).output();
        match output {
            Ok(out) if out.status.success() => {
                let cargo_home = vendor_dir.parent().unwrap_or(vendor_dir).to_path_buf();
                create_vendor_tarball(&cargo_home, tarball_path)?;
                return Ok(());
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                // If offline failed due to missing cache, try next attempt (online)
                if stderr.contains("--offline was specified")
                    && args.contains(&"--offline".to_string())
                {
                    last_err = Some(stderr);
                    continue;
                }
                anyhow::bail!(
                    "cargo vendor failed (status {:?}): {}",
                    out.status.code(),
                    stderr
                )
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                anyhow::bail!("cargo not found on PATH: {e}")
            }
            Err(e) => {
                last_err = Some(e.to_string());
                continue;
            }
        }
    }
    Err(anyhow::anyhow!(
        "cargo vendor failed (offline and online): {:?}",
        last_err
    ))
    .context("spawning cargo vendor")
}

fn create_vendor_tarball(cargo_home: &Path, out: &Path) -> Result<()> {
    let out_file =
        std::fs::File::create(out).with_context(|| format!("create {}", out.display()))?;
    let enc = xz2::write::XzEncoder::new(out_file, 9);
    let mut tar = tar::Builder::new(enc);
    tar.mode(tar::HeaderMode::Deterministic);

    // `vendor_to_tarball` always calls us with `<distdir>/<crate_tarball_prefix>`
    // (i.e. `.../cargo_home/gentoo`)'s parent, so this is always `cargo_home` —
    // tar its contents (not the directory itself) under that literal prefix,
    // matching `ECARGO_VENDOR=${WORKDIR}/cargo_home/gentoo` (cargo.eclass:342).
    let display_prefix = PathBuf::from("cargo_home");

    for entry in
        std::fs::read_dir(cargo_home).with_context(|| format!("read {}", cargo_home.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy();
        let dest = display_prefix.join(&*name);
        if path.is_dir() {
            tar.append_dir_all(&dest, &path)
                .with_context(|| format!("append {}", path.display()))?;
        } else {
            tar.append_path_with_name(&path, &dest)
                .with_context(|| format!("append {}", path.display()))?;
        }
    }

    // Set mtime=0 for determinism if any headers missed
    let enc = tar.into_inner()?;
    enc.finish().context("xz finish")?;
    Ok(())
}

/// Whether `cargo vendor` is available — used for a clear early error
/// instead of a `cargo vendor` subprocess failure buried under other output.
pub fn has_cargo_vendor() -> bool {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    Command::new(&cargo)
        .args(["vendor", "--help"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
