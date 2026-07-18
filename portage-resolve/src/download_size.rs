use std::collections::HashMap;

use camino::Utf8Path;
use portage_atom::interner::Interned;
use portage_atom::{Cpn, Cpv, Version};
use portage_atom_pubgrub::{CededFlag, PortagePackage, UseFlagState, resolve_effective_use};
use portage_repo::{Manifest, ManifestEntry};

use crate::effective_use::{apply_ceded, apply_force_mask, iuse_defaults, iuse_set};
use crate::repo::{RepoData, ResolvePolicy, find_cache};

/// Per-package download size, in **bytes**, of the distfiles that are not
/// already present in `DISTDIR` — matching what `emerge -pv` totals as
/// "Size of downloads".
///
/// Distfiles are resolved from `SRC_URI` evaluated against each package's
/// effective USE (so USE-conditional sources are only counted when active) and
/// sized from the package's `Manifest`. A file present in `DISTDIR` at its
/// recorded size counts as zero (already fetched).
pub fn compute(
    repo_path: &Utf8Path,
    distdir: &str,
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    policy: &ResolvePolicy,
    ceded: &[CededFlag],
) -> HashMap<Cpv, u64> {
    let distdir = Utf8Path::new(distdir);
    // One Manifest parse per CPN, reused across that package's versions.
    let mut manifests: HashMap<Cpn, HashMap<String, u64>> = HashMap::new();
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
        if cache.metadata.src_uri.is_empty() {
            continue;
        }

        let cpv = Cpv::new(*pkg.cpn(), ver.clone());
        let defaults = iuse_defaults(cache);
        let mut effective = resolve_effective_use(
            &defaults,
            policy.pre_env,
            &cpv,
            pkg.slot(),
            policy.package_use,
            policy.env_use,
        );
        let stable = policy
            .accept_keywords
            .is_stable(&cache.metadata.keywords, &cpv, pkg.slot());
        let iuse = iuse_set(cache);
        apply_force_mask(&mut effective, policy.force_mask, &cpv, stable, &iuse);
        apply_ceded(&mut effective, *pkg.cpn(), ceded);
        let enabled = |flag: &str| -> bool {
            matches!(effective.get(Interned::intern(flag)), UseFlagState::Enabled)
        };

        let mut wanted: Vec<String> = Vec::new();
        for entry in &cache.metadata.src_uri {
            entry.collect_filenames(&enabled, &mut wanted);
        }
        wanted.sort();
        wanted.dedup();

        let manifest = manifests
            .entry(*pkg.cpn())
            .or_insert_with(|| load_manifest_sizes(repo_path, pkg.cpn()));

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

/// Parse `<repo>/<cat>/<pkg>/Manifest` into a `filename -> size` map.
fn load_manifest_sizes(repo_path: &Utf8Path, cpn: &Cpn) -> HashMap<String, u64> {
    let path = repo_path
        .join(cpn.category.as_str())
        .join(cpn.package.as_str())
        .join("Manifest");
    let Ok(content) = std::fs::read_to_string(path.as_std_path()) else {
        return HashMap::new();
    };
    let Ok(manifest) = Manifest::parse(&content) else {
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
