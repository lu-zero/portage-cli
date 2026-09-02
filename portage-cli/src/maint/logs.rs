//! `em maint logs` — prune the `build.log` files a finished merge leaves behind
//!
//! Deliberately not portage's target. Real `emaint logs` cleans
//! `PORTAGE_LOGDIR`, which for `em` holds only elog output (`em read` owns
//! that, including its own `--delete`). `em` writes each package's build log to
//! `<work_base>/<root-key>/<category>/<PF>/build.log` and keeps it after a
//! successful merge on purpose — a merge that went fine still gets its log
//! read afterwards — so that is what accumulates and what this reclaims.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

/// One retained build log
struct Log {
    path: Utf8PathBuf,
    /// `<root-key>/<category>/<PF>`, the part worth showing
    label: String,
    bytes: u64,
    age: std::time::Duration,
}

/// `remove_hint` names the flag *this caller* removes with — `em maint logs`
/// has `--fix`, while `em clean all` removes by default and reports under
/// `-p`, so a shared wording would tell one of them to pass a flag it has no
/// idea about.
pub fn run(
    work_base: &Utf8Path,
    older_than: Option<&str>,
    fix: bool,
    remove_hint: &str,
) -> Result<()> {
    let cutoff = older_than
        .map(|spec| {
            humantime::parse_duration(spec)
                .with_context(|| format!("--older-than {spec}: not a duration"))
        })
        .transpose()?;

    if !work_base.is_dir() {
        println!("No build directory at {work_base}.");
        return Ok(());
    }

    let mut logs = collect(work_base);
    if let Some(cutoff) = cutoff {
        logs.retain(|l| l.age >= cutoff);
    }
    logs.sort_by_key(|l| std::cmp::Reverse(l.bytes));

    if logs.is_empty() {
        println!("No build logs to clean under {work_base}.");
        return Ok(());
    }

    let total: u64 = logs.iter().map(|l| l.bytes).sum();
    for l in &logs {
        println!("    {} ({})", l.label, crate::clean::human_bytes(l.bytes));
    }
    if !fix {
        println!(
            ">>> {} build log(s), {}. {remove_hint} to remove them.",
            logs.len(),
            crate::clean::human_bytes(total)
        );
        return Ok(());
    }

    let mut removed = 0usize;
    let mut freed = 0u64;
    for l in &logs {
        match std::fs::remove_file(l.path.as_std_path()) {
            Ok(()) => {
                removed += 1;
                freed += l.bytes;
            }
            Err(e) => crate::style::warn_line!("cannot remove {}: {e}", l.path),
        }
    }
    println!(
        ">>> Removed {removed} build log(s), freed {}.",
        crate::clean::human_bytes(freed)
    );
    Ok(())
}

/// `<work_base>/<root-key>/<category>/<PF>/build.log`
///
/// Two things a plain depth-3 walk gets wrong, both of which delete a log
/// that is still being written:
///
/// * **A live `em` build** appends to exactly this path while it runs, so a
///   `<PF>` directory whose `.builddir.lock` is currently flock-held is
///   skipped. That lock is held for the whole phase chain (`ebuild.rs`'s
///   `lock_builddir`), which is precisely the window to stay out of.
/// * **Real portage builds in the same directory.** `default_work_base`
///   resolves to `/var/tmp/portage` whenever it is writable — portage's own
///   `PORTAGE_TMPDIR` — and portage's layout is
///   `<cat>/<pf>/temp/build.log`, which puts its live `${T}/build.log` at the
///   same depth as em's. A `<PF>` component named `temp` is therefore treated
///   as portage's, not ours, and left alone.
fn collect(work_base: &Utf8Path) -> Vec<Log> {
    let now = std::time::SystemTime::now();
    let mut out = Vec::new();
    for root_key in read_dirs(work_base) {
        for category in read_dirs(&root_key.1) {
            for pf in read_dirs(&category.1) {
                // `<cat>/<pf>/temp/build.log` is portage's shape, not ours.
                if pf.0 == "temp" {
                    continue;
                }
                if build_in_progress(&pf.1) {
                    continue;
                }
                let log = pf.1.join("build.log");
                let Ok(meta) = std::fs::metadata(log.as_std_path()) else {
                    continue;
                };
                if !meta.is_file() {
                    continue;
                }
                let age = meta
                    .modified()
                    .ok()
                    .and_then(|m| now.duration_since(m).ok())
                    .unwrap_or_default();
                out.push(Log {
                    path: log,
                    label: format!("{}/{}/{}", root_key.0, category.0, pf.0),
                    bytes: meta.len(),
                    age,
                });
            }
        }
    }
    out
}

