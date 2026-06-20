use portage_atom::interner::Interned;
use portage_atom::{Cpv, Dep, Version};
use portage_atom_pubgrub::{
    PortagePackage, UseConfig, UseFlagState, UseOverride, apply_package_use,
};
use portage_metadata::IUseDefault;

use super::repo::{RepoData, find_cache};

/// A `REQUIRED_USE` constraint left unsatisfied by a planned package's
/// effective USE.
pub(super) struct RequiredUseViolation {
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
pub(super) fn find_violations(
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    use_config: &UseConfig,
    package_use: &[(Dep, Vec<UseOverride>)],
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
        let effective = apply_package_use(use_config, &cpv, pkg.slot(), package_use);

        // Mirror format_flags: a flag's effective state is the configured value
        // if set, otherwise its IUSE default (`+flag`).
        let enabled = |flag: &str| -> bool {
            let interned = Interned::intern(flag);
            match effective.get_opt(interned) {
                Some(UseFlagState::Enabled) => true,
                Some(_) => false,
                None => cache
                    .metadata
                    .iuse
                    .iter()
                    .find(|f| f.name() == flag)
                    .is_some_and(|f| matches!(f.default, Some(IUseDefault::Enabled))),
            }
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
