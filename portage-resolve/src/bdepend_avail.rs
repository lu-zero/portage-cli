//! BROOT / within-run package availability for `BDEPEND` checks
//!
//! Shared by `em`'s `preflight` module (validate) and the depgraph post-solve
//! `BDEPEND` trim pass.

use std::cell::OnceCell;
use std::collections::HashMap;

use camino::Utf8Path;
use portage_atom::Slot;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, DepEntry, UseDefault, UseDep, UseDepKind};
use portage_atom_pubgrub::{DepClass, MergeRoot};
use portage_vdb::{InstalledPackage, Vdb};

use crate::Roots;
use crate::installed::BrootSnapshot;

/// Interned `(enabled, iuse)` USE state — see `AvailEntry::interned_use`
type InternedUse = (
    Vec<Interned<DefaultInterner>>,
    Vec<Interned<DefaultInterner>>,
);

/// One entry in an [`Avail`] set: an installed/available `(cpv, main-slot)`,
/// plus the installed package itself when it's an authoritative source of
/// USE info
#[derive(Debug, Clone)]
struct AvailEntry {
    cpv: Cpv,
    slot: Option<Interned<DefaultInterner>>,
    /// The VDB-backed installed package this entry came from, when known
    ///
    /// Lets `atom_satisfied` verify USE-dep brackets (PMS 8.3.4) against its
    /// USE/IUSE instead of just CPN/version/slot.
    ///
    /// `None` for within-run solved-plan merges: the solver's own `check_use_deps`
    /// already validates USE-dep constraints among those, so re-checking here would
    /// duplicate that without the parent-flag context it needs.
    ///
    /// Deliberately *not* read eagerly: most `AvailEntry`s built for a whole
    /// VDB are never checked against a USE-dep atom (eager reads cost ~1.3s
    /// of pure I/O on a 712-package host, a real regression found via
    /// `em -p` benchmarking). Reading lazily, only for an entry that already
    /// matched and whose atom has USE-dep brackets, keeps the common case free.
    installed: Option<InstalledPackage>,
    /// Interned `(enabled, iuse)` USE state, cached on first use-dep check:
    /// a single pass can check the same entry against several atoms, and
    /// `use_flags()`/`iuse()` each allocate a fresh `Vec<String>` per call
    /// even though the file read itself is cached — this cell avoids
    /// redoing that allocation+parse per repeat check
    interned_use: OnceCell<InternedUse>,
}

/// Installed `(cpv, main-slot)` pairs visible for dependency presence checks
#[derive(Debug, Clone, Default)]
pub struct Avail {
    entries: Vec<AvailEntry>,
    by_cpn: HashMap<Cpn, Vec<usize>>,
}

