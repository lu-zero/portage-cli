//! Generic `@name` group expansion with portage cycle-breaking semantics
//!
//! Shared by atom sets ([`super::sets`]) and license groups
//! ([`super::license_groups`]).

use std::collections::HashSet;

use crate::error::{Error, Result};

/// The `@` prefix that marks a nested group reference (portage `SETPREFIX`)
pub const GROUP_PREFIX: char = '@';

/// One member of a named group: a leaf value or a reference to another group
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupEntry<M> {
    /// A concrete member (atom, license id, …)
    Leaf(M),
    /// Nested group name **without** the leading `@`
    Ref(String),
}

/// True iff `s` is a group reference (starts with `@` and has a name)
pub fn is_group_ref(s: &str) -> bool {
    group_ref_name(s).is_some()
}

/// Strip the leading `@` from a group reference, or `None` if `s` is not one
pub fn group_ref_name(s: &str) -> Option<&str> {
    s.strip_prefix(GROUP_PREFIX).filter(|n| !n.is_empty())
}

/// Expand a named group to a flat leaf list
///
/// `lookup` returns the direct members of `name`; [`GroupEntry::Ref`] members
/// are expanded recursively. Revisiting a name on the expansion stack yields an
/// empty contribution (portage `ignorelist` / set-cycle semantics).
pub fn expand_group<M, F>(
    name: &str,
    visited: &mut HashSet<String>,
    lookup: &mut F,
) -> Result<Vec<M>>
where
    M: Clone,
    F: FnMut(&str) -> Result<Vec<GroupEntry<M>>>,
{
    if name.is_empty() {
        return Err(Error::InvalidProfile("empty group name".to_string()));
    }
    if !visited.insert(name.to_string()) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in lookup(name)? {
        match entry {
            GroupEntry::Leaf(m) => out.push(m),
            GroupEntry::Ref(r) => out.extend(expand_group(&r, visited, lookup)?),
        }
    }
    Ok(out)
}

/// Classify a whitespace token as leaf or `@ref` using `parse_leaf`
pub fn classify_token<M, F>(token: &str, parse_leaf: F) -> Result<GroupEntry<M>>
where
    F: FnOnce(&str) -> Result<M>,
{
    if let Some(name) = group_ref_name(token) {
        Ok(GroupEntry::Ref(name.to_string()))
    } else {
        parse_leaf(token).map(GroupEntry::Leaf)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn group_ref_name_rejects_bare_at() {
        assert_eq!(group_ref_name("@FREE"), Some("FREE"));
        assert_eq!(group_ref_name("@"), None);
        assert_eq!(group_ref_name("MIT"), None);
    }

    #[test]
    fn expand_group_flattens_nested_refs() {
        let groups: HashMap<&str, Vec<GroupEntry<&str>>> = HashMap::from([
            (
                "A",
                vec![GroupEntry::Leaf("x"), GroupEntry::Ref("B".into())],
            ),
            ("B", vec![GroupEntry::Leaf("y")]),
        ]);
        let mut visited = HashSet::new();
        let mut lookup = |n: &str| Ok(groups.get(n).cloned().unwrap_or_default());
        let out = expand_group("A", &mut visited, &mut lookup).unwrap();
        assert_eq!(out, vec!["x", "y"]);
    }

    #[test]
    fn expand_group_breaks_cycles() {
        let groups: HashMap<&str, Vec<GroupEntry<&str>>> = HashMap::from([
            (
                "a",
                vec![GroupEntry::Leaf("z"), GroupEntry::Ref("b".into())],
            ),
            ("b", vec![GroupEntry::Ref("a".into())]),
        ]);
        let mut visited = HashSet::new();
        let mut lookup = |n: &str| Ok(groups.get(n).cloned().unwrap_or_default());
        let out = expand_group("a", &mut visited, &mut lookup).unwrap();
        assert_eq!(out, vec!["z"]);
    }
}
