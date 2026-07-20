//! `em maint binhost` — regenerate the binpkg `Packages` index.
//!
//! Thin CLI wrapper: resolves `PKGDIR`/`CHOST` and reports the result. The
//! actual scan/checksum/write logic lives in `portage_binpkg::regen`.

use anyhow::{Result, bail};

use crate::binpkg::{read_make_conf_var, resolve_pkgdir};
use crate::cli::Cli;

/// Dispatch `em maint binhost`.
pub async fn run(globals: &Cli) -> Result<()> {
    let pkgdir = resolve_pkgdir(globals).await;
    if !pkgdir.exists() {
        bail!("PKGDIR does not exist: {}", pkgdir);
    }
    let chost = read_make_conf_var(globals, "CHOST")
        .await
        .unwrap_or_default();
    let (count, skipped) = portage_binpkg::index_pkgdir(&pkgdir, &chost)?;
    println!(
        "emaint binhost: indexed {} package(s){} -> {}/Packages",
        count,
        if skipped > 0 {
            format!(", skipped {skipped}")
        } else {
            String::new()
        },
        pkgdir
    );
    Ok(())
}