impl Avail {
    fn from_entries(entries: Vec<AvailEntry>) -> Self {
        let mut by_cpn: HashMap<Cpn, Vec<usize>> = HashMap::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            by_cpn.entry(entry.cpv.cpn).or_default().push(index);
        }
        Self { entries, by_cpn }
    }

    fn push(&mut self, entry: AvailEntry) {
        let index = self.entries.len();
        self.by_cpn.entry(entry.cpv.cpn).or_default().push(index);
        self.entries.push(entry);
    }

    /// `BDEPEND` availability at the start of a run: the build host's own
    /// `BROOT` — `roots.satisfaction_root(DepClass::Bdepend)`, carried
    /// correctly on `roots` even under an active `--target` sysroot
    /// substitution, so the *same* `Roots` value passed for `DEPEND` checks
    /// answers this too (mirrors `load_host_installed`'s fix for the same
    /// bug in the solver's own host-installed view)
    ///
    /// `--prefix` (an unprivileged overlay) additionally weaves in the
    /// prefix's own VDB: `Cli::host_roots()` sends an unsatisfied BDEPEND
    /// there (the overlay can't write the real host `/`), so a package
    /// already built into the prefix by a previous run also counts as
    /// satisfied. Not done for `--root`/`--local`: nothing is ever merged
    /// anywhere but the single satisfaction root there.
    pub fn initial_bdepend(roots: &Roots) -> Self {
        Self::initial_bdepend_from_snapshot(&BrootSnapshot::load(roots))
    }

    /// `BDEPEND` availability from one raw BROOT/prefix snapshot.
    ///
    /// Both roots remain visible: the permissive availability view treats a
    /// package as present if either installed row satisfies the atom.
    pub fn initial_bdepend_from_snapshot(snapshot: &BrootSnapshot) -> Self {
        Self::from_entries(avail_entries_from(snapshot.packages()))
    }

    /// `DEPEND` availability
    ///
    /// VDB at `satisfaction_root(Depend)`, plus the target VDB when it differs.
    ///
    /// Use `merge_root() != satisfaction_root` (not only `is_overlay()`): bare
    /// `--root` resolves DEPEND against BROOT, so a partially populated target must
    /// still count as satisfied.
    pub fn initial_depend(roots: &Roots) -> Self {
        let depend_root = roots.satisfaction_root(DepClass::Depend);
        let mut out = vdb_avail_entries(Some(depend_root));
        if roots.merge_root() != depend_root {
            out.extend(vdb_avail_entries(Some(roots.merge_root())));
        }
        Self::from_entries(out)
    }

    /// `DEPEND` availability against a fixed sysroot (`ESYSROOT`)
    ///
    /// `None` is the host `/var/db/pkg`.
    pub fn initial_sysroot_depend(sysroot: Option<&camino::Utf8Path>) -> Self {
        Self::from_entries(vdb_avail_entries(sysroot))
    }

    /// `DEPEND` availability at the toolchain sysroot alone (`ESYSROOT`),
    /// under the board-root topology
    ///
    /// Unlike [`initial_depend`](Self::initial_depend), the target `ROOT`'s
    /// own VDB is deliberately **not** woven in: this answers "does the
    /// *sysroot* have it", the question `root_closure::base` schedules a merge
    /// from. A provider present only in the board root does not satisfy a
    /// build-time `DEPEND` there — its headers/libs/`.pc` are not on the
    /// compiler's sysroot search path.
    ///
    /// Reads [`Roots::satisfaction_root`]`(DepClass::Depend)` rather than a
    /// raw path, so it cannot drift from the rule `Roots` itself encodes.
    pub fn initial_base_depend(roots: &Roots) -> Self {
        Self::from_entries(vdb_avail_entries(Some(
            roots.satisfaction_root(DepClass::Depend),
        )))
    }

    /// Target `ROOT` visibility from an explicit set of installed CPVs
    pub fn from_cpvs(cpvs: Vec<(Cpv, Option<String>)>) -> Self {
        Self::from_entries(
            cpvs.into_iter()
                .map(|(cpv, slot)| AvailEntry {
                    cpv,
                    slot: slot.as_deref().map(Interned::intern),
                    installed: None,
                    interned_use: OnceCell::new(),
                })
                .collect(),
        )
    }

    /// Record a `package.provided` entry as present with its slot
    ///
    /// A system-supplied package, so a build dep on it is satisfied without
    /// a merge.
    ///
    /// Slot is authoritative for the match; USE-deps on such an atom are treated as
    /// satisfied (`installed: None`), matching the solver, which counts the
    /// provided package as present regardless of flags.
    pub fn record_provided(&mut self, cpv: Cpv, slot: Option<Interned<DefaultInterner>>) {
        self.push(AvailEntry {
            cpv,
            slot,
            installed: None,
            interned_use: OnceCell::new(),
        });
    }

    /// Record a host merge visible to later `BDEPEND` checks
    pub fn record_merge_bdepend(&mut self, cpv: Cpv) {
        self.push(AvailEntry {
            cpv,
            slot: None,
            installed: None,
            interned_use: OnceCell::new(),
        });
    }

    /// Record a target merge for both DEPEND and BDEPEND views (preflight)
    pub fn record_target_merge(&mut self, depend: &mut Self, cpv: Cpv) {
        depend.push(AvailEntry {
            cpv: cpv.clone(),
            slot: None,
            installed: None,
            interned_use: OnceCell::new(),
        });
        self.push(AvailEntry {
            cpv,
            slot: None,
            installed: None,
            interned_use: OnceCell::new(),
        });
    }

    /// Record a merge for within-run `BDEPEND` trim (host or target)
    pub fn record_merge(&mut self, cpv: Cpv, _merge_root: MergeRoot) {
        self.push(AvailEntry {
            cpv,
            slot: None,
            installed: None,
            interned_use: OnceCell::new(),
        });
    }

    /// Whether `dep` is already satisfied by this availability set (CPN,
    /// version, slot, and any USE-dep brackets — see the `use_deps_satisfied`
    /// function)
    pub fn atom_satisfied(&self, dep: &Dep) -> bool {
        self.by_cpn.get(&dep.cpn).is_some_and(|indices| {
            indices.iter().any(|&index| {
                let entry = &self.entries[index];
                let slot = entry.slot.map(Slot::from_name);
                dep.matches_cpv(&entry.cpv, slot.as_ref()) && use_deps_satisfied(dep, entry)
            })
        })
    }

    /// `true` when `entries` contain an unsatisfied atom on `cpn`
    pub fn has_unsatisfied_atom_for_cpn(&self, entries: &[DepEntry], cpn: Cpn) -> bool {
        has_unsatisfied_atom_for_cpn(self, entries, cpn)
    }
}

