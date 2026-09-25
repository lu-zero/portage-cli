//! Build/merge serialization locks
//!
//! Portage's `EbuildBuildDir` / merge lock pair: an exclusive `flock` on the
//! package work directory for the whole phase chain, and one on
//! `<work_base>/.merge.lock` around the merge critical section, so parallel
//! `__worker` processes — and concurrent `em` instances sharing the tree —
//! cannot interleave qmerge.
//!
//! Also holds the `FEATURES`-driven clean-subdir filter, which is the other
//! half of "what may be deleted around the phase loop" and lives here because
//! both are build-tree epilogue policy.

use camino::Utf8Path;

/// When a build-tree clean runs relative to the phase loop
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanWhen {
    /// Before phases (stale-tree scrub)
    Pre,
    /// After a successful merge chain
    Post,
}

/// Apply `FEATURES=keepwork|keeptemp|noclean` to a clean-subdir list
///
/// Portage make.conf(5) / `__dyn_clean` shape:
/// - **keepwork** — skip all cleaning (pre and post)
/// - **keeptemp** — never delete `temp/` (`${T}`)
/// - **noclean** — after merge only: keep source and temporary files
///   (`work/` + `temp/`); still drop `image/` / `homedir`
pub(crate) fn filter_clean_subs(
    subs: &[&'static str],
    features: &std::collections::HashSet<String>,
    when: CleanWhen,
) -> Vec<&'static str> {
    if features.contains("keepwork") {
        return Vec::new();
    }
    let mut out: Vec<&'static str> = subs.to_vec();
    if features.contains("keeptemp") {
        out.retain(|s| *s != "temp");
    }
    if when == CleanWhen::Post && features.contains("noclean") {
        // "Do not delete the source and temporary files after the merge"
        out.retain(|s| *s != "work" && *s != "temp");
    }
    out
}

/// Exclusive flock on `<work_base>/.merge.lock`, held around the merge
/// critical section so parallel `__worker` processes — and concurrent em
/// instances sharing the tree — cannot interleave qmerge.
///
/// `work_dir` is [`package_work_dir`] (`$work_base/<root-key>/<cat>/<pf>`), so
/// the work base is three parents up. Blocking acquire runs off the async
/// executor; released on drop (or by the kernel on process exit).
pub(crate) async fn lock_merge_flock(work_dir: &Utf8Path) -> Option<std::fs::File> {
    // work_base / root_key / category / pf
    let base = work_dir.parent()?.parent()?.parent()?;
    acquire_flock(base.join(".merge.lock").into_std_path_buf(), "merge lock").await
}

/// How long to wait on a contended lock before saying so.
///
/// Contention is normal and brief: under `--jobs N` every worker serialises on
/// `.merge.lock` for its own qmerge. Only a wait this long means something is
/// actually stuck — a suspended `em`, or one wedged mid-merge — which is
/// otherwise indistinguishable from a hang, since the acquire is silent.
const LOCK_NOTICE_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// Blocking exclusive flock on `path`, announcing the wait if it runs long
///
/// Released on drop, or by the kernel if the process dies — note that a
/// *suspended* process keeps it, which is the case the notice exists to name.
pub(crate) async fn acquire_flock(path: std::path::PathBuf, label: &str) -> Option<std::fs::File> {
    let notice_path = path.clone();
    let acquire = tokio::task::spawn_blocking(move || {
        // append: never truncate at open — that would race the holder, since
        // the lock is not ours until `flock` returns.
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)
            .ok()?;
        rustix::fs::flock(&f, rustix::fs::FlockOperation::LockExclusive).ok()?;
        stamp_holder(&path);
        Some(f)
    });
    tokio::pin!(acquire);
    match tokio::time::timeout(LOCK_NOTICE_AFTER, &mut acquire).await {
        Ok(joined) => joined.ok().flatten(),
        Err(_) => {
            match read_holder(&notice_path) {
                Some(who) => crate::style::warn_line!(
                    "waiting for the {label} held by {who} ({})",
                    notice_path.display()
                ),
                None => {
                    crate::style::warn_line!("waiting for the {label} ({})", notice_path.display())
                }
            }
            (&mut acquire).await.ok().flatten()
        }
    }
}

