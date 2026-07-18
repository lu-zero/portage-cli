//! Detect whether an installed package must be rebuilt for USE / IUSE drift
//! (`--newuse` / `--changed-use`), matching Portage's `_reinstall_for_flags`.

use std::collections::HashSet;

use portage_atom::interner::{DefaultInterner, Interned};

/// How to compare installed USE against the planned configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseReinstallMode {
    /// `--newuse` / `-N`: IUSE set change **or** enabled-flag change (within
    /// IUSE), ignoring profile-forced flags.
    Newuse,
    /// `--changed-use` / `-U`: only enabled-flag change among flags that
    /// exist in both installed and current IUSE (ignore pure IUSE add/drop).
    ChangedUse,
}

/// Return `true` when Portage would reinstall for USE/IUSE drift.
///
/// Portage reference (`_emerge/depgraph.py` `_reinstall_for_flags`), with one
/// practical filter for multi-impl noise:
/// - **newuse**: IUSE set XOR (minus forced/masked, minus *stale never-enabled*
///   tokens only on the installed IUSE) ∪ enabled-set XOR within IUSE
/// - **changed-use**: enabled-set XOR only (`orig_iuse∩orig_use ⊕ cur_iuse∩cur_use`)
///
/// `forced_flags` is `use.force ∪ use.mask` for the package.
///
/// Stale IUSE-only tokens (e.g. VDB still lists `python_targets_python3_11`
/// though it was never enabled) are ignored so shallow `-Np` does not flood
/// the plan; real enabled-target flips and new IUSE flags still rebuild.
/// Full python-target cleanups belong on **`-uND`**, matching Portage.
pub fn needs_use_reinstall(
    mode: UseReinstallMode,
    forced_flags: &HashSet<Interned<DefaultInterner>>,
    orig_use: &[Interned<DefaultInterner>],
    orig_iuse: &[Interned<DefaultInterner>],
    cur_use_enabled: &HashSet<Interned<DefaultInterner>>,
    cur_iuse: &HashSet<Interned<DefaultInterner>>,
) -> bool {
    let orig_use: HashSet<_> = orig_use.iter().copied().collect();
    let orig_iuse: HashSet<_> = orig_iuse.iter().copied().collect();

    match mode {
        UseReinstallMode::Newuse => {
            // IUSE set change, minus forced/masked. Drop *stale never-enabled*
            // tokens that exist only on installed IUSE (common PYTHON_TARGETS:
            // VDB still lists python3_11 while the ebuild dropped them). New
            // IUSE flags and enabled-set flips still count.
            // Each f is in exactly one of orig_iuse / cur_iuse: keep if new in
            // the ebuild, or was enabled on the install; drop stale never-on
            // IUSE-only tokens.
            let mut flags: HashSet<_> = orig_iuse.symmetric_difference(cur_iuse).copied().collect();
            flags.retain(|f| {
                !forced_flags.contains(f) && (cur_iuse.contains(f) || orig_use.contains(f))
            });
            let old_en: HashSet<_> = orig_iuse.intersection(&orig_use).copied().collect();
            let new_en: HashSet<_> = cur_iuse.intersection(cur_use_enabled).copied().collect();
            flags.extend(old_en.symmetric_difference(&new_en).copied());
            !flags.is_empty()
        }
        UseReinstallMode::ChangedUse => {
            let old_en: HashSet<_> = orig_iuse.intersection(&orig_use).copied().collect();
            let new_en: HashSet<_> = cur_iuse.intersection(cur_use_enabled).copied().collect();
            old_en.symmetric_difference(&new_en).next().is_some()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(s: &str) -> Interned<DefaultInterner> {
        Interned::intern(s)
    }

    #[test]
    fn newuse_detects_enabled_flag_flip() {
        let forced = HashSet::new();
        let orig_use = vec![i("foo")];
        let orig_iuse = vec![i("foo"), i("bar")];
        let cur_iuse: HashSet<_> = [i("foo"), i("bar")].into_iter().collect();
        let cur_en: HashSet<_> = [i("bar")].into_iter().collect(); // foo off, bar on
        assert!(needs_use_reinstall(
            UseReinstallMode::Newuse,
            &forced,
            &orig_use,
            &orig_iuse,
            &cur_en,
            &cur_iuse,
        ));
    }

    #[test]
    fn newuse_detects_iuse_add() {
        let forced = HashSet::new();
        let orig_use = vec![i("foo")];
        let orig_iuse = vec![i("foo")];
        let cur_iuse: HashSet<_> = [i("foo"), i("newflag")].into_iter().collect();
        let cur_en: HashSet<_> = [i("foo")].into_iter().collect();
        assert!(needs_use_reinstall(
            UseReinstallMode::Newuse,
            &forced,
            &orig_use,
            &orig_iuse,
            &cur_en,
            &cur_iuse,
        ));
    }

    #[test]
    fn newuse_ignores_stale_never_enabled_iuse_only() {
        // VDB IUSE still has python3_11; ebuild dropped it; never was enabled.
        let forced = HashSet::new();
        let orig_use = vec![i("python_targets_python3_13")];
        let orig_iuse = vec![
            i("python_targets_python3_11"),
            i("python_targets_python3_13"),
            i("python_targets_python3_14"),
        ];
        let cur_iuse: HashSet<_> = [
            i("python_targets_python3_13"),
            i("python_targets_python3_14"),
        ]
        .into_iter()
        .collect();
        let cur_en: HashSet<_> = [
            i("python_targets_python3_13"),
            i("python_targets_python3_14"),
        ]
        .into_iter()
        .collect();
        // Enabled set also grew (3.14 on) → still reinstall.
        assert!(needs_use_reinstall(
            UseReinstallMode::Newuse,
            &forced,
            &orig_use,
            &orig_iuse,
            &cur_en,
            &cur_iuse,
        ));
        // Same enabled set: only stale IUSE token → no reinstall.
        let cur_en2: HashSet<_> = [i("python_targets_python3_13")].into_iter().collect();
        assert!(!needs_use_reinstall(
            UseReinstallMode::Newuse,
            &forced,
            &orig_use,
            &orig_iuse,
            &cur_en2,
            &cur_iuse,
        ));
    }

    #[test]
    fn changed_use_ignores_pure_iuse_add() {
        let forced = HashSet::new();
        let orig_use = vec![i("foo")];
        let orig_iuse = vec![i("foo")];
        let cur_iuse: HashSet<_> = [i("foo"), i("newflag")].into_iter().collect();
        let cur_en: HashSet<_> = [i("foo")].into_iter().collect();
        assert!(!needs_use_reinstall(
            UseReinstallMode::ChangedUse,
            &forced,
            &orig_use,
            &orig_iuse,
            &cur_en,
            &cur_iuse,
        ));
    }

    #[test]
    fn newuse_strips_forced_from_iuse_delta_only() {
        // Portage: IUSE-set delta subtracts forced; enabled-set XOR does not.
        // Pure IUSE add of a forced flag alone → no reinstall if enabled sets match
        // after stripping that flag from the IUSE XOR... actually enabled XOR
        // still sees forced flip. So only when forced stays in both enabled sets
        // (or neither) and IUSE delta is only forced is it a no-op:
        let forced: HashSet<_> = [i("forced")].into_iter().collect();
        let orig_use = vec![i("foo"), i("forced")];
        let orig_iuse = vec![i("foo"), i("forced")];
        let cur_iuse: HashSet<_> = [i("foo"), i("forced")].into_iter().collect();
        let cur_en: HashSet<_> = [i("foo"), i("forced")].into_iter().collect();
        assert!(!needs_use_reinstall(
            UseReinstallMode::Newuse,
            &forced,
            &orig_use,
            &orig_iuse,
            &cur_en,
            &cur_iuse,
        ));
    }

    #[test]
    fn unchanged_is_false() {
        let forced = HashSet::new();
        let orig_use = vec![i("foo")];
        let orig_iuse = vec![i("foo"), i("bar")];
        let cur_iuse: HashSet<_> = [i("foo"), i("bar")].into_iter().collect();
        let cur_en: HashSet<_> = [i("foo")].into_iter().collect();
        assert!(!needs_use_reinstall(
            UseReinstallMode::Newuse,
            &forced,
            &orig_use,
            &orig_iuse,
            &cur_en,
            &cur_iuse,
        ));
    }
}
