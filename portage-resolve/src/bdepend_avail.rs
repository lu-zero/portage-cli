//! BROOT / within-run package availability for `BDEPEND` checks.
//!
//! Shared by `em`'s `preflight` module (validate) and the depgraph post-solve
//! `BDEPEND` trim pass.

use std::cell::OnceCell;

use camino::Utf8Path;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, DepEntry, UseDefault, UseDep, UseDepKind};
use portage_atom_pubgrub::{DepClass, MergeRoot};
use portage_vdb::{InstalledPackage, Vdb};

use crate::Roots;

/// Interned `(enabled, iuse)` USE state — see `AvailEntry::interned_use`.
type InternedUse = (
    Vec<Interned<DefaultInterner>>,
    Vec<Interned<DefaultInterner>>,
);

/// One entry in an [`Avail`] set: an installed/available `(cpv, main-slot)`,
/// plus the installed package itself when it's an authoritative source of
/// USE info.
#[derive(Debug, Clone)]
struct AvailEntry {
    cpv: Cpv,
    slot: Option<String>,
    /// The VDB-backed installed package this entry came from, when known —
    /// letting `atom_satisfied` verify USE-dep brackets (PMS 8.3.4) against
    /// its USE/IUSE instead of just CPN/version/slot. `None` for within-run
    /// solved-plan merges (`record_merge` and friends): the solver's own
    /// `check_use_deps` already validates USE-dep constraints among those
    /// packages, so re-checking here would just duplicate that logic
    /// without the parent-flag context it needs.
    ///
    /// Deliberately *not* read eagerly into a `Vec<String>` pair at
    /// construction: `USE`/`IUSE` are separate on-disk files per package
    /// (`InstalledPackage::use_flags`/`iuse`), and the overwhelming majority
    /// of `AvailEntry`s constructed for a whole VDB (712 packages measured
    /// on one real host) are never checked against a USE-dep atom at all —
    /// eagerly reading both files for every one of them cost ~1.3s of
    /// almost pure file-I/O overhead nobody needed, a real regression found
    /// live via `em -p` benchmarking against `emerge -p`. Reading lazily,
    /// only inside `use_deps_satisfied` and only for an entry that already
    /// matched the atom's CPN/version/slot *and* only when the atom actually
    /// has USE-dep brackets to check, keeps the common case (no USE-deps on
    /// the atom at all, or an early match in the `.any()` scan) essentially
    /// free.
    installed: Option<InstalledPackage>,
    /// Interned `(enabled, iuse)` USE state, computed and cached on first
    /// use-dep check against this entry. A single `preflight`/trim pass can
    /// check the same entry against several different atoms (e.g. multiple
    /// BDEPEND edges on the same host package), and `use_flags()`/`iuse()`
    /// each allocate a fresh `Vec<String>` (with fresh `String`s per flag)
    /// on every call even though the underlying file read is itself cached
    /// (see `portage-vdb`'s `field_cache`) — this cell avoids redoing that
    /// allocation+parse for every repeat check of the same entry.
    interned_use: OnceCell<InternedUse>,
}

/// Installed `(cpv, main-slot)` pairs visible for dependency presence checks.
#[derive(Debug, Clone, Default)]
pub struct Avail(Vec<AvailEntry>);

impl Avail {
    /// `BDEPEND` availability at the start of a run: the build host's own
    /// `BROOT` — `roots.satisfaction_root(DepClass::Bdepend)`, which is
    /// carried correctly on `roots` even under an active `--target` sysroot
    /// substitution (see `Cli::roots`'s doc comment), so the *same* `Roots`
    /// value passed for `DEPEND` checks answers this too: an unsatisfied
    /// Host BDEPEND builds into that BROOT (`entry_roots()` in
    /// `merge/mod.rs`), so satisfaction must be checked against that same
    /// root's VDB, or a package built there on one run is never recognized
    /// as already satisfied on the next. This mirrors the
    /// `load_host_installed` fix for the same bug in the solver's own
    /// host-installed view.
    ///
    /// `--prefix` (an unprivileged overlay) additionally weaves in the
    /// prefix's own VDB: `Cli::broot()` now sends an unsatisfied BDEPEND
    /// there (the overlay can't write the real host `/`), so a package
    /// already built into the prefix by a previous run must also count as
    /// satisfied. Not done for `--root`/`--local`: there, nothing is ever
    /// merged anywhere but the single satisfaction root, so a second read
    /// would only risk a false positive from an unrelated package
    /// coincidentally present at the merge target.
    pub fn initial_bdepend(roots: &Roots) -> Self {
        Self(avail_entries_from(broot_vdb_packages(roots)))
    }

