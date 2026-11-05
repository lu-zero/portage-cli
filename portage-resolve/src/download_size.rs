use std::collections::HashMap;

use camino::Utf8Path;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Version};
use portage_atom_pubgrub::{CededFlag, PortagePackage, UseFlagState};
use portage_repo::{ManifestEntry, RepoSet, Repository};

use crate::effective_use::effective_use;
use crate::repo::{RepoData, ResolvePolicy, find_cache, repo_name_of};

/// Per-package download size, in **bytes**, of the distfiles that are not
/// already present in `DISTDIR` — matching what `emerge -pv` totals as
/// "Size of downloads"
///
/// Distfiles are resolved from `SRC_URI` evaluated against each package's
/// effective USE (so USE-conditional sources are only counted when active) and
/// sized from the `Manifest` of the repository the version comes from (for an alias package,
/// the real package's). A file present in `DISTDIR` at its recorded size counts as zero
/// (already fetched).
pub fn compute(
    set: &RepoSet,
    distdir: &str,
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    policy: &ResolvePolicy,
    ceded: &[CededFlag],
) -> HashMap<Cpv, u64> {
    let distdir = Utf8Path::new(distdir);
    // One Manifest parse per package and repo, reused across that package's versions.
    let mut manifests: HashMap<(Interned<DefaultInterner>, Cpn), HashMap<String, u64>> =
        HashMap::new();
    // A distfile shared by several packages is fetched once: emerge counts it
    // against the first package in the list that needs it, and zero thereafter.
    let mut counted: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut sizes = HashMap::new();

    for (pkg, ver) in order {
        if pkg.is_virtual() {
            continue;
        }
        let Some(cache) = find_cache(data, pkg, ver) else {
            continue;
        };
        if cache.metadata.src_uri.is_empty_raw() {
            continue;
        }

        let cpv = Cpv::new(*pkg.cpn(), ver.clone());
        let stable = policy
            .accept_keywords
            .is_stable(&cache.metadata.keywords, &cpv, pkg.slot());
        let effective = effective_use(policy, pkg, ver, cache, stable, ceded);
        let enabled = |flag: &str| -> bool {
            matches!(effective.get(Interned::intern(flag)), UseFlagState::Enabled)
        };

        let mut wanted: Vec<String> = Vec::new();
        for entry in cache.metadata.src_uri.list() {
            entry.collect_filenames(&enabled, &mut wanted);
        }
        wanted.sort();
        wanted.dedup();

        let real_cpn = data
            .real_cpn_of
            .get(pkg.cpn())
            .copied()
            .unwrap_or(*pkg.cpn());
        let repo_name = repo_name_of(data, &Cpv::new(real_cpn, ver.clone()));
        let manifest = manifests.entry((repo_name, real_cpn)).or_insert_with(|| {
            let repo = set.by_name(repo_name.as_str()).unwrap_or(set.main());
            load_manifest_sizes(repo, &real_cpn)
        });

        let mut total = 0u64;
        for filename in &wanted {
            let Some(&size) = manifest.get(filename) else {
                continue;
            };
            let cached = distdir.join(filename);
            let present = std::fs::metadata(cached.as_std_path())
                .map(|m| m.len() == size)
                .unwrap_or(false);
            // Count a missing distfile once, against the first package needing it.
            if !present && counted.insert(filename.clone()) {
                total += size;
            }
        }
        sizes.insert(cpv, total);
    }

    sizes
}

