//! Portage set resolution: expand `@name` references into concrete atoms.
//!
//! Sets are *not* PMS-defined — they are a portage-config concept (`man emerge`,
//! §SETS). A set is a named collection of atoms and (recursively) other `@set`
//! references. This module resolves a set name to its flat atom list, mirroring
//! portage's `SetConfig.getSetAtoms`:
//!
//! - [`SetResolver::system`] and [`SetResolver::profile`] read the profile
//!   `packages` file (PMS 5.2.6): `*cat/pkg` lines are `@system`, all lines are
//!   `@profile`.
//! - [`SetResolver::selected`] reads `/var/lib/portage/world` +
//!   `world_sets` (the `@set` refs the user asked to track).
//! - [`SetResolver::world`] is always `@selected ∪ @system`.
//! - User sets live in `etc/portage/sets/<name>` (one atom/`@ref` per line) and
//!   `etc/portage/sets.conf` (ini `[name]` sections, portage's `StaticFileSet`/
//!   `ConfigFileSet` builders).
//!
//! Recursive expansion uses [`super::named_groups::expand_group`] (cycle-safe).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Dep;

use crate::error::{Error, Result};
use crate::repo::ProfileStack;
use crate::repo::ini;
use crate::repo::named_groups::{self, GroupEntry, classify_token, expand_group};

/// The `@` prefix that marks a set reference (portage `SETPREFIX`).
pub(crate) const SET_PREFIX: char = named_groups::GROUP_PREFIX;

/// Resolve portage set references to flat atom lists.
pub struct SetResolver<'a> {
    profile_stack: &'a ProfileStack,
    /// `EROOT` — the root holding `var/lib/portage/world[_sets]` and
    /// `etc/portage/sets[.conf]`. Usually the install target.
    eroot: &'a Utf8Path,
}

impl<'a> SetResolver<'a> {
    /// Build a resolver over `profile_stack` (for `@system`/`@profile`) and
    /// `eroot` (for `@world`, `@selected`, and user-defined sets).
    pub fn new(profile_stack: &'a ProfileStack, eroot: &'a Utf8Path) -> Self {
        Self {
            profile_stack,
            eroot,
        }
    }

    /// Resolve any `@name` to its flat atom list.
    pub fn resolve(&self, name: &str) -> Result<Vec<Dep>> {
        let mut visited = HashSet::new();
        self.resolve_named(name, &mut visited)
    }

    fn resolve_named(&self, name: &str, visited: &mut HashSet<String>) -> Result<Vec<Dep>> {
        let mut lookup = |n: &str| self.direct_members(n);
        expand_group(name, visited, &mut lookup)
    }

    fn direct_members(&self, name: &str) -> Result<Vec<GroupEntry<Dep>>> {
        match name {
            "system" => Ok(self
                .profile_stack
                .system_set()?
                .into_iter()
                .map(GroupEntry::Leaf)
                .collect()),
            "profile" => Ok(self
                .profile_stack
                .packages()?
                .into_iter()
                .map(|(_, d)| GroupEntry::Leaf(d))
                .collect()),
            "selected" => self.selected_members(),
            "world" => {
                let mut out = self.selected_members()?;
                out.extend(
                    self.profile_stack
                        .system_set()?
                        .into_iter()
                        .map(GroupEntry::Leaf),
                );
                Ok(out)
            }
            other => self.user_set_members(other),
        }
    }

    fn selected_members(&self) -> Result<Vec<GroupEntry<Dep>>> {
        let mut out: Vec<GroupEntry<Dep>> = read_atoms(&self.eroot.join("var/lib/portage/world"))?
            .into_iter()
            .map(GroupEntry::Leaf)
            .collect();
        for set_ref in read_lines(&self.eroot.join("var/lib/portage/world_sets"))? {
            let name = set_ref
                .trim()
                .strip_prefix(SET_PREFIX)
                .unwrap_or(&set_ref)
                .trim();
            if name.is_empty() {
                continue;
            }
            out.push(GroupEntry::Ref(name.to_string()));
        }
        Ok(out)
    }