/// Whether a package build currently holds this work directory
///
/// Probed by trying the same `.builddir.lock` non-blockingly: acquiring it
/// means nobody is building here, and the lock is dropped again immediately.
/// A failure to even open the file is treated as "not building" — the log is
/// then almost certainly from a finished run.
fn build_in_progress(work_dir: &Utf8Path) -> bool {
    let path = work_dir.join(".builddir.lock");
    if !path.exists() {
        return false;
    }
    let Ok(f) = std::fs::OpenOptions::new()
        .append(true)
        .open(path.as_std_path())
    else {
        return false;
    };
    match rustix::fs::flock(&f, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        // We got it, so no build holds it; release straight away.
        Ok(()) => {
            let _ = rustix::fs::flock(&f, rustix::fs::FlockOperation::Unlock);
            false
        }
        Err(_) => true,
    }
}

/// `(name, path)` for each subdirectory, or empty when unreadable
fn read_dirs(dir: &Utf8Path) -> Vec<(String, Utf8PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir.as_std_path()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            Utf8PathBuf::from_path_buf(e.path()).ok().map(|p| (name, p))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Utf8Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap().as_std_path()).unwrap();
        std::fs::write(path.as_std_path(), vec![b'x'; bytes]).unwrap();
    }

    // The layout is exactly three levels deep. A live build tree sits *below*
    // that with a `${WORKDIR}` of its own, and descending into it would both
    // cost more than the walk is worth and report a log that is not a
    // package's own.
    #[test]
    fn collect_stops_at_the_layout_depth_and_ignores_work_trees() {
        let dir = tempfile::tempdir().unwrap();
        let base = camino::Utf8Path::from_path(dir.path()).unwrap();

        write(&base.join("host/sys-libs/zlib-1.3.1/build.log"), 5000);
        write(&base.join("host/app-misc/foo-1.0/build.log"), 3000);
        write(&base.join("var-tmp-board/sys-apps/bar-2.0/build.log"), 1000);
        // Inside an unpacked ${WORKDIR}: deeper than the layout, must be missed.
        write(
            &base.join("host/sys-libs/zlib-1.3.1/work/zlib-1.3.1/deep/build.log"),
            100,
        );

        let mut found: Vec<String> = collect(base).into_iter().map(|l| l.label).collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                "host/app-misc/foo-1.0".to_string(),
                "host/sys-libs/zlib-1.3.1".to_string(),
                "var-tmp-board/sys-apps/bar-2.0".to_string(),
            ]
        );
        assert_eq!(collect(base).iter().map(|l| l.bytes).sum::<u64>(), 9000);
    }

    // Real portage builds in the same /var/tmp/portage that
    // `default_work_base` picks, and its `<cat>/<pf>/temp/build.log` lands at
    // em's depth. Deleting that takes out a live portage build's log.
    #[test]
    fn collect_leaves_portages_own_temp_build_log_alone() {
        let dir = tempfile::tempdir().unwrap();
        let base = camino::Utf8Path::from_path(dir.path()).unwrap();
        write(&base.join("sys-devel/gcc-14.2.1/temp/build.log"), 23);
        write(&base.join("host/sys-libs/zlib-1.3.1/build.log"), 18);

        let found: Vec<String> = collect(base).into_iter().map(|l| l.label).collect();
        assert_eq!(found, vec!["host/sys-libs/zlib-1.3.1".to_string()]);
    }

    // A build still running holds `.builddir.lock` for its whole phase chain,
    // and is still appending to the log this would delete.
    #[test]
    fn collect_skips_a_work_dir_whose_builddir_lock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let base = camino::Utf8Path::from_path(dir.path()).unwrap();
        let live = base.join("host/sys-libs/zlib-1.3.1");
        write(&live.join("build.log"), 10);
        write(&base.join("host/app-misc/done-1.0/build.log"), 10);

        let lock = live.join(".builddir.lock");
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(lock.as_std_path())
            .unwrap();
        rustix::fs::flock(&f, rustix::fs::FlockOperation::LockExclusive).unwrap();

        let found: Vec<String> = collect(base).into_iter().map(|l| l.label).collect();
        assert_eq!(
            found,
            vec!["host/app-misc/done-1.0".to_string()],
            "a locked work dir is a live build"
        );

        // Once the build is done the log becomes collectable.
        drop(f);
        assert_eq!(collect(base).len(), 2);
    }

    #[test]
    fn a_missing_build_base_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = camino::Utf8Path::from_path(dir.path())
            .unwrap()
            .join("nope");
        assert!(run(&missing, None, false, "Run with --fix").is_ok());
    }
}
