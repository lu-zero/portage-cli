//! `em --local setup`'s profile resolution — step 3 of the config-root
//! ladder (`todo/local-bootstrap-provided.md`). Mirrors the host's own
//! `make.profile` when it resolves under the just-synced repo (a real
//! Gentoo host), else falls back to a per-ARCH default prefix profile (any
//! other host, e.g. Debian, has no Gentoo profile to mirror at all).

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use gentoo_core::Arch;
use portage_repo::{ProfileDesc, Repository};

use crate::config_plan::{self, ConfigEntry};

/// The profile path (relative to `profiles/`) `host_root`'s own
/// `make.profile` resolves to, if that link exists and lands inside
/// `repo`'s own tree — the same canonicalize-and-strip-prefix check
/// `select::profile::current_profile` uses, against the host link instead
/// of the prefix's. `host_root` is `/` in production; tests pass a fixture
/// dir so both branches (mirror vs. ARCH-default) are exercised.
fn host_profile_under_repo(repo: &Repository, host_root: &Utf8Path) -> Option<String> {
    let target = std::fs::canonicalize(host_root.join("etc/portage/make.profile")).ok()?;
    let target = Utf8PathBuf::from_path_buf(target).ok()?;
    let profiles = repo.path().join("profiles").canonicalize_utf8().ok()?;
    target.strip_prefix(&profiles).ok().map(Utf8Path::to_string)
}

/// Best-effort release-version key for sorting candidate profiles newest
/// first: the first path component that parses as a PMS version (e.g. `23.0`
/// in `default/linux/amd64/23.0/no-multilib/prefix`). Unparseable paths sort
/// lowest, which only matters if the tree ever offers more than one
/// candidate release — today's `::gentoo` has exactly one amd64 match.
fn release_key(path: &str) -> Option<portage_atom::Version> {
    path.split('/')
        .find_map(|seg| portage_atom::Version::parse(seg).ok())
}

/// This host's ARCH's default standalone-prefix profile: the newest
/// `.../prefix` leaf for `Arch::current()`, excluding `split-usr` (merged-usr
/// is the modern default) and kernel-version-pinned sub-leaves (`.../prefix/
/// kernel-3.2+`, which don't end in plain `/prefix`). Status is deliberately
/// not filtered to `stable` — every amd64 prefix profile in `::gentoo` today
/// is `exp`, which is simply what a Prefix profile is, not a quality signal.
fn default_profile_for_arch(repo: &Repository) -> Result<String> {
    let arch = Arch::current();
    let mut candidates: Vec<ProfileDesc> = repo
        .profiles_desc()
        .context("reading profiles.desc")?
        .into_iter()
        .filter(|d| {
            d.arch().as_str() == arch.as_str()
                && d.path().ends_with("/prefix")
                && !d.path().contains("split-usr")
        })
        .collect();
    candidates.sort_by_key(|d| release_key(d.path()));
    candidates
        .pop()
        .map(|d| d.path().to_owned())
        .with_context(|| format!("no {arch} prefix profile found in ::gentoo — pass --profile"))
}

/// Symlink `<eroot>/etc/portage/make.profile` to the resolved profile
/// Idempotent: a profile already linked is left untouched — checked by
/// link presence (`symlink_metadata`), not `exists()`, which follows the
/// link and would treat a dangling symlink (e.g. its host profile target
/// since removed) as absent and silently relink instead of leaving it for
/// the user to fix.
pub(super) fn ensure_profile(eroot: &Utf8Path, repo: &Repository) -> Result<()> {
    ensure_profile_from(eroot, repo, Utf8Path::new("/"))
}

/// [`ensure_profile`], but with the "host" root pulled out as a parameter —
/// production always passes `/`; tests pass a fixture dir standing in for it.
fn ensure_profile_from(eroot: &Utf8Path, repo: &Repository, host_root: &Utf8Path) -> Result<()> {
    let link = eroot.join("etc/portage/make.profile");
    if std::fs::symlink_metadata(&link).is_ok() {
        return Ok(());
    }
    let chosen = match host_profile_under_repo(repo, host_root) {
        Some(p) => p,
        None => default_profile_for_arch(repo)?,
    };
    let target = repo.path().join("profiles").join(&chosen);
    tracing::info!(profile = %chosen, "make.profile resolved");
    config_plan::apply_now(&[ConfigEntry::Symlink { link, target }])
}

