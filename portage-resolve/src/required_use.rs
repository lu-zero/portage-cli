use portage_atom::{Cpv, Version};
use portage_atom_pubgrub::{CededFlag, PortagePackage, resolve_effective_use};

use crate::effective_use::{apply_ceded, apply_force_mask, iuse_defaults, iuse_set};
use crate::repo::{RepoData, ResolvePolicy, find_cache};

/// A `REQUIRED_USE` constraint left unsatisfied by a planned package's
/// effective USE.
pub struct RequiredUseViolation {
    /// The package whose constraint is violated.
    pub cpv: Cpv,
    /// The failing sub-constraints, rendered (e.g. `^^ ( llvm_slot_20 llvm_slot_21 )`).
    pub unsatisfied: Vec<String>,
}

/// Evaluate each planned package's `REQUIRED_USE` against the USE it would be
/// built with.
///
/// This is a post-solve, per-package check (it needs only a package's own
/// effective USE, not the solution graph). Portage hard-errors on an unsatisfied
/// `REQUIRED_USE` and tells the user which flags to change; `em -p` surfaces the
/// same information as an advisory warning.
pub fn find_violations(
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    policy: &ResolvePolicy,
    ceded: &[CededFlag],
) -> Vec<RequiredUseViolation> {
    let mut out = Vec::new();
    for (pkg, ver) in order {
        if pkg.is_virtual() {
            continue;
        }
        let Some(cache) = find_cache(data, pkg, ver) else {
            continue;
        };
        let Some(required_use) = cache.metadata.required_use.as_ref() else {
            continue;
        };

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
        let slot_key = pkg.slot();
        apply_force_mask(
            &mut effective,
            policy.force_mask,
            &cpv,
            slot_key.as_ref().map(|s| s.as_str()),
            stable,
            &iuse,
        );
        apply_ceded(&mut effective, *pkg.cpn(), ceded);

        // `effective` already has this package's IUSE defaults folded in, so
        // an unset flag is simply Disabled — no fallback needed.
        let enabled = |flag: &str| -> bool {
            matches!(
                effective.get(portage_atom::interner::Interned::intern(flag)),
                portage_atom_pubgrub::UseFlagState::Enabled
            )
        };

        let unmet = required_use.unsatisfied(&enabled);
        if !unmet.is_empty() {
            out.push(RequiredUseViolation {
                cpv,
                unsatisfied: unmet.iter().map(|e| e.to_string()).collect(),
            });
        }
    }
    out
}