    /// `DEPEND` availability at the start of a run:
    /// `roots.satisfaction_root(DepClass::Depend)`'s VDB, plus the target's
    /// own VDB whenever it differs from that satisfaction root. This used to
    /// hardcode `VDB(base) ∪ VDB(target)` directly, bypassing
    /// `satisfaction_root` entirely — never migrated when that centralized
    /// the satisfaction-root logic (2026-07-09). For a bare `--root`, `base`
    /// and `target` used to both be the (possibly empty) offset, so that old
    /// logic never consulted the host at all: found 2026-07-11 comparing `em
    /// --root` against real `ROOT=X emerge`'s own (much smaller) plan for
    /// `sys-devel/gcc`.
    ///
    /// Checking `merge_root() != satisfaction_root(Depend)` (not
    /// `roots.is_overlay()`) is deliberate: a bare `--root` now resolves
    /// DEPEND against BROOT (the host) for a native build, which differs
    /// from the target offset just as much as `--prefix`'s overlay does — a
    /// package already built into a *partially populated* `--root` from an
    /// earlier run must still count as DEPEND-satisfied even though the host
    /// lacks it, or a resumed stage build hits a false preflight failure
    /// (found independently reviewing this same day's fix, before it ever
    /// shipped).
    pub fn initial_depend(roots: &Roots) -> Self {
        let depend_root = roots.satisfaction_root(DepClass::Depend);
        let mut out = vdb_avail_entries(Some(depend_root));
        if roots.merge_root() != depend_root {
            out.extend(vdb_avail_entries(Some(roots.merge_root())));
        }
        Self(out)
    }

    /// `DEPEND` availability against a fixed sysroot (`ESYSROOT`). `None` is
    /// the host `/var/db/pkg`.
    pub fn initial_sysroot_depend(sysroot: Option<&camino::Utf8Path>) -> Self {
        Self(vdb_avail_entries(sysroot))
    }

    /// Target `ROOT` visibility from an explicit set of installed CPVs.
    pub fn from_cpvs(cpvs: Vec<(Cpv, Option<String>)>) -> Self {
        Self(
            cpvs.into_iter()
                .map(|(cpv, slot)| AvailEntry {
                    cpv,
                    slot,
                    installed: None,
                    interned_use: OnceCell::new(),
                })
                .collect(),
        )
    }

    /// Record a `package.provided` entry (a system-supplied package) as present
    /// with its slot, so a build dep on it is satisfied without a merge. Slot is
    /// authoritative for the match; USE-deps on such an atom are treated as
    /// satisfied (`installed: None`), matching the solver, which counts the
    /// provided package as present regardless of flags.
    pub fn record_provided(&mut self, cpv: Cpv, slot: Option<String>) {
        self.0.push(AvailEntry {
            cpv,
            slot,
            installed: None,
            interned_use: OnceCell::new(),
        });
    }

    /// Record a host merge visible to later `BDEPEND` checks.
    pub fn record_merge_bdepend(&mut self, cpv: Cpv) {
        self.0.push(AvailEntry {
            cpv,
            slot: None,
            installed: None,
            interned_use: OnceCell::new(),
        });
    }

    /// Record a target merge for both DEPEND and BDEPEND views (preflight).
    pub fn record_target_merge(&mut self, depend: &mut Self, cpv: Cpv) {
        depend.0.push(AvailEntry {
            cpv: cpv.clone(),
            slot: None,
            installed: None,
            interned_use: OnceCell::new(),
        });
        self.0.push(AvailEntry {
            cpv,
            slot: None,
            installed: None,
            interned_use: OnceCell::new(),
        });
    }

