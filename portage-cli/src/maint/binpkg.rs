//! `em maint binpkg` — local `PKGDIR` maintenance on top of the `Packages`
//! index/reader substrate.
//!
//! No real-portage `emaint` module covers this ground (its own `emaint
//! binhost` only regenerates the index) — this is an em-only extension. All
//! the actual scan/checksum/report logic lives in `portage_binpkg::maint`;
//! this module resolves `PKGDIR`/`CHOST` from `&Cli` and formats the
//! structured reports for the terminal.

use anyhow::{Result, bail};
use humansize::{BINARY, format_size};
use portage_binpkg::maint::{PruneReport, VerifyReport};

use crate::binpkg::{read_make_conf_var, resolve_pkgdir};
use crate::cli::{BinpkgAction, Cli};

/// Dispatch `em maint binpkg <action>`.
pub async fn run(action: &BinpkgAction, globals: &Cli) -> Result<()> {
    let pkgdir = resolve_pkgdir(globals).await;
    match action {
        BinpkgAction::Verify { fix } => {
            let chost = read_make_conf_var(globals, "CHOST")
                .await
                .unwrap_or_default();
            verify(&pkgdir, &chost, *fix)
        }
        BinpkgAction::List => list(&pkgdir),
        BinpkgAction::Prune { dry_run } => {
            let chost = read_make_conf_var(globals, "CHOST")
                .await
                .unwrap_or_default();
            prune(&pkgdir, &chost, *dry_run)
        }
    }
}

fn verify(pkgdir: &camino::Utf8Path, chost: &str, fix: bool) -> Result<()> {
    let report: VerifyReport = portage_binpkg::maint::verify(pkgdir, chost, fix)?;

    for problem in &report.problems {
        if problem.missing {
            println!("!!! missing: {} ({})", problem.cpv, problem.path);
            continue;
        }
        println!("!!! digest mismatch: {} ({})", problem.cpv, problem.path);
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
    }

    println!(
        "emaint binpkg verify: {} ok, {} corrupt, {} missing (of {})",
        report.ok,
        report.corrupt_count(),
        report.missing_count(),
        report.total
    );
    if let Some(count) = report.reindexed {
        println!("emaint binpkg verify: reindexed -> {count} package(s)");
    }

    if !fix && !report.is_clean() {
        bail!(
            "{} corrupt, {} missing binpkg(s) found (rerun with --fix)",
            report.corrupt_count(),
            report.missing_count()
        );
    }
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
