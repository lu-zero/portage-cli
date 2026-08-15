//! Pre-flight build-dependency check (todo/build-roadmap.md, M2).
//!
//! Before the (potentially hours-long) build loop, verify that every plan
//! entry's build-time dependencies are present in the set that will be visible
//! when it builds:
//!
//! - a **Target**-routed entry's `DEPEND` is resolved against
//!   `roots.satisfaction_root(DepClass::Depend)` (BROOT for a native/
//!   same-arch build — there's no separate build sysroot when
//!   `CBUILD==CHOST` — or the target sysroot for a genuine cross build),
//!   plus the target's own VDB whenever it differs from that satisfaction
//!   root (`Avail::initial_depend`, so a package already built into a
//!   partially populated `--root`/`--prefix` from an earlier run still
//!   counts, even though the host lacks it);
//! - a **Host**-routed entry's `DEPEND` — it's built *at* `BROOT`, so its
//!   own build-time deps live there too — and every entry's `BDEPEND` are
//!   both resolved against the build host's own `BROOT` (`roots.satisfaction_root(DepClass::BDepend)`),
//!   *not* unconditionally bare `/` (a `--root`/`--prefix`/`--local`
//!   invocation's Host merges land at that BROOT, so that's what must be
//!   checked).
//!
//! Both sets grow with each earlier plan entry: a package merged earlier in the
//! run is visible to everything after it (root-model.md "within-run
//! visibility"). The solver already produces a complete dependency closure, so
//! this is a guard rail — it turns a would-be mid-build "command not found" /
//! missing-library failure into a clear, early error that names the dep.

use std::collections::HashSet;

use anyhow::{Result, bail};
use portage_atom::interner::Interned;
use portage_atom::{Cpv, DepEntry};

use portage_atom_pubgrub::MergeRoot;
use portage_resolve::Roots;
use portage_resolve::{Avail, collect_unsatisfied};

use crate::query::depgraph::PlannedMerge;