    fn user_set_members(&self, name: &str) -> Result<Vec<GroupEntry<Dep>>> {
        let file = self.eroot.join("etc/portage/sets").join(name);
        if file.exists() {
            return self.parse_set_file(&file);
        }
        if let Some(filename) = lookup_sets_conf(&self.eroot.join("etc/portage/sets.conf"), name)? {
            return self.parse_set_file(
                Utf8Path::from_path(&filename).ok_or_else(|| {
                    Error::InvalidProfile(format!("non-utf8 set path for @{name}"))
                })?,
            );
        }
        Err(Error::InvalidProfile(format!("unknown set @{name}")))
    }

    fn parse_set_file(&self, path: &Utf8Path) -> Result<Vec<GroupEntry<Dep>>> {
        let mut out = Vec::new();
        for line in read_lines(path)? {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if trimmed
                .strip_prefix(SET_PREFIX)
                .is_some_and(|r| r.is_empty())
            {
                return Err(Error::InvalidProfile(
                    "bare '@' is not a valid set ref".to_string(),
                ));
            }
            out.push(classify_token(trimmed, parse_dep)?);
        }
        Ok(out)
    }
}

/// True iff `s` is a set reference (starts with `@`).
pub fn is_set_ref(s: &str) -> bool {
    named_groups::is_group_ref(s)
}

/// Strip the leading `@` from a set reference, or `None` if `s` is not one.
pub fn set_name(s: &str) -> Option<&str> {
    named_groups::group_ref_name(s)
}

/// Whether a `@name` set, typed directly on the command line, should itself
/// be recorded to `world_sets` (real emerge's `world-candidate` set-config
/// flag). Real portage's default `sets.conf` (`cnf/sets/portage.conf`) sets
/// `world-candidate = True` only on `[usersets]` — every file under
/// `etc/portage/sets/` (or a `sets.conf` `[name]` section); every *named*
/// built-in set defaults to `world_candidate = False` and is never recorded
/// no matter how it's invoked. The exclusion list below is every section
/// name in `cnf/sets/portage.conf` (not just the ones `em` currently
/// implements — `@security`/`@live-rebuild`/etc. are real portage set names
/// today even though `SetResolver` can't resolve them yet, and must stay
/// excluded once they are implemented too, the same as the ones already
/// live here).
pub fn is_world_candidate(name: &str) -> bool {
    !matches!(
        name,
        "world"
            | "profile"
            | "selected"
            | "selected-packages"
            | "selected-sets"
            | "system"
            | "security"
            | "installed"
            | "preserved-rebuild"
            | "live-rebuild"
            | "deprecated-live-rebuild"
            | "module-rebuild"
            | "x11-module-rebuild"
            | "rebuilt-binaries"
            | "changed-subslot"
            | "downgrade"
            | "unavailable"
            | "unavailable-binaries"
            | "changed-deps"
            | "golang-rebuild"
            | "rust-rebuild"
    )
}

fn parse_dep(s: &str) -> Result<Dep> {
    Dep::parse(s).map_err(Error::from)
}

fn read_atoms(path: &Utf8Path) -> Result<Vec<Dep>> {
    let mut out = Vec::new();
    for line in read_lines(path)? {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        out.push(Dep::parse(trimmed)?);
    }
    Ok(out)
}