/// Availability source used by dependency-tree checks.
pub(crate) trait Availability {
    fn atom_satisfied(&self, dep: &Dep) -> bool;
}

impl Availability for Avail {
    fn atom_satisfied(&self, dep: &Dep) -> bool {
        Avail::atom_satisfied(self, dep)
    }
}

/// A read-only availability view over a base set and earlier plan entries.
pub(crate) struct AvailPrefix<'a> {
    base: Option<&'a Avail>,
    extra: &'a [Cpv],
}

impl<'a> AvailPrefix<'a> {
    pub(crate) fn new(base: Option<&'a Avail>, extra: &'a [Cpv]) -> Self {
        Self { base, extra }
    }

    pub(crate) fn has_unsatisfied_atom_for_cpn(&self, entries: &[DepEntry], cpn: Cpn) -> bool {
        has_unsatisfied_atom_for_cpn(self, entries, cpn)
    }
}

impl Availability for AvailPrefix<'_> {
    fn atom_satisfied(&self, dep: &Dep) -> bool {
        self.base.is_some_and(|base| base.atom_satisfied(dep))
            || self.extra.iter().any(|cpv| dep.matches_cpv(cpv, None))
    }
}

fn has_unsatisfied_atom_for_cpn<A: Availability + ?Sized>(
    avail: &A,
    entries: &[DepEntry],
    cpn: Cpn,
) -> bool {
    entries
        .iter()
        .any(|e| entry_unsatisfied_for_cpn(e, cpn, avail))
}

/// Whether `dep`'s USE-dep brackets (if any) are satisfied by `entry`
///
/// `entry.installed: None` means "no authoritative USE data" (a within-run
/// solved merge already validated by the solver) — always satisfied here.
/// Only the simple `[flag]`/`[-flag]` forms are checked; `[flag?]`/`[flag=]`
/// and their inverses need the *parent* package's own flag state, which
/// `Avail` has no visibility into, so those are conservatively treated as
/// satisfied (same as the prior behaviour for every USE-dep form).
///
/// `USE`/`IUSE` are read from `entry.installed` lazily (only after a
/// cpv/slot match, only when `dep` has USE-dep brackets — see
/// [`AvailEntry::installed`]) and memoized per entry (see
/// [`AvailEntry::interned_use`]), so a package checked against several
/// USE-dep atoms in the same pass only pays the read+parse once.
fn use_deps_satisfied(dep: &Dep, entry: &AvailEntry) -> bool {
    let Some(use_deps) = &dep.use_deps else {
        return true;
    };
    let Some(pkg) = &entry.installed else {
        return true;
    };
    let (enabled, iuse) = entry.interned_use.get_or_init(|| {
        let enabled = pkg
            .use_flags()
            .unwrap_or_default()
            .into_iter()
            .map(|f| Interned::intern(&f))
            .collect();
        let iuse = pkg
            .iuse()
            .unwrap_or_default()
            .iter()
            .map(Interned::from)
            .collect();
        (enabled, iuse)
    });
    use_deps
        .iter()
        .all(|ud| use_dep_satisfied(ud, enabled, iuse))
}

fn use_dep_satisfied(
    ud: &UseDep,
    enabled: &[Interned<DefaultInterner>],
    iuse: &[Interned<DefaultInterner>],
) -> bool {
    let flag = ud.flag;
    let enabled = if iuse.contains(&flag) {
        enabled.contains(&flag)
    } else {
        match ud.default {
            Some(UseDefault::Enabled) => true,
            Some(UseDefault::Disabled) | None => false,
        }
    };
    match ud.kind {
        UseDepKind::Enabled => enabled,
        UseDepKind::Disabled => !enabled,
        // Conditional/Equal forms depend on the parent's own flag state,
        // which isn't available here — see the doc comment above.
        UseDepKind::Conditional
        | UseDepKind::ConditionalInverse
        | UseDepKind::Equal
        | UseDepKind::EqualInverse => true,
    }
}

/// Like [`Avail::from_cpvs`], but keeps each installed package around
///
/// Lets [`Avail::atom_satisfied`] verify USE-dep brackets against them (see
/// [`AvailEntry::installed`]).
///
/// `None` = host `/var/db/pkg`.
fn vdb_avail_entries(root: Option<&Utf8Path>) -> Vec<AvailEntry> {
    let vdb = match root {
        Some(r) => Vdb::open(r.join("var/db/pkg")),
        None => Vdb::open_default(),
    };
    let Ok(vdb) = vdb else {
        return Vec::new();
    };
    let packages = vdb.packages().collect_vec();
    avail_entries_from(packages.iter())
}