/// The package's `Manifest` as a `filename -> size` map
fn load_manifest_sizes(repo: &Repository, cpn: &Cpn) -> HashMap<String, u64> {
    let Some(Ok(Some(manifest))) = repo.package(cpn).map(|pkg| pkg.manifest()) else {
        return HashMap::new();
    };
    manifest
        .dist_entries()
        .filter_map(|e| match e {
            ManifestEntry::Dist { filename, size, .. } => Some((filename.clone(), *size)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, OnceLock};

    use camino::Utf8PathBuf;
    use portage_atom_pubgrub::UseLayer;
    use portage_metadata::CacheEntry;
    use portage_repo::{AcceptSet, LicenseGroupRegistry};

    use super::*;
    use crate::force_mask::ForceMask;
    use crate::repo::{AcceptKeywords, AcceptOverlay, PolicyMaskList};

    fn empty_layer() -> &'static UseLayer {
        static E: OnceLock<UseLayer> = OnceLock::new();
        E.get_or_init(UseLayer::default)
    }

    // A repo named `name` with one Manifest per `(cpn, distfile, size)`
    fn make_repo(dir: &Utf8PathBuf, name: &str, files: &[(&str, &str, u64)]) -> Repository {
        std::fs::create_dir_all(dir.join("metadata")).unwrap();
        std::fs::write(dir.join("metadata/layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.join("profiles")).unwrap();
        std::fs::write(dir.join("profiles/repo_name"), format!("{name}\n")).unwrap();
        for (cpn, file, size) in files {
            std::fs::create_dir_all(dir.join(cpn)).unwrap();
            std::fs::write(
                dir.join(cpn).join("Manifest"),
                format!("DIST {file} {size} SHA256 aa\n"),
            )
            .unwrap();
        }
        Repository::builder().in_memory_cache().open(dir).unwrap()
    }

    #[test]
    fn sizes_come_from_the_versions_own_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let main = make_repo(
            &root.join("gentoo"),
            "gentoo",
            &[("app-misc/foo", "foo-1.tar.gz", 100)],
        );
        let overlay = make_repo(
            &root.join("ov"),
            "ov",
            &[("app-misc/bar", "bar-1.tar.gz", 200)],
        );
        let set = RepoSet::from_ordered(vec![Arc::new(overlay), Arc::new(main)], 1, Vec::new());
        let distdir = root.join("distfiles");
        std::fs::create_dir_all(&distdir).unwrap();

        let foo = Cpv::parse("app-misc/foo-1").unwrap();
        let bar = Cpv::parse("app-misc/bar-1").unwrap();
        // An alias package, sized from the overlay package it was cloned from.
        let cross_bar = Cpv::parse("cross-x/bar-1").unwrap();
        let entry = |file: &str| {
            CacheEntry::parse(&format!(
                "DESCRIPTION=d\nEAPI=8\nSLOT=0\nSRC_URI=https://e/{file}\n"
            ))
            .unwrap()
        };
        let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
        versions.insert(foo.cpn, vec![(foo.clone(), entry("foo-1.tar.gz"))]);
        versions.insert(bar.cpn, vec![(bar.clone(), entry("bar-1.tar.gz"))]);
        versions.insert(
            cross_bar.cpn,
            vec![(cross_bar.clone(), entry("bar-1.tar.gz"))],
        );
        let data = RepoData::from_parts(
            vec![foo.cpn, bar.cpn, cross_bar.cpn],
            versions,
            "gentoo".into(),
            HashMap::from([(bar.clone(), "ov".into())]),
            HashMap::from([(cross_bar.cpn, bar.cpn)]),
        );

        let accept_keywords =
            AcceptKeywords::from_global(&gentoo_core::Arch::intern("amd64"), &["amd64"]);
        let accept_all = AcceptOverlay::new(
            AcceptSet::from_tokens(&["*".into()], &LicenseGroupRegistry::default()),
            Vec::new(),
        );
        let force_mask = ForceMask::default();
        let policy = ResolvePolicy {
            accept_keywords: &accept_keywords,
            package_mask: &PolicyMaskList::EMPTY,
            package_unmask: &PolicyMaskList::EMPTY,
            accept_licenses: &accept_all,
            accept_properties: &accept_all,
            accept_restrict: &accept_all,
            defaults: empty_layer(),
            conf: empty_layer(),
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &force_mask,
            facts: None,
        };
        let order: Vec<(PortagePackage, Version)> = [&foo, &bar, &cross_bar]
            .iter()
            .map(|cpv| (PortagePackage::unslotted(cpv.cpn), cpv.version.clone()))
            .collect();

        let sizes = compute(&set, distdir.as_str(), &data, &order, &policy, &[]);
        assert_eq!(sizes[&foo], 100);
        assert_eq!(sizes[&bar], 200);
        // Same distfile as `bar`: fetched once, counted against the first package needing it.
        assert_eq!(sizes[&cross_bar], 0);
    }
}
