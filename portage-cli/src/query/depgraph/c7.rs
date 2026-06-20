//! Corner-case spec for **C7 — cross-package `[flag]` USE-dep co-solve**.
//!
//! These are minimal package sets that exercise the scenarios C7 must handle.
//! Today a cross-package `[flag]` dep is **Tier 2**: the solver *detects* the
//! requirement (`PortageDependencyProvider::use_flag_requirements`) and the cli
//! turns it into an autounmask `package.use` suggestion, but the flag is **not**
//! applied — the plan is emitted with the flag unchanged.
//!
//! The goal of C7 (under `--autosolve-use`, mirroring Level-C `REQUIRED_USE`) is
//! to **cede** the target flag and re-solve to a fixpoint, so the requirement is
//! satisfied in the plan rather than only suggested. Each test below asserts the
//! *current* behaviour and documents the C7 target in a `C7:` comment, so the
//! switch to co-solve is a localized assertion change plus the implementation.
//!
//! The default (no `--autosolve-use`) must keep matching `emerge -p`, which also
//! only advises USE changes — so these cases stay Tier 2 unless autosolve is on.

#![cfg(test)]

use gentoo_core::Arch;
use portage_atom::{Cpv, Dep};
use portage_atom_pubgrub::{
    PortageDependencyProvider, PortageVersionSet, UseConfig, UseFlagRequirement, UseOverride,
};
use portage_metadata::CacheEntry;
use std::collections::HashMap;

use portage_repo::{AcceptLicense, LicenseGroupRegistry};

use super::force_mask::ForceMask;
use super::repo::{AcceptKeywords, AcceptLicenses, Adapter, RepoData, target_package};

/// Build a `RepoData` from `(cpv, md5-cache-text)` pairs.
fn repo_from(entries: &[(&str, &str)]) -> RepoData {
    let mut versions: HashMap<portage_atom::Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
    let mut cpns = Vec::new();
    for (cpv_str, text) in entries {
        let cpv = Cpv::parse(cpv_str).unwrap();
        let entry = CacheEntry::parse(text).unwrap();
        if !cpns.contains(&cpv.cpn) {
            cpns.push(cpv.cpn);
        }
        versions.entry(cpv.cpn).or_default().push((cpv, entry));
    }
    RepoData {
        repo_of: Default::default(),
        cpns,
        versions,
        repo_name: "test".into(),
    }
}

/// The outcome of a solve: the cross-package USE-flag requirements the solver
/// detected, and the real (non-virtual) packages in the plan as `cat/pkg-ver`.
struct Outcome {
    reqs: Vec<UseFlagRequirement>,
    plan: Vec<String>,
}

impl Outcome {
    fn req_for(&self, cpn: &str) -> Option<&UseFlagRequirement> {
        self.reqs
            .iter()
            .find(|r| r.package.cpn().to_string() == cpn)
    }
    fn needs_enabled(&self, cpn: &str, flag: &str) -> bool {
        self.req_for(cpn)
            .is_some_and(|r| r.required_enabled.iter().any(|f| f.as_str() == flag))
    }
    fn needs_disabled(&self, cpn: &str, flag: &str) -> bool {
        self.req_for(cpn)
            .is_some_and(|r| r.required_disabled.iter().any(|f| f.as_str() == flag))
    }
    fn has(&self, cpv_prefix: &str) -> bool {
        self.plan.iter().any(|p| p.starts_with(cpv_prefix))
    }
}

/// Solve `targets` against `data` with the given `package_use`. Returns `None`
/// for an unsatisfiable problem.
fn solve_with(
    data: &RepoData,
    targets: &[&str],
    pu: &[(Dep, Vec<UseOverride>)],
) -> Option<Outcome> {
    let arch = Arch::intern("amd64");
    let accept = AcceptKeywords::from_global(&arch, &["amd64"]);
    let lic = AcceptLicenses::new(
        AcceptLicense::from_tokens(&["*".into()], &LicenseGroupRegistry::default()),
        Vec::new(),
    );
    let fm = ForceMask::default();
    let use_config = UseConfig::new();
    let adapter = Adapter {
        data,
        accept_keywords: &accept,
        package_mask: &[],
        package_unmask: &[],
        installed_cpvs: &std::collections::HashSet::new(),
        accept_licenses: &lic,
        use_config: &use_config,
        package_use: pu,
        force_mask: &fm,
        autosolve_use: true,
    };
    let mut provider = PortageDependencyProvider::new(adapter);
    let roots: Vec<_> = targets
        .iter()
        .map(|t| {
            let dep = Dep::parse(t).unwrap();
            let pkg = target_package(data, &dep, &accept, &[], &[], &lic, &use_config, pu, &fm);
            (pkg, PortageVersionSet::any())
        })
        .collect();
    let sol = provider.resolve_targets(roots).ok()?;
    let plan = sol
        .iter()
        .filter(|(p, _)| !p.is_virtual())
        .map(|(p, v)| format!("{}-{}", p.cpn(), v))
        .collect();
    Some(Outcome {
        reqs: provider.use_flag_requirements().to_vec(),
        plan,
    })
}