fn avail_entries_from<'a>(packages: impl Iterator<Item = &'a InstalledPackage>) -> Vec<AvailEntry> {
    packages
        .map(|p| AvailEntry {
            cpv: p.cpv().clone(),
            slot: p.slot_main().ok(),
            installed: Some(p.clone()),
            interned_use: OnceCell::new(),
        })
        .collect()
}

/// Compatibility loader for callers that do not yet hold a shared snapshot.
pub fn broot_vdb_packages(roots: &Roots) -> Vec<InstalledPackage> {
    BrootSnapshot::load(roots).into_vec()
}

/// The CPNs of every unsatisfied (non-blocker) atom in `entries`
///
/// An `AnyOf` (`||`) or `ExactlyOneOf` (`^^`) group contributes its branch CPNs
/// only when no member is available. `AtMostOneOf` (`??`) is optional and
/// contributes none. `UseConditional`s are assumed already resolved by
/// `evaluate_use`. Used to find build-dep edges a root lacks (e.g. the native
/// offset host build-closure walk).
pub fn unsatisfied_cpns(entries: &[DepEntry], avail: &Avail) -> Vec<Cpn> {
    let mut out = Vec::new();
    unsat_cpns_rec(entries, avail, &mut out);
    out
}

/// Every non-blocker atom CPN in `entries`, satisfied or not
///
/// Unlike [`unsatisfied_cpns`], not filtered by `Avail`: used where a caller
/// needs to know *which provider a dependency resolves to* even when that
/// provider was already scheduled by an earlier, unrelated consumer (e.g.
/// `root_closure` recording a blocker edge on a shared entry).
pub fn all_cpns(entries: &[DepEntry]) -> Vec<Cpn> {
    let mut out = Vec::new();
    for e in entries {
        cpns_of(e, &mut out);
    }
    out
}

fn unsat_cpns_rec(entries: &[DepEntry], avail: &Avail, out: &mut Vec<Cpn>) {
    for e in entries {
        match e {
            DepEntry::Atom(dep) if dep.blocker.is_none() && !avail.atom_satisfied(dep) => {
                out.push(dep.cpn);
            }
            DepEntry::AllOf(c) => unsat_cpns_rec(c, avail, out),
            DepEntry::AnyOf(c) | DepEntry::ExactlyOneOf(c) if !group_satisfied(c, avail) => {
                for branch in c {
                    cpns_of(branch, out);
                }
            }
            _ => {}
        }
    }
}

/// Collect every non-blocker atom CPN mentioned in `e` (for an unsatisfied
/// `||`-group's branches)
fn cpns_of(e: &DepEntry, out: &mut Vec<Cpn>) {
    DepEntry::walk_atoms(std::slice::from_ref(e), &mut |dep| {
        if dep.blocker.is_none() {
            out.push(dep.cpn);
        }
    });
}

/// Append the display form of each unsatisfied requirement in `entries` to `out`
///
/// `UseConditional`s are assumed already resolved by `evaluate_use`.
pub fn collect_unsatisfied(entries: &[DepEntry], avail: &Avail, out: &mut Vec<String>) {
    for e in entries {
        match e {
            DepEntry::Atom(dep) if dep.blocker.is_some() => {}
            DepEntry::Atom(dep) => {
                if !avail.atom_satisfied(dep) {
                    out.push(dep.to_string());
                }
            }
            DepEntry::AllOf(children) => collect_unsatisfied(children, avail, out),
            DepEntry::AnyOf(children) => {
                if !group_satisfied(children, avail) {
                    out.push(e.to_string());
                }
            }
            DepEntry::ExactlyOneOf(children) => {
                if !group_satisfied(children, avail) {
                    out.push(e.to_string());
                }
            }
            DepEntry::AtMostOneOf(_) => {}
            DepEntry::UseConditional { .. } => {}
        }
    }
}

fn group_satisfied<A: Availability + ?Sized>(entries: &[DepEntry], avail: &A) -> bool {
    entries.iter().any(|e| entry_satisfied(e, avail))
}

/// Whether `e` is satisfied on `avail` (blockers count as satisfied)
fn entry_satisfied<A: Availability + ?Sized>(e: &DepEntry, avail: &A) -> bool {
    match e {
        DepEntry::Atom(dep) => dep.blocker.is_some() || avail.atom_satisfied(dep),
        DepEntry::AllOf(c) => c.iter().all(|e| entry_satisfied(e, avail)),
        DepEntry::AnyOf(c) | DepEntry::ExactlyOneOf(c) => group_satisfied(c, avail),
        DepEntry::AtMostOneOf(c) => {
            c.iter()
                .filter(|child| entry_satisfied(child, avail))
                .count()
                <= 1
        }
        DepEntry::UseConditional { .. } => true,
    }
}