/// Verify the plan's build dependencies are satisfiable in install order.
///
/// One `roots` value answers both `DEPEND` (via `initial_depend`) and
/// `BDEPEND` (via `initial_bdepend`, `roots.satisfaction_root(DepClass::BDepend)`)
/// — `roots.broot` is carried correctly even under an active `--target`
/// sysroot substitution, so a separate `host_roots` parameter is no longer
/// needed (see `Cli::roots`'s doc comment).
///
/// `hard_cycle_edges` names pairs already known to sit in a genuine
/// irreducible dependency cycle — a failure matching one gets a distinct
/// note instead of reading like a missing package.
///
/// Returns an error listing every unsatisfied requirement (package → missing
/// atoms) when the check fails; `Ok(())` otherwise.
pub fn check(
    plan: &[PlannedMerge],
    roots: &Roots,
    provided: &[(Cpv, Option<String>)],
    hard_cycle_edges: &[(Cpv, Cpv)],
) -> Result<()> {
    let mut depend_avail = Avail::initial_depend(roots);
    let mut bdepend_avail = Avail::initial_bdepend(roots);

    // `package.provided` packages are supplied by the system on both roots.
    for (cpv, slot) in provided {
        depend_avail.record_provided(cpv.clone(), slot.clone());
        bdepend_avail.record_provided(cpv.clone(), slot.clone());
    }

    let mut problems: Vec<String> = Vec::new();
    for planned in plan {
        let active: HashSet<Interned<_>> = planned.use_flags.iter().copied().collect();
        let depend = DepEntry::evaluate_use(&planned.depend, &active);
        let bdepend = DepEntry::evaluate_use(&planned.bdepend, &active);

        // A Host entry is built *at* `base_roots()` (BROOT), so its own
        // DEPEND — not just BDEPEND — must be checked against that same
        // view, not `depend_avail` (the target/base sysroot's, a different
        // root entirely). Checking it against `depend_avail` regardless of
        // `merge_root` made a Host package's DEPEND on another Host-merged
        // package (e.g. `dev-lang/perl` on `sys-libs/gdbm`, both routed to
        // `base_roots()`) spuriously fail: `depend_avail` only grows from
        // Target merges, so it never saw the earlier Host merge at all.
        let mut missing: Vec<String> = Vec::new();
        match planned.merge_root {
            MergeRoot::Host => collect_unsatisfied(&depend, &bdepend_avail, &mut missing),
            MergeRoot::Target => collect_unsatisfied(&depend, &depend_avail, &mut missing),
        }
        collect_unsatisfied(&bdepend, &bdepend_avail, &mut missing);
        if !missing.is_empty() {
            missing.sort();
            missing.dedup();
            problems.push(format!("  {} needs: {}", planned.cpv, missing.join(", ")));
            let cycle_partners: Vec<&Cpv> = hard_cycle_edges
                .iter()
                .filter(|(dependent, _)| *dependent == planned.cpv)
                .map(|(_, dependency)| dependency)
                .collect();
            if !cycle_partners.is_empty() {
                let cpv = &planned.cpv;
                let names = cycle_partners
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                problems.push(format!(
                    "    (not a missing package — a real hard-dependency cycle: \
                     {cpv} and {names})"
                ));
            }
        }

        // Within-run visibility: this entry satisfies later entries' deps once
        // merged. Slot is left unknown (None) — a permissive presence check,
        // which is the right bias for a guard that must not block a valid plan.
        match planned.merge_root {
            MergeRoot::Host => bdepend_avail.record_merge_bdepend(planned.cpv.clone()),
            MergeRoot::Target => {
                bdepend_avail.record_target_merge(&mut depend_avail, planned.cpv.clone())
            }
        }
    }

    if !problems.is_empty() {
        bail!(
            "pre-flight dependency check failed — these build dependencies are not \
             satisfied by the installed view or earlier plan entries:\n{}",
            problems.join("\n")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a plan entry from an already-parsed [`Cpv`] — never re-derives
    /// identity from a string internally, matching the rest of the merge
    /// path.
    fn planned(merge_root: MergeRoot, cpv: Cpv, depend: &str) -> Result<PlannedMerge> {
        Ok(PlannedMerge {
            merge_root,
            cpv,
            ebuild_path: camino::Utf8PathBuf::new(),
            use_flags: Vec::new(),
            depend: DepEntry::parse(depend)?,
            bdepend: Vec::new(),
            reinstall: false,
        })
    }

    fn roots_at(tmp: &tempfile::TempDir) -> Result<Roots> {
        let path = tmp
            .path()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("tempdir path is not valid UTF-8"))?;
        Ok(Roots::for_test(path))
    }

    /// Regression test for the riscv64 stage3 shakeout (#31): a `Host`
    /// entry's own DEPEND on an *earlier* `Host` entry in the same plan
    /// (both routed to `base_roots()`, e.g. `dev-lang/perl` on
    /// `sys-libs/gdbm` in a self-contained native bootstrap) must be seen
    /// as satisfied — checking it against `depend_avail` (which only grows
    /// from Target merges) made it spuriously fail.
    ///
    /// Uses an isolated tempdir root (via `Roots::for_test`), not
    /// `Roots::default()`: the default falls through to the *real* bare
    /// host `/var/db/pkg`, which may already have `gdbm`/`perl` installed on
    /// the machine running the test, silently passing regardless of the fix.
    #[test]
    fn host_entry_depend_satisfied_by_earlier_host_entry() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let roots = roots_at(&tmp)?;
        let plan = vec![
            planned(MergeRoot::Host, Cpv::parse("sys-libs/gdbm-1.26")?, "")?,
            planned(
                MergeRoot::Host,
                Cpv::parse("dev-lang/perl-5.42.2")?,
                ">=sys-libs/gdbm-1.8.3:=",
            )?,
        ];
        assert!(check(&plan, &roots, &[], &[]).is_ok());
        Ok(())
    }

    /// Negative control: a `Target` entry's DEPEND on a `Host`-only merge is
    /// *not* satisfied — the two roots are genuinely different, and Host
    /// merges must not leak into the Target/base-system view.
    #[test]
    fn target_entry_depend_not_satisfied_by_host_only_entry() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let roots = roots_at(&tmp)?;
        let plan = vec![
            planned(MergeRoot::Host, Cpv::parse("sys-libs/gdbm-1.26")?, "")?,
            planned(
                MergeRoot::Target,
                Cpv::parse("dev-lang/perl-5.42.2")?,
                ">=sys-libs/gdbm-1.8.3:=",
            )?,
        ];
        assert!(check(&plan, &roots, &[], &[]).is_err());
        Ok(())
    }

    /// A failure caused by a genuine hard-dependency cycle (`hard_cycle_edges`
    /// names the pair) gets the distinct cycle note appended, not just the
    /// generic "needs: X".
    #[test]
    fn hard_cycle_failure_gets_a_distinct_note() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let roots = roots_at(&tmp)?;
        let zstd = Cpv::parse("app-arch/zstd-1.5.7")?;
        let meson = Cpv::parse("dev-build/meson-1.12.0")?;
        let plan = vec![planned(
            MergeRoot::Host,
            zstd.clone(),
            ">=dev-build/meson-1.2.3",
        )?];
        let err = check(&plan, &roots, &[], &[(zstd, meson)])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("real hard-dependency cycle"),
            "expected the cycle note, got: {err}"
        );
        Ok(())
    }

    /// A `:slot` build dep on a system-supplied `package.provided` package is
    /// satisfied by the seeded provided entry (the interpreter case: the plan's
    /// python packages `DEPEND` on `dev-lang/python:3.14`, which is provided by
    /// the host and never merged). Without the seed the check fails.
    #[test]
    fn provided_slotted_dep_is_satisfied() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let roots = roots_at(&tmp)?;
        let plan = vec![planned(
            MergeRoot::Host,
            Cpv::parse("dev-python/wheel-0.47.0")?,
            "dev-lang/python:3.14",
        )?];
        assert!(check(&plan, &roots, &[], &[]).is_err(), "unseeded control");

        let provided = vec![(Cpv::parse("dev-lang/python-3.14.0")?, Some("3.14".into()))];
        assert!(check(&plan, &roots, &provided, &[]).is_ok());
        Ok(())
    }
}
