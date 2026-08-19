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
    let mut roots = vec![root.clone()];
    // In-place `--local` (EPREFIX set): the run installs every package — build
    // tools and libraries alike — into the prefix EROOT, which is layered on
    // the host. So `-b`/`-d`/`-r` queries must also see the prefix, e.g.
    // python-any-r1's `has_version -b xcb-proto` where xcb-proto was just built
    // into the prefix, not the host. (Non-prefix builds have EROOT == ROOT, so
    // this adds nothing.)
    if get("EPREFIX").is_some()
        && let Some(eroot) = get("EROOT")
        && eroot != root
    {
        roots.push(eroot);
    }
    roots
        .into_iter()
        .map(|r| std::path::Path::new(&r).join("var/db/pkg"))
        .collect()
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
        let vdbs = vdb_roots_for(context.shell, self.broot, self.sysroot);
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
        let vdbs = vdb_roots_for(context.shell, self.broot, self.sysroot);
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