fn read_lines(path: &Utf8Path) -> Result<Vec<String>> {
    match std::fs::read_to_string(path.as_std_path()) {
        Ok(s) => Ok(s.lines().map(|l| l.trim().to_string()).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(Error::Io {
            path: path.as_std_path().to_path_buf(),
            source: e,
        }),
    }
}

fn lookup_sets_conf(path: &Utf8Path, name: &str) -> Result<Option<PathBuf>> {
    let Ok(content) = std::fs::read_to_string(path.as_std_path()) else {
        return Ok(None);
    };
    let mut sections = HashMap::new();
    let mut order = Vec::new();
    ini::merge_sections(&mut sections, &mut order, &content);
    let Some(section) = sections.get(name) else {
        return Ok(None);
    };
    let find = |key: &str| {
        section
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    };
    Ok(finish_static_file(
        find("class").unwrap_or_default(),
        find("filename"),
        path,
    ))
}

fn finish_static_file(
    class: &str,
    filename: Option<&str>,
    conf_path: &Utf8Path,
) -> Option<PathBuf> {
    let is_static = class
        .rsplit('.')
        .next()
        .is_some_and(|c| c.eq_ignore_ascii_case("StaticFileSet"));
    if !is_static {
        return None;
    }
    let f = filename?;
    let p = Utf8PathBuf::from(f);
    let resolved = if p.is_absolute() {
        p.into_std_path_buf()
    } else {
        conf_path
            .parent()
            .unwrap_or(Utf8Path::new("."))
            .join(&p)
            .into_std_path_buf()
    };
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn make_profile_stack(dir: &Path) -> ProfileStack {
        ProfileStack::build(dir.to_path_buf()).unwrap()
    }

    #[test]
    fn classify_atom_vs_set() {
        let atom = classify_token("dev-libs/openssl", parse_dep).unwrap();
        assert!(matches!(atom, GroupEntry::Leaf(_)));
        let set = classify_token("@system", parse_dep).unwrap();
        assert!(matches!(set, GroupEntry::Ref(_)));
        assert!(classify_token("@", parse_dep).is_err());
    }

    #[test]
    fn world_candidate_excludes_every_named_builtin() {
        for name in [
            "world",
            "selected",
            "system",
            "profile",
            "preserved-rebuild",
            "selected-packages",
            "security",
            "live-rebuild",
        ] {
            assert!(!is_world_candidate(name), "@{name} must not be a candidate");
        }
        for name in ["mytest", "openwrt-prerequisites"] {
            assert!(is_world_candidate(name), "@{name} should be a candidate");
        }
    }

    #[test]
    fn is_set_ref_and_name() {
        assert!(is_set_ref("@world"));
        assert!(!is_set_ref("dev-libs/openssl"));
        assert_eq!(set_name("@system"), Some("system"));
        assert_eq!(set_name("@"), None);
    }

    #[test]
    fn user_set_file_with_nested_refs() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("etc/portage/sets/base"),
            "sys-libs/zlib\n@extra\n",
        );
        write(&root.join("etc/portage/sets/extra"), "dev-libs/openssl\n");
        let stack = make_profile_stack(root);
        let eroot = Utf8Path::from_path(root).unwrap();
        let r = SetResolver::new(&stack, eroot);
        let atoms = r.resolve("base").unwrap();
        let names: Vec<String> = atoms.iter().map(|d| d.to_string()).collect();
        assert!(names.contains(&"sys-libs/zlib".to_string()));
        assert!(names.contains(&"dev-libs/openssl".to_string()));
    }

    #[test]
    fn set_cycle_is_broken_not_errored() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("etc/portage/sets/a"), "sys-libs/zlib\n@b\n");
        write(&root.join("etc/portage/sets/b"), "dev-libs/openssl\n@a\n");
        let stack = make_profile_stack(root);
        let eroot = Utf8Path::from_path(root).unwrap();
        let r = SetResolver::new(&stack, eroot);
        let atoms = r.resolve("a").unwrap();
        assert_eq!(atoms.len(), 2);
    }

    #[test]
    fn unknown_set_is_an_error() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let stack = make_profile_stack(root);
        let eroot = Utf8Path::from_path(root).unwrap();
        let r = SetResolver::new(&stack, eroot);
        assert!(r.resolve("does-not-exist").is_err());
    }
}
