//! `has_version` / `best_version` builtins (PMS 12.3.13 / 12.3.4)
//!
//! Query the installed-package database for an atom. `-r` (default) queries
//! `ROOT`'s VDB, `-d` `ESYSROOT`'s, `-b` `BROOT`'s — under a `--prefix` run
//! these differ: build-time tools (`-b`, e.g. autotools.eclass probing the
//! installed autoconf) live on the host, while runtime deps live in the
//! prefix.

use std::collections::HashSet;
use std::io::Write;

use brush_core::builtins;
use clap::Parser;
use portage_atom::interner::{DefaultInterner, Interned};

fn vdb_roots_for<SE: brush_core::ShellExtensions>(
    shell: &brush_core::Shell<SE>,
    broot: bool,
    sysroot: bool,
    atom: &str,
) -> Vec<std::path::PathBuf> {
    let get = |var: &str| {
        shell
            .env_str(var)
            .map(|s| s.into_owned())
            .filter(|s| !s.is_empty())
    };
    let var = if broot {
        "BROOT"
    } else if sysroot {
        "ESYSROOT"
    } else {
        "ROOT"
    };
    let root = get(var).unwrap_or_else(|| "/".to_string());
    let eroot = get("EPREFIX")
        .and_then(|_| get("EROOT"))
        .filter(|eroot| *eroot != root);
    select_vdb_roots(root, eroot, atom)
        .into_iter()
        .map(|r| std::path::Path::new(&r).join("var/db/pkg"))
        .collect()
}

/// Which roots an atom may be satisfied from, given the query root and the
/// prefix's `EROOT` (when there is a distinct one).
///
/// Ordinarily both: an in-place `--local`/`--prefix` run installs build tools
/// and libraries alike into the prefix, which is layered on the host, so a
/// query has to see either — e.g. python-any-r1's `has_version -b xcb-proto`
/// where xcb-proto was just built into the prefix rather than the host.
///
/// A `cross-<tuple>/*` atom is the exception, and the reason this is not just
/// `roots.push(eroot)`. Piggybacking off the host is right for a build *tool*,
/// whose consumer only execs it and never cares where it lives. A `cross-*`
/// atom's consumer does care: `toolchain.eclass` answers
/// `has_version ${CATEGORY}/${needed_libc}` by hardcoding
/// `--with-sysroot=${PREFIX}/${CTARGET}` in every non-freestanding branch. A
/// host-side match therefore reports "libc present" while pointing the build
/// at a prefix sysroot that has none — gcc-stage1 then fails on a missing
/// `stdio.h` instead of correctly configuring `--without-headers`. That holds
/// in upstream Gentoo Prefix, where ROOT and EPREFIX coincide, and breaks in
/// `em`'s overlay model, where they are deliberately separate trees.
fn select_vdb_roots(root: String, eroot: Option<String>, atom: &str) -> Vec<String> {
    let Some(eroot) = eroot else {
        return vec![root];
    };
    if is_cross_atom(atom) {
        return vec![eroot];
    }
    vec![root, eroot]
}

/// Whether `atom` names a crossdev-aliased package (`cross-<tuple>/<pn>`)
///
/// Unparseable text is treated as ordinary, leaving the existing behaviour for
/// anything this does not understand.
fn is_cross_atom(atom: &str) -> bool {
    portage_atom::Dep::parse(atom).is_ok_and(|dep| dep.cpn.category.as_str().starts_with("cross-"))
}