/// Record who holds this lock, for a waiter to report
///
/// Written *after* `flock` returns, so only the real holder ever writes, and a
/// waiter that has already blocked is guaranteed to see a live process rather
/// than a stale pid. Best-effort: a lock whose stamp cannot be written or read
/// just degrades to naming the file.
///
/// This is deliberately not `/proc/locks`. That table reports the superblock's
/// `s_dev`, which is not `stat`'s `st_dev` on a filesystem giving each
/// subvolume its own anonymous device (btrfs), so matching a file to its lock
/// there is fiddly and wrong by default — and it does not exist off Linux,
/// where `em` also runs. The lock file is already open; it can simply say.
fn stamp_holder(path: &std::path::Path) {
    use std::io::{Seek, Write};
    let line = format!("pid {}\n", std::process::id());
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|mut f| {
            f.set_len(0)?;
            f.rewind()?;
            f.write_all(line.as_bytes())
        });
}

/// The holder recorded by [`stamp_holder`], if any
fn read_holder(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Exclusive flock on the package work directory itself (Portage
/// `EbuildBuildDir`), held for the whole phase chain so two concurrent
/// merges never share a WORKDIR even if scheduling fails to serialize them.
pub(crate) async fn lock_builddir(work_dir: &Utf8Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(work_dir.as_std_path()).ok()?;
    acquire_flock(
        work_dir.join(".builddir.lock").into_std_path_buf(),
        "build directory lock",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feats(tokens: &[&str]) -> std::collections::HashSet<String> {
        tokens.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn filter_clean_keepwork_skips_everything() {
        let base = ["work", "image", "temp", "homedir"];
        assert!(filter_clean_subs(&base, &feats(&["keepwork"]), CleanWhen::Pre).is_empty());
        assert!(filter_clean_subs(&base, &feats(&["keepwork"]), CleanWhen::Post).is_empty());
    }

    #[test]
    fn filter_clean_keeptemp_preserves_temp_pre_and_post() {
        let base = ["work", "image", "temp", "homedir"];
        let pre = filter_clean_subs(&base, &feats(&["keeptemp"]), CleanWhen::Pre);
        assert_eq!(pre, vec!["work", "image", "homedir"]);
        let post = filter_clean_subs(&base, &feats(&["keeptemp"]), CleanWhen::Post);
        assert_eq!(post, vec!["work", "image", "homedir"]);
    }

    #[test]
    fn filter_clean_noclean_only_affects_post() {
        let base = ["work", "image", "temp", "homedir"];
        let pre = filter_clean_subs(&base, &feats(&["noclean"]), CleanWhen::Pre);
        assert_eq!(pre, base.to_vec(), "noclean must not disable pre-clean");
        let post = filter_clean_subs(&base, &feats(&["noclean"]), CleanWhen::Post);
        assert_eq!(
            post,
            vec!["image", "homedir"],
            "noclean keeps source (work) and temp"
        );
    }

    // The stamp is what a waiter reports, so pin that it is written only by
    // the real holder and is readable while the lock is held. Portable by
    // construction: no `/proc`, so this also covers the BSD hosts `em` runs on.
    #[test]
    fn a_held_lock_names_its_holder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("held.lock");

        // Nothing recorded before anyone holds it.
        assert_eq!(read_holder(&path), None);

        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        rustix::fs::flock(&f, rustix::fs::FlockOperation::LockExclusive).unwrap();
        stamp_holder(&path);

        assert_eq!(
            read_holder(&path).as_deref(),
            Some(format!("pid {}", std::process::id()).as_str())
        );

        // Re-stamping replaces rather than appends, so a reused lock file
        // never reports a previous run's pid alongside the current one.
        stamp_holder(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
    }
}