    /// Record a merge for within-run `BDEPEND` trim (host or target).
    pub fn record_merge(&mut self, cpv: Cpv, _merge_root: MergeRoot) {
        self.0.push(AvailEntry {
            cpv,
            slot: None,
            installed: None,
            interned_use: OnceCell::new(),
        });
    }

    /// Whether `dep` is already satisfied by this availability set (CPN,
    /// version, slot, and any USE-dep brackets — see the `use_deps_satisfied`
    /// function).
    pub fn atom_satisfied(&self, dep: &Dep) -> bool {
        self.0
            .iter()
            .any(|e| dep.matches_cpv(&e.cpv, e.slot.as_deref()) && use_deps_satisfied(dep, e))
    }

    /// `true` when `entries` contain an unsatisfied atom on `cpn`.
    pub fn has_unsatisfied_atom_for_cpn(&self, entries: &[DepEntry], cpn: Cpn) -> bool {
        entries
            .iter()
            .any(|e| entry_unsatisfied_for_cpn(e, cpn, self))
    }
}

/// Whether `dep`'s USE-dep brackets (if any) are satisfied by `entry`.
///
/// `entry.installed: None` means "no authoritative USE data" (a within-run
/// solved merge already validated by the solver) — always satisfied here.
/// Only the simple `[flag]`/`[-flag]` forms are checked; `[flag?]`/`[flag=]`
/// and their inverses need the *parent* package's own flag state, which
/// `Avail` has no visibility into, so those are conservatively treated as
/// satisfied (same as the prior behaviour for every USE-dep form).
///
/// `USE`/`IUSE` are read from `entry.installed` here — lazily, only once a
/// cpv/slot match already happened (see `atom_satisfied`'s `&&`) and only
/// when `dep` actually has USE-dep brackets to check at all — not eagerly
/// for every installed package up front (see `AvailEntry::installed`'s doc
/// comment) — and memoized per entry afterward (see
/// `AvailEntry::interned_use`'s doc comment), so a package checked against
/// several USE-dep atoms in the same pass only pays the read+parse once.
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
            .into_iter()
            .map(|f| Interned::intern(f.trim_start_matches(['+', '-'])))
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

/// Like [`Avail::from_cpvs`], but keeps each installed package around so
/// [`Avail::atom_satisfied`] can verify USE-dep brackets against them (see
/// [`AvailEntry::installed`]). `None` = host `/var/db/pkg`.
fn vdb_avail_entries(root: Option<&Utf8Path>) -> Vec<AvailEntry> {
    let vdb = match root {
        Some(r) => Vdb::open(r.join("var/db/pkg")),
        None => Vdb::open_default(),
    };
    let Ok(vdb) = vdb else {
        return Vec::new();
    };
    avail_entries_from(vdb.packages().collect_vec())
}

fn avail_entries_from(pkgs: Vec<InstalledPackage>) -> Vec<AvailEntry> {
    pkgs.into_iter()
        .map(|p| AvailEntry {
            cpv: p.cpv().clone(),
            slot: p.slot_main().ok(),
            installed: Some(p),
            interned_use: OnceCell::new(),
        })
        .collect()
}

/// Raw installed-package rows for the BROOT-availability seed shared by
/// [`Avail::initial_bdepend`] and the solver's `host_installed` view
/// (`query::depgraph::installed::load_host_installed`) — both need exactly
/// the same root selection (the BDEPEND satisfaction root, plus the
/// prefix's own VDB under `--prefix`, see `initial_bdepend`'s doc comment),
/// only converting the resulting rows differently. Read once here; each
/// caller converts to its own entry type and keeps its own merge semantics
/// (union for `Avail`, last-wins insert for `add_host_installed`) — host
/// entries come first, then prefix, so both behaviours fall out of
/// iteration order.
pub fn broot_vdb_packages(roots: &Roots) -> Vec<InstalledPackage> {
    let mut out = vdb_packages_at(roots.satisfaction_root(DepClass::Bdepend));
    if roots.is_overlay() {
        out.extend(vdb_packages_at(roots.merge_root()));
    }
    out
}