fn entry_unsatisfied_for_cpn<A: Availability + ?Sized>(e: &DepEntry, cpn: Cpn, avail: &A) -> bool {
    match e {
        DepEntry::Atom(dep) if dep.blocker.is_some() => false,
        DepEntry::Atom(dep) if dep.cpn != cpn => false,
        DepEntry::Atom(dep) => !avail.atom_satisfied(dep),
        DepEntry::AllOf(c) => c.iter().any(|e| entry_unsatisfied_for_cpn(e, cpn, avail)),
        DepEntry::AnyOf(c) | DepEntry::ExactlyOneOf(c) => {
            !group_satisfied(c, avail) && c.iter().any(|child| branch_mentions_cpn(child, cpn))
        }
        DepEntry::AtMostOneOf(_) => false,
        DepEntry::UseConditional { .. } => false,
    }
}

fn branch_mentions_cpn(e: &DepEntry, cpn: Cpn) -> bool {
    let mut found = false;
    DepEntry::walk_atoms(std::slice::from_ref(e), &mut |dep| {
        found |= dep.cpn == cpn;
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atoms(specs: &[&str]) -> Avail {
        Avail::from_cpvs(
            specs
                .iter()
                .map(|s| (Cpv::parse(s).unwrap(), None))
                .collect(),
        )
    }

    // Like [`atoms`], but the entry carries a real installed package with
    // authoritative USE/IUSE, so USE-dep brackets get checked (mirrors
    // [`vdb_avail_entries`]'s output). `AvailEntry::installed` reads
    // `USE`/`IUSE` lazily from disk now (not a hand-buildable `UseInfo`
    // pair), so this writes a real fake VDB entry and opens it — the
    // tempdir is deliberately leaked (`into_path`) rather than dropped at
    // the end of this function, since the returned `Avail` only reads its
    // files on demand, when the caller's `atom_satisfied` runs.
    fn atom_with_use(spec: &str, enabled: &[&str], iuse: &[&str]) -> Avail {
        let cpv = Cpv::parse(spec).unwrap();
        let tmp = tempfile::tempdir().unwrap().keep();
        let pkg_dir = tmp
            .join("var/db/pkg")
            .join(cpv.cpn.category.as_ref())
            .join(format!("{}-{}", cpv.cpn.package, cpv.version));
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("EAPI"), "8").unwrap();
        std::fs::write(pkg_dir.join("SLOT"), "0").unwrap();
        std::fs::write(pkg_dir.join("CONTENTS"), "").unwrap();
        std::fs::write(pkg_dir.join("USE"), enabled.join(" ")).unwrap();
        std::fs::write(pkg_dir.join("IUSE"), iuse.join(" ")).unwrap();

        let roots = Roots::for_test(tmp.to_str().unwrap());
        Avail::initial_bdepend(&roots)
    }

    fn parse(dep: &str) -> Vec<DepEntry> {
        DepEntry::parse(dep).unwrap()
    }

    // Regression test for the `sys-apps/systemd-utils` stage3 failure: the
    // host had `dev-python/jinja2` installed, but only built for
    // `python_targets_python3_13`, not the `_14` this run actually needs.
    // The old CPN/version/slot-only check treated the atom as satisfied
    // regardless of the `[python_targets_python3_14(-)]` USE-dep bracket, so
    // `em` never scheduled a jinja2 rebuild and the target package's
    // `meson` configure failed with "python3 is missing modules: jinja2"
    #[test]
    fn use_dep_not_satisfied_by_installed_flag_mismatch() {
        let avail = atom_with_use(
            "dev-python/jinja2-3.1.6",
            &["python_targets_python3_13"],
            &[
                "python_targets_python3_12",
                "python_targets_python3_13",
                "python_targets_python3_14",
            ],
        );
        let dep = parse("dev-python/jinja2[python_targets_python3_14(-)]");
        let DepEntry::Atom(dep) = &dep[0] else {
            unreachable!()
        };
        assert!(
            !avail.atom_satisfied(dep),
            "jinja2 built only for py3.13 must not satisfy a py3.14 USE-dep"
        );
    }

    #[test]
    fn use_dep_satisfied_by_matching_installed_flag() {
        let avail = atom_with_use(
            "dev-python/jinja2-3.1.6",
            &["python_targets_python3_14"],
            &["python_targets_python3_13", "python_targets_python3_14"],
        );
        let dep = parse("dev-python/jinja2[python_targets_python3_14(-)]");
        let DepEntry::Atom(dep) = &dep[0] else {
            unreachable!()
        };
        assert!(avail.atom_satisfied(dep));
    }

    #[test]
    fn negated_use_dep_satisfied_when_flag_disabled() {
        let avail = atom_with_use("dev-libs/foo-1.0", &[], &["static-libs"]);
        let dep = parse("dev-libs/foo[-static-libs]");
        let DepEntry::Atom(dep) = &dep[0] else {
            unreachable!()
        };
        assert!(avail.atom_satisfied(dep));
    }

    // Within-run solved-plan entries (no `use_info`) keep the old,
    // USE-dep-blind behaviour — the solver's own `check_use_deps` already
    // validated those, so `atom_satisfied` shouldn't re-check and risk a
    // false negative without the parent-flag context that requires
    #[test]
    fn use_dep_ignored_when_use_info_unknown() {
        let avail = atoms(&["dev-python/jinja2-3.1.6"]);
        let dep = parse("dev-python/jinja2[python_targets_python3_14(-)]");
        let DepEntry::Atom(dep) = &dep[0] else {
            unreachable!()
        };
        assert!(avail.atom_satisfied(dep));
    }

    #[test]
    fn satisfied_atom_is_not_reported() {
        let avail = atoms(&["dev-libs/foo-1.2"]);
        let mut out = Vec::new();
        collect_unsatisfied(&parse(">=dev-libs/foo-1.0"), &avail, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn missing_atom_is_reported() {
        let avail = atoms(&["dev-libs/foo-1.2"]);
        let mut out = Vec::new();
        collect_unsatisfied(&parse("dev-libs/bar"), &avail, &mut out);
        assert_eq!(out, ["dev-libs/bar"]);
    }

    #[test]
    fn has_unsatisfied_atom_for_cpn_detects_gap() {
        let avail = atoms(&["dev-build/b-1.0"]);
        let bdepend = parse(">=dev-build/b-2.0");
        assert!(avail.has_unsatisfied_atom_for_cpn(&bdepend, Cpn::parse("dev-build/b").unwrap()));
    }

    #[test]
    fn prefix_view_keeps_base_and_only_selected_plan_entries() {
        let base = atoms(&["dev-build/base-1"]);
        let extra = vec![Cpv::parse("dev-build/later-1").unwrap()];
        let entries = parse("|| ( dev-build/later dev-build/other )");
        let cpn = Cpn::parse("dev-build/later").unwrap();

        let with_extra = AvailPrefix::new(Some(&base), &extra);
        assert!(!with_extra.has_unsatisfied_atom_for_cpn(&entries, cpn));

        let without_extra = AvailPrefix::new(Some(&base), &[]);
        assert!(without_extra.has_unsatisfied_atom_for_cpn(&entries, cpn));
    }

    #[test]
    fn version_too_low_is_reported() {
        let avail = atoms(&["dev-libs/foo-1.2"]);
        let mut out = Vec::new();
        collect_unsatisfied(&parse(">=dev-libs/foo-2.0"), &avail, &mut out);
        assert_eq!(out, [">=dev-libs/foo-2.0"]);
    }

    #[test]
    fn any_of_satisfied_when_one_member_present() {
        let avail = atoms(&["dev-libs/b-1"]);
        let mut out = Vec::new();
        collect_unsatisfied(&parse("|| ( dev-libs/a dev-libs/b )"), &avail, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn blockers_are_ignored() {
        let avail = Avail::default();
        let mut out = Vec::new();
        collect_unsatisfied(&parse("!dev-libs/foo"), &avail, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn unsatisfied_cpns_returns_missing() {
        let avail = atoms(&["dev-libs/foo-1.2"]);
        let cpns: Vec<String> = unsatisfied_cpns(&parse("dev-libs/foo dev-libs/bar"), &avail)
            .into_iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(cpns, ["dev-libs/bar"]);
    }

    #[test]
    fn unsatisfied_cpns_any_of_unsatisfied_lists_branches() {
        // A fully-unsatisfied || group contributes every branch's CPN.
        let avail = Avail::default();
        let cpns: Vec<String> = unsatisfied_cpns(&parse("|| ( dev-libs/a dev-libs/b )"), &avail)
            .into_iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(cpns, ["dev-libs/a", "dev-libs/b"]);
    }

    #[test]
    fn unsatisfied_cpns_any_of_satisfied_is_empty() {
        let avail = atoms(&["dev-libs/b-1"]);
        let cpns = unsatisfied_cpns(&parse("|| ( dev-libs/a dev-libs/b )"), &avail);
        assert!(cpns.is_empty());
    }

    #[test]
    fn exactly_one_of_reports_missing_branches() {
        let avail = Avail::default();
        let deps = parse("^^ ( dev-libs/a dev-libs/b )");
        assert_eq!(
            unsatisfied_cpns(&deps, &avail)
                .into_iter()
                .map(|cpn| cpn.to_string())
                .collect::<Vec<_>>(),
            ["dev-libs/a", "dev-libs/b"]
        );
        assert!(avail.has_unsatisfied_atom_for_cpn(&deps, Cpn::parse("dev-libs/a").unwrap()));

        let mut out = Vec::new();
        collect_unsatisfied(&deps, &avail, &mut out);
        assert_eq!(out, ["^^ ( dev-libs/a dev-libs/b )"]);

        let optional = parse("?? ( dev-libs/c dev-libs/d )");
        assert!(unsatisfied_cpns(&optional, &avail).is_empty());
        assert!(!avail.has_unsatisfied_atom_for_cpn(&optional, Cpn::parse("dev-libs/c").unwrap()));
    }

    #[test]
    fn all_cpns_walks_exactly_one_and_at_most_one_groups() {
        let cpns = all_cpns(&parse(
            "^^ ( dev-libs/a dev-libs/b ) ?? ( dev-libs/c dev-libs/d )",
        ))
        .into_iter()
        .map(|cpn| cpn.to_string())
        .collect::<Vec<_>>();
        assert_eq!(
            cpns,
            ["dev-libs/a", "dev-libs/b", "dev-libs/c", "dev-libs/d"]
        );
    }

    #[test]
    fn any_of_does_not_require_a_branch_when_another_is_available() {
        let avail = atoms(&["dev-libs/b-1"]);
        let deps = parse("|| ( dev-libs/a dev-libs/b )");
        assert!(!avail.has_unsatisfied_atom_for_cpn(&deps, Cpn::parse("dev-libs/a").unwrap()));
    }

    // bug class as `load_host_installed` (installed.rs) — `initial_bdepend`
    // must read `host_roots`'s VDB, not unconditionally the bare host's
    #[test]
    fn initial_bdepend_reads_the_given_root_not_the_bare_host() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg_dir = tmp.path().join("var/db/pkg/dev-python/jinja2-3.1.6");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("EAPI"), "8").unwrap();
        std::fs::write(pkg_dir.join("SLOT"), "0").unwrap();
        std::fs::write(pkg_dir.join("CONTENTS"), "").unwrap();
        std::fs::write(pkg_dir.join("USE"), "").unwrap();

        let root_str = tmp.path().to_str().unwrap();
        let host_roots = Roots::for_test(root_str);
        let avail = Avail::initial_bdepend(&host_roots);

        let dep = parse("dev-python/jinja2");
        let DepEntry::Atom(dep) = &dep[0] else {
            unreachable!()
        };
        assert!(
            avail.atom_satisfied(dep),
            "must find the package via host_roots' VDB, not the bare host's"
        );
    }

    fn write_fake_vdb_entry(root: &std::path::Path, cpv: &str) {
        let pkg_dir = root.join("var/db/pkg").join(cpv);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("EAPI"), "8").unwrap();
        std::fs::write(pkg_dir.join("SLOT"), "0").unwrap();
        std::fs::write(pkg_dir.join("CONTENTS"), "").unwrap();
        std::fs::write(pkg_dir.join("USE"), "").unwrap();
    }

    // `--prefix`: a BDEPEND satisfied only by the prefix's own VDB (never
    // built into the real host) must still count as satisfied — the weave
    // this fixes, since `Cli::host_roots()` sends an unsatisfied one there
    #[test]
    fn initial_bdepend_weaves_in_the_prefix_vdb_under_overlay() {
        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        write_fake_vdb_entry(prefix.path(), "dev-python/jinja2-3.1.6");

        let roots = Roots::for_test_overlay(
            host.path().to_str().unwrap(),
            prefix.path().to_str().unwrap(),
        );
        let avail = Avail::initial_bdepend(&roots);

        let dep = parse("dev-python/jinja2");
        let DepEntry::Atom(dep) = &dep[0] else {
            unreachable!()
        };
        assert!(
            avail.atom_satisfied(dep),
            "a BDEPEND present only in the prefix's own VDB must count as satisfied"
        );
    }

    #[test]
    fn initial_bdepend_snapshot_keeps_the_host_prefix_union() {
        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        write_fake_vdb_entry(host.path(), "dev-libs/foo-1.0");
        write_fake_vdb_entry(prefix.path(), "dev-libs/foo-2.0");

        let roots = Roots::for_test_overlay(
            host.path().to_str().unwrap(),
            prefix.path().to_str().unwrap(),
        );
        let snapshot = BrootSnapshot::load(&roots);
        std::fs::remove_dir_all(host.path()).unwrap();
        std::fs::remove_dir_all(prefix.path()).unwrap();

        let avail = Avail::initial_bdepend_from_snapshot(&snapshot);
        for version in ["1.0", "2.0"] {
            let dep = Dep::parse(&format!("=dev-libs/foo-{version}")).unwrap();
            assert!(
                avail.atom_satisfied(&dep),
                "the snapshot must retain both union rows for {version}"
            );
        }
    }

    // Regression test: `initial_depend` must weave in the *target's* own
    // VDB for a bare `--root`, not just BROOT — a DEPEND provider already
    // built into a partially populated `--root` from an earlier run must
    // still count as satisfied even though the (real) host lacks it, or a
    // resumed stage build hits a false preflight failure. Found reviewing
    // the 2026-07-11 fix that made DEPEND resolve against BROOT for a
    // native build (`satisfaction_root(DepClass::Depend)`) — that fix
    // alone regressed this case by dropping the old `VDB(base) ∪
    // VDB(target)` weave entirely.
    #[test]
    fn initial_depend_weaves_in_the_target_vdb_for_a_bare_root() {
        let broot = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        write_fake_vdb_entry(target.path(), "dev-python/jinja2-3.1.6");

        let roots = Roots::for_test_root_with_broot(
            target.path().to_str().unwrap(),
            broot.path().to_str().unwrap(),
        );
        let avail = Avail::initial_depend(&roots);

        let dep = parse("dev-python/jinja2");
        let DepEntry::Atom(dep) = &dep[0] else {
            unreachable!()
        };
        assert!(
            avail.atom_satisfied(dep),
            "a DEPEND present only in the target's own VDB (built by an \
             earlier partial run) must still count as satisfied"
        );
    }

    // The same weave also still finds a BROOT-only entry — the target weave
    // adds the offset's VDB, it doesn't replace BROOT's
    #[test]
    fn initial_depend_still_finds_broot_only_entry_for_a_bare_root() {
        let broot = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        write_fake_vdb_entry(broot.path(), "dev-python/jinja2-3.1.6");

        let roots = Roots::for_test_root_with_broot(
            target.path().to_str().unwrap(),
            broot.path().to_str().unwrap(),
        );
        let avail = Avail::initial_depend(&roots);

        let dep = parse("dev-python/jinja2");
        let DepEntry::Atom(dep) = &dep[0] else {
            unreachable!()
        };
        assert!(
            avail.atom_satisfied(dep),
            "a DEPEND present only on BROOT (the host) must still count as satisfied"
        );
    }

    // The root cause of the readline/ncursesw incident: a provider present
    // only in the board root must NOT count as satisfying a build-time
    // DEPEND that root_closure::base is checking against the toolchain sysroot —
    // unlike initial_depend, this must never weave the target VDB in.
    #[test]
    fn initial_base_depend_ignores_the_target_vdb() {
        let sysroot = tempfile::tempdir().unwrap();
        let board = tempfile::tempdir().unwrap();
        write_fake_vdb_entry(board.path(), "sys-libs/ncurses-6.5");

        let roots = Roots::for_test_board_root(
            sysroot.path().to_str().unwrap(),
            board.path().to_str().unwrap(),
        );
        let avail = Avail::initial_base_depend(&roots);

        let dep = parse("sys-libs/ncurses");
        let DepEntry::Atom(dep) = &dep[0] else {
            unreachable!()
        };
        assert!(
            !avail.atom_satisfied(dep),
            "a provider only in the board root must not satisfy the sysroot's DEPEND view"
        );

        write_fake_vdb_entry(sysroot.path(), "sys-libs/ncurses-6.5");
        let roots = Roots::for_test_board_root(
            sysroot.path().to_str().unwrap(),
            board.path().to_str().unwrap(),
        );
        let avail = Avail::initial_base_depend(&roots);
        assert!(
            avail.atom_satisfied(dep),
            "a provider in the sysroot itself must satisfy it"
        );
    }

    // The same weave also still finds a host-only entry — the overlay adds
    // the prefix's VDB, it doesn't replace the host's
    #[test]
    fn initial_bdepend_still_finds_host_only_entry_under_overlay() {
        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        write_fake_vdb_entry(host.path(), "dev-python/jinja2-3.1.6");

        let roots = Roots::for_test_overlay(
            host.path().to_str().unwrap(),
            prefix.path().to_str().unwrap(),
        );
        let avail = Avail::initial_bdepend(&roots);

        let dep = parse("dev-python/jinja2");
        let DepEntry::Atom(dep) = &dep[0] else {
            unreachable!()
        };
        assert!(
            avail.atom_satisfied(dep),
            "a BDEPEND present only in the host's VDB must still count as satisfied"
        );
    }
}