/// Best installed cpv matching `atom` across any of `vdb_paths`
fn best_match_any(
    vdb_paths: &[std::path::PathBuf],
    atom: &str,
    parent_use: &std::collections::HashSet<String>,
) -> Option<portage_atom::Cpv> {
    vdb_paths
        .iter()
        .filter_map(|p| best_match(p, atom, parent_use))
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// Best installed cpv matching `atom` in the VDB at `vdb_path`, if any
fn best_match(
    vdb_path: &std::path::Path,
    atom: &str,
    parent_use: &std::collections::HashSet<String>,
) -> Option<portage_atom::Cpv> {
    let dep = portage_atom::Dep::parse(atom).ok()?;
    let vdb_path = camino::Utf8Path::from_path(vdb_path)?;
    let vdb = portage_vdb::Vdb::open(vdb_path).ok()?;
    let cat = vdb.category(dep.cpn.category.as_str())?;
    let mut best: Option<portage_atom::Cpv> = None;
    for pkg in cat.packages() {
        let cpv = pkg.cpv();
        if cpv.cpn != dep.cpn {
            continue;
        }
        let slot = pkg.slot().ok();
        if !dep.matches_cpv(cpv, slot.as_ref()) {
            continue;
        }
        // The atom's USE-dependency (`[headers-only(-)]`, `[ssl,-debug]`, …) must
        // match the *installed* package's recorded USE, or e.g. toolchain.eclass's
        // `has_version glibc[headers-only(-)]` matches a full glibc as if it were
        // headers-only and builds gcc `--disable-shared`. matches_cpv only checks
        // cpn/version/slot, so evaluate the USE constraints here against the VDB.
        if let Some(use_deps) = &dep.use_deps
            && !use_deps.is_empty()
        {
            let installed_use: HashSet<Interned<DefaultInterner>> =
                pkg.use_flags().unwrap_or_default().into_iter().collect();
            // `IUse` already holds the name interned; `From` reuses that key
            // rather than resolving it to a `&str` and interning it again.
            let installed_iuse: HashSet<Interned<DefaultInterner>> = pkg
                .iuse()
                .unwrap_or_default()
                .iter()
                .map(Interned::from)
                .collect();
            if !use_deps_satisfied(use_deps, &installed_use, &installed_iuse, parent_use) {
                continue;
            }
        }
        if best.as_ref().is_none_or(|b| cpv.version > b.version) {
            best = Some(cpv.clone());
        }
    }
    best
}

/// Whether every USE-dependency in `use_deps` holds for an installed
/// package (PMS 8.3.4)
///
/// Checked against the given active `installed_use` / declared
/// `installed_iuse`, relative to the querying package's `parent_use`. A
/// flag absent from IUSE resolves through its `(+)`/`(-)` default; absent
/// and undefaulted means the constraint cannot be satisfied.
fn use_deps_satisfied(
    use_deps: &[portage_atom::UseDep],
    installed_use: &HashSet<Interned<DefaultInterner>>,
    installed_iuse: &HashSet<Interned<DefaultInterner>>,
    parent_use: &std::collections::HashSet<String>,
) -> bool {
    use portage_atom::{UseDefault, UseDepKind};
    use_deps.iter().all(|ud| {
        let flag = ud.flag.as_str();
        // Interned both sides: comparing keys, not hashing flag text.
        let state = if installed_iuse.contains(&ud.flag) {
            Some(installed_use.contains(&ud.flag))
        } else {
            match ud.default {
                Some(UseDefault::Enabled) => Some(true),
                Some(UseDefault::Disabled) => Some(false),
                None => None,
            }
        };
        let parent = parent_use.contains(flag);
        match ud.kind {
            UseDepKind::Enabled => state == Some(true),
            UseDepKind::Disabled => state == Some(false),
            UseDepKind::Conditional => !parent || state == Some(true),
            UseDepKind::ConditionalInverse => parent || state == Some(true),
            UseDepKind::Equal => state == Some(parent),
            UseDepKind::EqualInverse => state == Some(!parent),
        }
    })
}

/// The querying package's active USE flags (the "parent" for conditional USE
/// deps), from the build shell's `USE`.
fn parent_use<SE: brush_core::ShellExtensions>(
    shell: &brush_core::Shell<SE>,
) -> std::collections::HashSet<String> {
    shell
        .env_str("USE")
        .map(|s| s.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// `has_version [-b|-d|-r] <atom>` — exit 0 when an installed package
/// matches.
#[derive(Parser)]
pub(crate) struct HasVersionCommand {
    /// Query BROOT (build tools)
    #[arg(short = 'b')]
    broot: bool,
    /// Query ESYSROOT
    #[arg(short = 'd')]
    sysroot: bool,
    /// Query ROOT (the default)
    #[arg(short = 'r')]
    root: bool,
    atom: String,
}

impl builtins::Command for HasVersionCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let vdbs = vdb_roots_for(context.shell, self.broot, self.sysroot, &self.atom);
        let found = best_match_any(&vdbs, &self.atom, &parent_use(context.shell)).is_some();
        Ok(brush_core::ExecutionResult::new(u8::from(!found)))
    }
}

/// `best_version [-b|-d|-r] <atom>` — print the best matching installed cpv
#[derive(Parser)]
pub(crate) struct BestVersionCommand {
    /// Query BROOT (build tools)
    #[arg(short = 'b')]
    broot: bool,
    /// Query ESYSROOT
    #[arg(short = 'd')]
    sysroot: bool,
    /// Query ROOT (the default)
    #[arg(short = 'r')]
    root: bool,
    atom: String,
}

impl builtins::Command for BestVersionCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let vdbs = vdb_roots_for(context.shell, self.broot, self.sysroot, &self.atom);
        match best_match_any(&vdbs, &self.atom, &parent_use(context.shell)) {
            Some(cpv) => {
                let shell = context.shell;
                let _ = writeln!(context.params.stdout(shell), "{cpv}");
                Ok(brush_core::ExecutionResult::new(0))
            }
            None => Ok(brush_core::ExecutionResult::new(1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::use_deps_satisfied;
    use portage_atom::UseDep;
    use std::collections::HashSet;

    use portage_atom::interner::{DefaultInterner, Interned};

    /// Installed USE/IUSE sets, interned like the real accessors return them
    fn set(flags: &[&str]) -> HashSet<Interned<DefaultInterner>> {
        flags.iter().copied().map(Interned::intern).collect()
    }

    /// The parent's USE, which comes from the build shell's environment as
    /// plain text rather than from the VDB.
    fn parent_set(flags: &[&str]) -> HashSet<String> {
        flags.iter().map(|s| (*s).to_string()).collect()
    }

    fn deps(atom_use: &str) -> Vec<UseDep> {
        // Parse the `[...]` body of an atom into UseDeps.
        let dep = portage_atom::Dep::parse(&format!("cat/pkg[{atom_use}]")).unwrap();
        dep.use_deps.unwrap()
    }

    // A `cross-*` atom must not be satisfiable from the host: its only
    // consumer, toolchain.eclass, hardcodes an EPREFIX-relative
    // `--with-sysroot`, so a host-side match points the build at a sysroot
    // that does not contain what was matched.
    #[test]
    fn a_cross_atom_is_answered_from_the_prefix_alone() {
        assert_eq!(
            super::select_vdb_roots(
                "/".to_string(),
                Some("/pfx".to_string()),
                "cross-riscv64-unknown-linux-gnu/glibc",
            ),
            vec!["/pfx".to_string()],
        );
    }

    // The host piggyback stays for everything else — that is what lets a build
    // tool installed on the host satisfy a `-b` query under `--prefix`.
    #[test]
    fn an_ordinary_atom_still_sees_both_roots() {
        assert_eq!(
            super::select_vdb_roots("/".to_string(), Some("/pfx".to_string()), "sys-libs/glibc"),
            vec!["/".to_string(), "/pfx".to_string()],
        );
    }

    // No prefix (a bare build, EROOT == ROOT) is unchanged for either shape.
    #[test]
    fn without_a_prefix_both_atom_shapes_query_the_one_root() {
        for atom in ["sys-libs/glibc", "cross-riscv64-unknown-linux-gnu/glibc"] {
            assert_eq!(
                super::select_vdb_roots("/".to_string(), None, atom),
                vec!["/".to_string()],
                "{atom}"
            );
        }
    }

    // Versioned and ranged forms reach the same category, and text this cannot
    // parse keeps the old two-root behaviour rather than silently narrowing.
    #[test]
    fn cross_detection_reads_the_category_not_the_raw_text() {
        assert!(super::is_cross_atom(">=cross-i586-pc-linux-gnu/glibc-2.41"));
        assert!(super::is_cross_atom(
            "cross-riscv64-unknown-linux-gnu/glibc[headers-only(-)]"
        ));
        assert!(!super::is_cross_atom("sys-devel/crossdev"));
        assert!(!super::is_cross_atom("not a valid atom"));
    }

    #[test]
    fn headers_only_default_disabled_matches_installed_state() {
        let iuse = set(&["headers-only", "multilib", "ssp"]);
        let parent = parent_set(&[]);
        // Full glibc: headers-only OFF → `[headers-only(-)]` must NOT match.
        let full = set(&["multilib", "ssp"]);
        assert!(!use_deps_satisfied(
            &deps("headers-only(-)"),
            &full,
            &iuse,
            &parent
        ));
        // Headers-only glibc: headers-only ON → matches.
        let hdrs = set(&["headers-only", "ssp"]);
        assert!(use_deps_satisfied(
            &deps("headers-only(-)"),
            &hdrs,
            &iuse,
            &parent
        ));
    }

    #[test]
    fn enabled_and_disabled_kinds() {
        let iuse = set(&["ssl", "debug"]);
        let parent = parent_set(&[]);
        let installed = set(&["ssl"]);
        assert!(use_deps_satisfied(&deps("ssl"), &installed, &iuse, &parent));
        assert!(use_deps_satisfied(
            &deps("-debug"),
            &installed,
            &iuse,
            &parent
        ));
        assert!(use_deps_satisfied(
            &deps("ssl,-debug"),
            &installed,
            &iuse,
            &parent
        ));
        assert!(!use_deps_satisfied(
            &deps("debug"),
            &installed,
            &iuse,
            &parent
        ));
        assert!(!use_deps_satisfied(
            &deps("-ssl"),
            &installed,
            &iuse,
            &parent
        ));
    }

    #[test]
    fn missing_flag_uses_default_else_unsatisfiable() {
        let iuse = set(&["other"]);
        let parent = parent_set(&[]);
        let installed = set(&[]);
        // Flag absent from IUSE: (+) → enabled, (-) → disabled.
        assert!(use_deps_satisfied(
            &deps("foo(+)"),
            &installed,
            &iuse,
            &parent
        ));
        assert!(!use_deps_satisfied(
            &deps("foo(-)"),
            &installed,
            &iuse,
            &parent
        ));
        // Absent and undefaulted → cannot be satisfied (neither enabled nor disabled).
        assert!(!use_deps_satisfied(
            &deps("foo"),
            &installed,
            &iuse,
            &parent
        ));
        assert!(!use_deps_satisfied(
            &deps("-foo"),
            &installed,
            &iuse,
            &parent
        ));
    }

    #[test]
    fn conditional_and_equal_relative_to_parent() {
        let iuse = set(&["x"]);
        let on = set(&["x"]);
        let off = set(&[]);
        // [x?]: only constrains when parent has x.
        assert!(use_deps_satisfied(
            &deps("x?"),
            &on,
            &iuse,
            &parent_set(&["x"])
        ));
        assert!(!use_deps_satisfied(
            &deps("x?"),
            &off,
            &iuse,
            &parent_set(&["x"])
        ));
        assert!(use_deps_satisfied(
            &deps("x?"),
            &off,
            &iuse,
            &parent_set(&[])
        ));
        // [x=]: dep flag must equal parent flag.
        assert!(use_deps_satisfied(
            &deps("x="),
            &on,
            &iuse,
            &parent_set(&["x"])
        ));
        assert!(use_deps_satisfied(
            &deps("x="),
            &off,
            &iuse,
            &parent_set(&[])
        ));
        assert!(!use_deps_satisfied(
            &deps("x="),
            &on,
            &iuse,
            &parent_set(&[])
        ));
    }
}