fn vdb_packages_at(root: &Utf8Path) -> Vec<InstalledPackage> {
    let Ok(vdb) = Vdb::open(root.join("var/db/pkg")) else {
        return Vec::new();
    };
    vdb.packages().collect_vec()
}

/// The CPNs of every unsatisfied (non-blocker) atom in `entries`. An `AnyOf`
/// (`||`) group contributes its branch CPNs only when the whole group is
/// unsatisfied. `UseConditional`s are assumed already resolved by
/// `evaluate_use`. Used to find build-dep edges a root lacks (e.g. the native
/// offset host build-closure walk).
pub fn unsatisfied_cpns(entries: &[DepEntry], avail: &Avail) -> Vec<Cpn> {
    let mut out = Vec::new();
    unsat_cpns_rec(entries, avail, &mut out);
    out
}

fn unsat_cpns_rec(entries: &[DepEntry], avail: &Avail, out: &mut Vec<Cpn>) {
    for e in entries {
        match e {
            DepEntry::Atom(dep) if dep.blocker.is_none() && !avail.atom_satisfied(dep) => {
                out.push(dep.cpn);
            }
            DepEntry::AllOf(c) => unsat_cpns_rec(c, avail, out),
            DepEntry::AnyOf(c) if !group_satisfied(c, avail) => {
                for branch in c {
                    cpns_of(branch, out);
                }
            }
            _ => {}
        }
    }
}

/// Collect every non-blocker atom CPN mentioned in `e` (for an unsatisfied
/// `||`-group's branches).
fn cpns_of(e: &DepEntry, out: &mut Vec<Cpn>) {
    match e {
        DepEntry::Atom(dep) if dep.blocker.is_none() => out.push(dep.cpn),
        DepEntry::AllOf(c) | DepEntry::AnyOf(c) => c.iter().for_each(|b| cpns_of(b, out)),
        _ => {}
    }
}

/// Append the display form of each unsatisfied requirement in `entries` to
/// `out`. `UseConditional`s are assumed already resolved by `evaluate_use`.
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
            any @ DepEntry::AnyOf(children) => {
                if !group_satisfied(children, avail) {
                    out.push(any.to_string());
                }
            }
            DepEntry::ExactlyOneOf(_) | DepEntry::AtMostOneOf(_) => {}
            DepEntry::UseConditional { .. } => {}
        }
    }
}

fn group_satisfied(entries: &[DepEntry], avail: &Avail) -> bool {
    entries.iter().any(|e| entry_satisfied(e, avail))
}

/// Whether `e` is satisfied on `avail` (blockers count as satisfied).
fn entry_satisfied(e: &DepEntry, avail: &Avail) -> bool {
    match e {
        DepEntry::Atom(dep) => dep.blocker.is_some() || avail.atom_satisfied(dep),
        DepEntry::AllOf(c) => c.iter().all(|e| entry_satisfied(e, avail)),
        DepEntry::AnyOf(c) => group_satisfied(c, avail),
        DepEntry::ExactlyOneOf(_) | DepEntry::AtMostOneOf(_) => true,
        DepEntry::UseConditional { .. } => true,
    }
}

fn entry_unsatisfied_for_cpn(e: &DepEntry, cpn: Cpn, avail: &Avail) -> bool {
    match e {
        DepEntry::Atom(dep) if dep.blocker.is_some() => false,
        DepEntry::Atom(dep) if dep.cpn != cpn => false,
        DepEntry::Atom(dep) => !avail.atom_satisfied(dep),
        DepEntry::AllOf(c) => c.iter().any(|e| entry_unsatisfied_for_cpn(e, cpn, avail)),
        DepEntry::AnyOf(c) => {
            // Unsatisfied || group only if every branch that mentions `cpn` fails
            // and at least one branch mentions `cpn`.
            let mut mentions = false;
            let mut any_sat = false;
            for child in c {
                if branch_mentions_cpn(child, cpn) {
                    mentions = true;
                    if entry_satisfied(child, avail) {
                        any_sat = true;
                    }
                }
            }
            mentions && !any_sat
        }
        DepEntry::ExactlyOneOf(_) | DepEntry::AtMostOneOf(_) => false,
        DepEntry::UseConditional { .. } => false,
    }
}