#[cfg(test)]
mod tests {
    use portage_repo::Repository;

    use super::*;

    #[test]
    fn release_key_finds_the_version_component() {
        assert_eq!(
            release_key("default/linux/amd64/23.0/no-multilib/prefix"),
            Some(portage_atom::Version::parse("23.0").unwrap())
        );
        assert_eq!(release_key("no/version/here"), None);
    }

    fn utf8_tempdir() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap().to_path_buf();
        (dir, path)
    }

    // A minimal openable repo (same skeleton `portage_repo`'s own
    // `make_test_repo` test helper uses) with `profiles.desc` set to
    // `desc_body`.
    fn fixture_repo(desc_body: &str) -> (tempfile::TempDir, Repository) {
        let (dir, path) = utf8_tempdir();
        std::fs::create_dir_all(path.join("metadata")).unwrap();
        std::fs::write(path.join("metadata/layout.conf"), "").unwrap();
        std::fs::create_dir_all(path.join("profiles")).unwrap();
        std::fs::write(path.join("profiles/profiles.desc"), desc_body).unwrap();
        let repo = Repository::builder()
            .in_memory_cache()
            .open(path.as_std_path())
            .unwrap();
        (dir, repo)
    }

    #[test]
    fn mirrors_the_host_profile_when_it_resolves_under_the_repo() {
        let (_repo_dir, repo) = fixture_repo("");
        std::fs::create_dir_all(repo.path().join("profiles/default/linux/amd64/23.0")).unwrap();
        let (_host_dir, host_root) = utf8_tempdir();
        std::fs::create_dir_all(host_root.join("etc/portage")).unwrap();
        std::os::unix::fs::symlink(
            repo.path().join("profiles/default/linux/amd64/23.0"),
            host_root.join("etc/portage/make.profile"),
        )
        .unwrap();
        let (_eroot_dir, eroot) = utf8_tempdir();

        ensure_profile_from(&eroot, &repo, &host_root).unwrap();

        let link_target = std::fs::read_link(eroot.join("etc/portage/make.profile")).unwrap();
        assert_eq!(
            link_target,
            repo.path()
                .join("profiles/default/linux/amd64/23.0")
                .as_std_path()
        );
    }

    #[test]
    fn falls_back_to_the_arch_default_prefix_profile_when_no_host_profile() {
        let arch = Arch::current();
        let desc = format!(
            "{arch} default/linux/{arch}/23.0/no-multilib/prefix exp\n\
             {arch} default/linux/{arch}/23.0/split-usr/no-multilib/prefix exp\n\
             {arch} default/linux/{arch}/23.0/no-multilib/prefix/kernel-3.2+ exp\n"
        );
        let (_repo_dir, repo) = fixture_repo(&desc);
        let (_host_dir, host_root) = utf8_tempdir(); // no make.profile at all
        let (_eroot_dir, eroot) = utf8_tempdir();

        ensure_profile_from(&eroot, &repo, &host_root).unwrap();

        let link_target = std::fs::read_link(eroot.join("etc/portage/make.profile")).unwrap();
        assert_eq!(
            link_target,
            repo.path()
                .join(format!(
                    "profiles/default/linux/{arch}/23.0/no-multilib/prefix"
                ))
                .as_std_path()
        );
    }

    #[test]
    fn does_not_clobber_an_existing_make_profile() {
        let (_repo_dir, repo) = fixture_repo("");
        let (_host_dir, host_root) = utf8_tempdir();
        let (_eroot_dir, eroot) = utf8_tempdir();
        std::fs::create_dir_all(eroot.join("etc/portage")).unwrap();
        std::os::unix::fs::symlink("/somewhere/else", eroot.join("etc/portage/make.profile"))
            .unwrap();

        ensure_profile_from(&eroot, &repo, &host_root).unwrap();

        let link_target = std::fs::read_link(eroot.join("etc/portage/make.profile")).unwrap();
        assert_eq!(link_target, std::path::Path::new("/somewhere/else"));
    }
}