/// A single (default, no-autosolve) solve — shows the Tier-2 behaviour.
fn solve(data: &RepoData, targets: &[&str]) -> Outcome {
    solve_with(data, targets, &[]).expect("solve")
}

/// Run the C7 co-solve fixpoint (as `depgraph` does under `--autosolve-use`):
/// returns the augmented `package_use` and the final outcome.
fn cosolve(data: &RepoData, targets: &[&str]) -> (Vec<(Dep, Vec<UseOverride>)>, Outcome) {
    let (pu, _applied, solved) = super::package_use::cosolve_use_deps(
        Vec::new(),
        data,
        |pu| solve_with(data, targets, pu),
        |o: &Outcome| o.reqs.clone(),
    );
    // Reuse the converged solve when the fixpoint returned one; otherwise solve.
    let out = solved.unwrap_or_else(|| solve_with(data, targets, &pu).expect("final solve"));
    (pu, out)
}

/// Whether `pu` forces `token` (e.g. `bar` / `-bar`) on `cpn`.
fn pu_forces(pu: &[(Dep, Vec<UseOverride>)], cpn: &str, token: &str) -> bool {
    let want = UseOverride::parse(token);
    pu.iter()
        .any(|(d, flags)| d.to_string() == cpn && flags.contains(&want))
}

// md5-cache helpers ---------------------------------------------------------

/// A leaf package with the given IUSE (space-separated, `+` for default-on).
fn leaf(iuse: &str, required_use: &str) -> String {
    let ru = if required_use.is_empty() {
        String::new()
    } else {
        format!("REQUIRED_USE={required_use}\n")
    };
    format!("EAPI=8\nSLOT=0\nIUSE={iuse}\nKEYWORDS=amd64\nDESCRIPTION=t\n{ru}")
}

/// A package whose RDEPEND is `rdepend`, with optional own IUSE.
fn parent(iuse: &str, rdepend: &str) -> String {
    format!("EAPI=8\nSLOT=0\nIUSE={iuse}\nKEYWORDS=amd64\nDESCRIPTION=t\nRDEPEND={rdepend}\n")
}

// ---------------------------------------------------------------------------
// CC1 — plain `[bar]`, target's bar is off: satisfiable by enabling bar.
// ---------------------------------------------------------------------------
#[test]
fn cc1_plain_enabled() {
    let data = repo_from(&[
        ("app/parent-1", &parent("", "dev/foo[bar]")),
        ("dev/foo-1", &leaf("bar", "")), // bar default off
    ]);
    // Default (Tier 2): detected, bar stays off (suggested only).
    let out = solve(&data, &["app/parent"]);
    assert!(out.has("dev/foo-1"), "foo is pulled into the plan");
    assert!(
        out.needs_enabled("dev/foo", "bar"),
        "default: detected only"
    );
    // C7 (autosolve): bar is forced on foo and the requirement is satisfied.
    let (pu, co) = cosolve(&data, &["app/parent"]);
    assert!(pu_forces(&pu, "dev/foo", "bar"), "C7 forces bar on foo");
    assert!(
        !co.needs_enabled("dev/foo", "bar"),
        "C7: requirement satisfied"
    );
    assert!(co.has("dev/foo-1"));
}

// ---------------------------------------------------------------------------
// CC2 — plain `[-bar]`, target's bar is on by default: needs disabling.
// ---------------------------------------------------------------------------
#[test]
fn cc2_plain_disabled() {
    let data = repo_from(&[
        ("app/parent-1", &parent("", "dev/foo[-bar]")),
        ("dev/foo-1", &leaf("+bar", "")), // bar default ON
    ]);
    assert!(
        solve(&data, &["app/parent"]).needs_disabled("dev/foo", "bar"),
        "default: detected"
    );
    let (pu, co) = cosolve(&data, &["app/parent"]);
    assert!(pu_forces(&pu, "dev/foo", "-bar"), "C7 forces bar off foo");
    assert!(
        !co.needs_disabled("dev/foo", "bar"),
        "C7: requirement satisfied"
    );
}