fn branch_mentions_cpn(e: &DepEntry, cpn: Cpn) -> bool {
    match e {
        DepEntry::Atom(dep) => dep.cpn == cpn,
        DepEntry::AllOf(c) | DepEntry::AnyOf(c) => c.iter().any(|e| branch_mentions_cpn(e, cpn)),
        DepEntry::ExactlyOneOf(c) | DepEntry::AtMostOneOf(c) => {
            c.iter().any(|e| branch_mentions_cpn(e, cpn))
        }
        DepEntry::UseConditional { .. } => false,
    }
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

    /// Like [`atoms`], but the entry carries a real installed package with
    /// authoritative USE/IUSE, so USE-dep brackets get checked (mirrors
    /// [`vdb_avail_entries`]'s output). `AvailEntry::installed` reads
    /// `USE`/`IUSE` lazily from disk now (not a hand-buildable `UseInfo`
    /// pair), so this writes a real fake VDB entry and opens it — the
    /// tempdir is deliberately leaked (`into_path`) rather than dropped at
    /// the end of this function, since the returned `Avail` only reads its
    /// files on demand, when the caller's `atom_satisfied` runs.
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

        let root = Utf8Path::from_path(&tmp).unwrap();
        Avail(avail_entries_from(vdb_packages_at(root)))
    }

    fn parse(dep: &str) -> Vec<DepEntry> {
        DepEntry::parse(dep).unwrap()
    }

    /// Regression test for the `sys-apps/systemd-utils` stage3 failure: the
    /// host had `dev-python/jinja2` installed, but only built for
    /// `python_targets_python3_13`, not the `_14` this run actually needs.
    /// The old CPN/version/slot-only check treated the atom as satisfied
    /// regardless of the `[python_targets_python3_14(-)]` USE-dep bracket, so
    /// `em` never scheduled a jinja2 rebuild and the target package's
    /// `meson` configure failed with "python3 is missing modules: jinja2".
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

    /// Within-run solved-plan entries (no `use_info`) keep the old,
    /// USE-dep-blind behaviour — the solver's own `check_use_deps` already
    /// validated those, so `atom_satisfied` shouldn't re-check and risk a
    /// false negative without the parent-flag context that requires.
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

    /// Regression test for the riscv64 stage3 shakeout (#28/#30): the same
    /// bug class as `load_host_installed` (installed.rs) — `initial_bdepend`
    /// must read `host_roots`'s VDB, not unconditionally the bare host's.
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

    /// `--prefix`: a BDEPEND satisfied only by the prefix's own VDB (never
    /// built into the real host) must still count as satisfied — the weave
    /// this fixes, since `Cli::broot()` sends an unsatisfied one there.
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

    /// Regression test: `initial_depend` must weave in the *target's* own
    /// VDB for a bare `--root`, not just BROOT — a DEPEND provider already
    /// built into a partially populated `--root` from an earlier run must
    /// still count as satisfied even though the (real) host lacks it, or a
    /// resumed stage build hits a false preflight failure. Found reviewing
    /// the 2026-07-11 fix that made DEPEND resolve against BROOT for a
    /// native build (`satisfaction_root(DepClass::Depend)`) — that fix
    /// alone regressed this case by dropping the old `VDB(base) ∪
    /// VDB(target)` weave entirely.
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

    /// The same weave also still finds a BROOT-only entry — the target weave
    /// adds the offset's VDB, it doesn't replace BROOT's.
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

    /// The same weave also still finds a host-only entry — the overlay adds
    /// the prefix's VDB, it doesn't replace the host's.
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
