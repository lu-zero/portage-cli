//! `em maint binpkg` — local `PKGDIR` maintenance on top of the `Packages`
//! index/reader substrate.
//!
//! No real-portage `emaint` module covers this ground (its own `emaint
//! binhost` only regenerates the index) — this is an em-only extension. All
//! the actual scan/checksum/report logic lives in `portage_binpkg::maint`;
//! this module resolves `PKGDIR`/`CHOST` from `&Cli` and formats the
//! structured reports for the terminal.

use anyhow::{Context, Result, bail};
use humansize::{BINARY, format_size};
use portage_binpkg::gpg;
use portage_binpkg::maint::{PruneReport, SignatureStatus, VerifyReport};

use crate::binpkg::{
    DesiredBuildEnv, read_make_conf_var, resolve_gpg_verify_home_for_roots, resolve_pkgdir,
};
use crate::cli::{BinpkgAction, Cli};
use crate::style::C_ERROR;

/// Dispatch `em maint binpkg <action>`.
pub async fn run(action: &BinpkgAction, globals: &Cli) -> Result<()> {
    let pkgdir = resolve_pkgdir(globals).await;
    match action {
        BinpkgAction::Verify {
            fix,
            require_signature,
        } => {
            let chost = read_make_conf_var(globals, "CHOST")
                .await
                .unwrap_or_default();
            verify(globals, &pkgdir, &chost, *fix, *require_signature).await
        }
        BinpkgAction::List => list(&pkgdir),
        BinpkgAction::Prune { dry_run } => {
            let chost = read_make_conf_var(globals, "CHOST")
                .await
                .unwrap_or_default();
            prune(&pkgdir, &chost, *dry_run)
        }
        BinpkgAction::Fingerprint { full, host } => fingerprint(globals, *full, *host).await,
        BinpkgAction::GpgImport { keyfile } => gpg_import(globals, keyfile).await,
    }
}

async fn verify(
    globals: &Cli,
    pkgdir: &camino::Utf8Path,
    chost: &str,
    fix: bool,
    require_signature: bool,
) -> Result<()> {
    let verify_home = resolve_gpg_verify_home_for_roots(&globals.roots()).await;
    let keyring = gpg::load_keyring_dir(verify_home.as_std_path())
        .with_context(|| format!("loading GPG verify keyring at {verify_home}"))?;
    if require_signature && keyring.is_none() {
        bail!(
            "--require-signature needs a GPG verify keyring at {verify_home} — run `em maint binpkg gpg-import <keyfile>` first"
        );
    }

    let report: VerifyReport =
        portage_binpkg::maint::verify(pkgdir, chost, fix, require_signature, keyring.as_ref())?;

    use std::io::Write;
    let mut out = anstream::stdout();
    for problem in &report.problems {
        if problem.missing {
            let _ = writeln!(
                out,
                "{C_ERROR}!!!{C_ERROR:#} missing: {} ({})",
                problem.cpv, problem.path
            );
            continue;
        }
        if problem.size_mismatch.is_some()
            || problem.md5_mismatch.is_some()
            || problem.sha1_mismatch.is_some()
        {
            let _ = writeln!(
                out,
                "{C_ERROR}!!!{C_ERROR:#} digest mismatch: {} ({})",
                problem.cpv, problem.path
            );
        }
        if let Some((got, expected)) = problem.size_mismatch {
            println!("    size: got {got}, expected {expected}");
        }
        if let Some((got, expected)) = &problem.md5_mismatch {
            println!("    MD5: got {got}, expected {expected}");
        }
        if let Some((got, expected)) = &problem.sha1_mismatch {
            println!("    SHA1: got {got}, expected {expected}");
        }
        if let Some(q) = &problem.quarantined_to {
            println!("    quarantined to {q}");
        }
        match problem.signature {
            Some(SignatureStatus::Unsigned) => {
                println!("    signature: unsigned (required)");
            }
            Some(SignatureStatus::Invalid) => {
                println!("    signature: INVALID (does not verify against the keyring)");
            }
            _ => {}
        }
    }
    let _ = out.flush();

    println!(
        "emaint binpkg verify: {} ok, {} corrupt, {} missing (of {})",
        report.ok,
        report.corrupt_count(),
        report.missing_count(),
        report.total
    );
    if keyring.is_some() || require_signature {
        println!(
            "emaint binpkg verify: {} signature problem(s)",
            report.signature_problems()
        );
    }
    if let Some(count) = report.reindexed {
        println!("emaint binpkg verify: reindexed -> {count} package(s)");
    }

    if !fix && !report.is_clean() {
        bail!(
            "{} corrupt, {} missing, {} signature problem(s) found (rerun with --fix for the corrupt/missing ones)",
            report.corrupt_count(),
            report.missing_count(),
            report.signature_problems()
        );
    }
    Ok(())
}