// ---------------------------------------------------------------------------
// CC3 — conditional `[bar?]`: target needs bar only if the parent has bar.
// ---------------------------------------------------------------------------
#[test]
fn cc3_conditional_parent_flag_on() {
    let data = repo_from(&[
        ("app/parent-1", &parent("+bar", "dev/foo[bar?]")), // parent bar on
        ("dev/foo-1", &leaf("bar", "")),
    ]);
    assert!(
        solve(&data, &["app/parent"]).needs_enabled("dev/foo", "bar"),
        "default: detected"
    );
    let (pu, co) = cosolve(&data, &["app/parent"]);
    assert!(
        pu_forces(&pu, "dev/foo", "bar"),
        "C7 matches the active conditional"
    );
    assert!(!co.needs_enabled("dev/foo", "bar"));
}

// ---------------------------------------------------------------------------
// CC4 — equality `[bar=]`: target's bar must equal the parent's bar.
// ---------------------------------------------------------------------------
#[test]
fn cc4_equal() {
    let data = repo_from(&[
        ("app/parent-1", &parent("+bar", "dev/foo[bar=]")), // parent bar on
        ("dev/foo-1", &leaf("bar", "")),                    // foo bar off ⇒ mismatch
    ]);
    assert!(
        solve(&data, &["app/parent"]).needs_enabled("dev/foo", "bar"),
        "default: detected"
    );
    let (pu, co) = cosolve(&data, &["app/parent"]);
    assert!(
        pu_forces(&pu, "dev/foo", "bar"),
        "C7 makes foo bar equal parent bar"
    );
    assert!(!co.needs_enabled("dev/foo", "bar"));
}

// ---------------------------------------------------------------------------
// CC5 — `[bar]` collides with the target's REQUIRED_USE (`?? ( bar baz )`,
// baz default-on): C7 forces bar on, Level-C must drop baz in the same re-solve.
// ---------------------------------------------------------------------------
#[test]
fn cc5_dep_interacts_with_target_required_use() {
    let data = repo_from(&[
        ("app/parent-1", &parent("", "dev/foo[bar]")),
        ("dev/foo-1", &leaf("bar +baz", "?? ( bar baz )")), // baz on; bar+baz illegal
    ]);
    assert!(
        solve(&data, &["app/parent"]).needs_enabled("dev/foo", "bar"),
        "default: detected"
    );
    // C7 forces bar; the solve still succeeds because Level-C cedes baz off so
    // `?? ( bar baz )` holds — C7 and Level-C co-operate in one re-solve.
    let (pu, co) = cosolve(&data, &["app/parent"]);
    assert!(pu_forces(&pu, "dev/foo", "bar"));
    assert!(
        co.has("dev/foo-1"),
        "plan is still produced (RU satisfied via baz)"
    );
    assert!(
        !co.needs_enabled("dev/foo", "bar"),
        "C7: requirement satisfied"
    );
}

// ---------------------------------------------------------------------------
// CC6 — two parents demand opposite values of the same flag. C7 must terminate
// (no oscillation): first-seen wins, the loser stays a Tier-2 advisory.
// ---------------------------------------------------------------------------
#[test]
fn cc6_conflicting_parents_terminate() {
    let data = repo_from(&[
        ("app/p1-1", &parent("", "dev/foo[bar]")),
        ("app/p2-1", &parent("", "dev/foo[-bar]")),
        ("dev/foo-1", &leaf("bar", "")),
    ]);
    // Tier-2 default: the conflict is surfaced rather than hidden. foo's bar is
    // off, so p2's `[-bar]` is already satisfied; without folding p1's pending
    // `enable` into the target state, only `enable` would be recorded and the
    // contradiction would stay invisible. Now both sides are reported.
    let out = solve(&data, &["app/p1", "app/p2"]);
    assert!(out.needs_enabled("dev/foo", "bar"), "p1's [bar] recorded");
    assert!(
        out.needs_disabled("dev/foo", "bar"),
        "p2's [-bar] recorded too — conflict surfaced, not lost"
    );

    // Co-solve must terminate and apply exactly one direction (it does not loop).
    let (pu, _co) = cosolve(&data, &["app/p1", "app/p2"]);
    let forces_on = pu_forces(&pu, "dev/foo", "bar");
    let forces_off = pu_forces(&pu, "dev/foo", "-bar");
    assert!(
        forces_on ^ forces_off,
        "exactly one side is applied, never both, never looping"
    );
}

// ---------------------------------------------------------------------------
// CC7 — `[bar]` where bar is not in the target's IUSE: cannot be applied.
// ---------------------------------------------------------------------------
#[test]
fn cc7_flag_absent_from_target_iuse() {
    let data = repo_from(&[
        ("app/parent-1", &parent("", "dev/foo[bar]")),
        ("dev/foo-1", &leaf("other", "")), // no bar IUSE
    ]);
    let (pu, co) = cosolve(&data, &["app/parent"]);
    assert!(co.has("dev/foo-1"), "foo still resolves");
    assert!(
        !pu_forces(&pu, "dev/foo", "bar"),
        "C7 never forces a non-IUSE flag"
    );
}