/// `em maint binpkg gpg-import <keyfile>` — import an armored public key
/// into the GPG verify keyring directory (`BINPKG_GPG_VERIFY_GPG_HOME`,
/// a flat directory of `*.asc` files, not a real gpg keybox — see
/// `portage_binpkg::gpg`'s module doc).
async fn gpg_import(globals: &Cli, keyfile: &camino::Utf8Path) -> Result<()> {
    let dir = resolve_gpg_verify_home_for_roots(&globals.roots()).await;
    std::fs::create_dir_all(dir.as_std_path()).with_context(|| format!("creating {dir}"))?;
    let bytes = std::fs::read(keyfile).with_context(|| format!("reading {keyfile}"))?;
    let (key, info) = gpg::parse_public_key(&bytes)
        .with_context(|| format!("parsing OpenPGP public key from {keyfile}"))?;
    let armored = gpg::export_public_key(&key).context("re-armoring imported key")?;
    let dest = dir.join(format!("{}.asc", info.fingerprint));
    std::fs::write(dest.as_std_path(), armored).with_context(|| format!("writing {dest}"))?;

    println!("imported OpenPGP key into {dir}");
    println!("  fingerprint: {}", info.fingerprint);
    if let Some(uid) = &info.primary_uid {
        println!("  user ID:     {uid}");
    }
    println!("  subkeys:     {}", info.subkeys);
    Ok(())
}

/// Truncate `s` to at most `max` chars, marking the cut with a trailing `…`
/// (counted within `max`, so the result is never longer than `max` chars).
fn truncate(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if s.chars().count() <= max {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    std::borrow::Cow::Owned(out)
}

fn list(pkgdir: &camino::Utf8Path) -> Result<()> {
    let rows = portage_binpkg::maint::list_index(pkgdir)?;
    for row in &rows {
        let size = row
            .size
            .map(|s| format_size(s, BINARY))
            .unwrap_or_else(|| "?".to_string());
        let build_id = row.build_id.map(|b| b.to_string()).unwrap_or_default();
        let chost = if row.chost.is_empty() {
            "-"
        } else {
            &row.chost
        };
        let key = portage_binpkg::index::short_build_env_key(&row.build_env_key);
        let cflags = truncate(&row.cflags, 32);
        println!(
            "{:<45} {build_id:>4}  {size:>10}  {chost:<26}  {key:<12}  {cflags:<32}  {}",
            row.cpv, row.path
        );
    }
    println!("{} package(s) in {pkgdir}", rows.len());
    Ok(())
}

/// `em maint binpkg fingerprint [--full] [--host]` — print the build-env key
/// for the current roots' make.conf flags. Default output is exactly one
/// line, the short slug, so it's directly usable in a PKGDIR path:
/// `PKGDIR=/var/cache/em-binpkgs/${CHOST}/$(em maint binpkg fingerprint)`.
async fn fingerprint(globals: &Cli, full: bool, host: bool) -> Result<()> {
    let roots = if host {
        globals.host_roots()
    } else {
        globals.roots()
    };
    let env = DesiredBuildEnv::for_roots(&roots).await;
    let key = env.key();

    if full {
        println!("CHOST:      {}", env.chost);
        println!("CFLAGS:     {}", env.cflags);
        println!("CXXFLAGS:   {}", env.cxxflags);
        println!("LDFLAGS:    {}", env.ldflags);
        println!("RUSTFLAGS:  {}", env.rustflags);
        println!(
            "key:        {}",
            if key.is_empty() { "(none)" } else { &key }
        );
        println!(
            "short key:  {}",
            portage_binpkg::index::short_build_env_key(&key)
        );
    } else {
        println!("{}", portage_binpkg::index::short_build_env_key(&key));
    }
    Ok(())
}

fn prune(pkgdir: &camino::Utf8Path, chost: &str, dry_run: bool) -> Result<()> {
    let report: PruneReport = portage_binpkg::maint::prune(pkgdir, chost, dry_run)?;

    for kept in &report.kept {
        println!(
            "{}: keeping build {} ({})",
            kept.cpv, kept.build_id, kept.rel
        );
    }
    for removed in &report.removed {
        if dry_run {
            println!(
                "{}: would remove build {} ({})",
                removed.cpv, removed.build_id, removed.rel
            );
        } else {
            println!(
                "{}: removed build {} ({})",
                removed.cpv, removed.build_id, removed.rel
            );
        }
    }

    if report.removed.is_empty() {
        println!("emaint binpkg prune: nothing to prune");
        return Ok(());
    }
    if dry_run {
        println!(
            "emaint binpkg prune: {} old build(s) would be removed (dry run, index untouched)",
            report.removed.len()
        );
        return Ok(());
    }
    if let Some(count) = report.reindexed {
        println!(
            "emaint binpkg prune: removed {} old build(s), reindexed -> {count} package(s)",
            report.removed.len()
        );
    }
    Ok(())
}
