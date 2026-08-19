//! License group definitions from `profiles/license_groups` and
//! [`AcceptSet`] token expansion for `ACCEPT_LICENSE`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use portage_metadata::interner::{DefaultInterner, Interned};
use portage_metadata::{LicenseExpr, RestrictExpr};

use crate::error::{Error, Result};
use crate::repo::named_groups::{GroupEntry, expand_group, group_ref_name};
use crate::repo::repository::Repository;

/// Parsed `profiles/license_groups`: group name → direct members
#[derive(Debug, Clone, Default)]
pub struct LicenseGroupRegistry {
    groups: HashMap<String, Vec<GroupEntry<Interned<DefaultInterner>>>>,
}

impl LicenseGroupRegistry {
    /// Load group definitions from `repo/profiles/license_groups`
    pub fn from_repo(repo: &Repository) -> Result<Self> {
        let path = repo.path().join("profiles/license_groups");
        Self::from_file(path.as_std_path())
    }

    /// Parse a `license_groups` file (tolerates absence → empty registry)
    pub fn from_file(path: &Path) -> Result<Self> {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Ok(Self::default());
        };
        Ok(Self::parse(&content))
    }

    /// Parse `profiles/license_groups` content
    pub fn parse(content: &str) -> Self {
        let mut groups: HashMap<String, Vec<GroupEntry<Interned<DefaultInterner>>>> =
            HashMap::new();
        for line in content.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut tokens = line.split_whitespace();
            let Some(name) = tokens.next() else {
                continue;
            };
            let members: Vec<_> = tokens
                .filter_map(|t| classify_license_token(t).ok())
                .collect();
            if !members.is_empty() {
                groups.insert(name.to_string(), members);
            }
        }
        Self { groups }
    }

    /// Expand a group name to interned license identifiers (cycle-safe)
    pub fn expand(&self, name: &str) -> Vec<Interned<DefaultInterner>> {
        let mut visited = HashSet::new();
        let mut lookup = |n: &str| Ok(self.groups.get(n).cloned().unwrap_or_default());
        expand_group(name, &mut visited, &mut lookup).unwrap_or_default()
    }

    /// Whether `name` is a defined license group
    pub fn contains(&self, name: &str) -> bool {
        self.groups.contains_key(name)
    }
}

fn classify_license_token(token: &str) -> Result<GroupEntry<Interned<DefaultInterner>>> {
    if let Some(name) = group_ref_name(token) {
        return Ok(GroupEntry::Ref(name.to_string()));
    }
    if token.is_empty() {
        return Err(Error::InvalidProfile("empty license token".to_string()));
    }
    Ok(GroupEntry::Leaf(Interned::intern(token)))
}

/// Effective `ACCEPT_LICENSE` after expanding `@GROUP` tokens and applying
/// `-` denials (portage `accept_license` semantics).
#[derive(Debug, Clone, Default)]
pub struct AcceptSet {
    /// `*` was present among allow tokens
    pub allow_all: bool,
    /// This list encountered `-*` (incremental clear-all)
    ///
    /// Used by [`AcceptSet::merge`] so a package.license line of `-* @FREE`
    /// can replace a global `ACCEPT_LICENSE=*`, not OR onto it.
    pub cleared: bool,
    allowed: HashSet<Interned<DefaultInterner>>,
    denied: HashSet<Interned<DefaultInterner>>,
}

impl AcceptSet {
    /// Build from raw `ACCEPT_LICENSE` tokens and a loaded group registry
    pub fn from_tokens(tokens: &[String], groups: &LicenseGroupRegistry) -> Self {
        let mut out = Self::default();
        for token in tokens {
            let (deny, raw) = token
                .strip_prefix('-')
                .map_or((false, token.as_str()), |r| (true, r));
            if raw == "*" {
                if deny {
                    // `-*` is the incremental clear-all (GLEP 23 / make.conf(5)):
                    // discard every license accepted *and* denied so far, so
                    // later tokens rebuild from empty (e.g. `-* @FREE` ⇒ only
                    // the FREE-group licenses are accepted).
                    out.allow_all = false;
                    out.allowed.clear();
                    out.denied.clear();
                    out.cleared = true;
                    continue;
                }
                out.allow_all = true;
                continue;
            }
            if let Some(name) = group_ref_name(raw) {
                for lic in groups.expand(name) {
                    out.insert(deny, lic);
                }
                continue;
            }
            out.insert(deny, Interned::intern(raw));
        }
        out
    }

    /// Build from raw tokens with no `@GROUP` support
    ///
    /// For `ACCEPT_PROPERTIES`/`ACCEPT_RESTRICT`, which share
    /// `ACCEPT_LICENSE`'s token grammar (`*`, `-token`, `-*`) but have no
    /// license-group concept. A `@name` token here (nonsensical for these
    /// variables) resolves against an empty registry and so contributes
    /// nothing, rather than being taken literally as a token named
    /// `@name`.
    pub fn from_tokens_plain(tokens: &[String]) -> Self {
        Self::from_tokens(tokens, &LicenseGroupRegistry::default())
    }

    fn insert(&mut self, deny: bool, lic: Interned<DefaultInterner>) {
        if deny {
            self.denied.insert(lic);
        } else {
            self.allowed.insert(lic);
        }
    }

    /// Layer `other` on top of this list for a per-package
    /// `package.license` overlay
    ///
    /// When `other` was built from a token list that included `-*`
    /// ([`AcceptSet::cleared`]), replace allow_all/allowed with other's
    /// post-clear state (so `-* @FREE` can restrict a global `*`).
    /// Otherwise union sets and OR `allow_all` (additive package.license
    /// lines).
    pub fn merge(&mut self, other: &AcceptSet) {
        if other.cleared {
            self.allow_all = other.allow_all;
            self.allowed = other.allowed.clone();
            self.denied = other.denied.clone();
            self.cleared = true;
            return;
        }
        self.allow_all |= other.allow_all;
        self.allowed.extend(other.allowed.iter().copied());
        self.denied.extend(other.denied.iter().copied());
    }

    /// Whether a single license identifier is accepted
    pub fn accepts(&self, name: &str) -> bool {
        if self.denied.contains(&Interned::intern(name)) {
            return false;
        }
        if self.allow_all {
            return true;
        }
        self.allowed.contains(&Interned::intern(name))
    }

    /// Whether a `LICENSE` expression is fully covered by this accept list
    ///
    /// `enabled` reports whether a USE flag is active for the package: a
    /// `flag? ( … )` / `!flag? ( … )` branch only contributes licenses when it
    /// is active, so a non-FREE license behind a disabled flag (e.g. ffmpeg's
    /// `fdk? ( all-rights-reserved )`) imposes no requirement. Pass a predicate
    /// that always returns `false` only when the expression has no conditionals.
    pub fn accepts_expr(&self, expr: &LicenseExpr, enabled: &dyn Fn(&str) -> bool) -> bool {
        match expr {
            LicenseExpr::License(name) => self.accepts(name),
            LicenseExpr::AnyOf(children) => children.iter().any(|c| self.accepts_expr(c, enabled)),
            LicenseExpr::All(children) => children.iter().all(|c| self.accepts_expr(c, enabled)),
            LicenseExpr::UseConditional {
                flag,
                negated,
                entries,
            } => {
                // Inactive branch imposes no license requirement.
                if enabled(flag) == *negated {
                    return true;
                }
                entries.iter().all(|e| self.accepts_expr(e, enabled))
            }
        }
    }

    /// Collect license names from `expr` not covered by this accept list
    /// Conditional branches are evaluated against `enabled` (see `accepts_expr`).
    pub fn licenses_needed(
        &self,
        expr: &LicenseExpr,
        enabled: &dyn Fn(&str) -> bool,
    ) -> Vec<String> {
        if self.allow_all && self.denied.is_empty() {
            return Vec::new();
        }
        match expr {
            LicenseExpr::License(name) => {
                if self.accepts(name) {
                    Vec::new()
                } else {
                    vec![name.clone()]
                }
            }
            LicenseExpr::AnyOf(children) => {
                if children.iter().any(|c| self.accepts_expr(c, enabled)) {
                    Vec::new()
                } else {
                    children
                        .first()
                        .map(|c| self.licenses_needed(c, enabled))
                        .unwrap_or_default()
                }
            }
            LicenseExpr::All(children) => children
                .iter()
                .flat_map(|c| self.licenses_needed(c, enabled))
                .collect(),
            LicenseExpr::UseConditional {
                flag,
                negated,
                entries,
            } => {
                // Inactive branch contributes nothing to unmask.
                if enabled(flag) == *negated {
                    return Vec::new();
                }
                entries
                    .iter()
                    .flat_map(|e| self.licenses_needed(e, enabled))
                    .collect()
            }
        }
    }

    /// Whether every top-level `RESTRICT`/`PROPERTIES` entry is accepted
    ///
    /// `enabled` reports whether a USE flag is active (see `accepts_expr`'s
    /// doc — same conditional-branch handling). Unlike [`LicenseExpr`],
    /// [`RestrictExpr`] has no `||` any-of group: a value is a flat entry
    /// list, an implicit AND, which is exactly what a slice's top level is.
    pub fn accepts_restrict(
        &self,
        entries: &[RestrictExpr],
        enabled: &dyn Fn(&str) -> bool,
    ) -> bool {
        entries
            .iter()
            .all(|e| self.accepts_restrict_entry(e, enabled))
    }

    fn accepts_restrict_entry(&self, entry: &RestrictExpr, enabled: &dyn Fn(&str) -> bool) -> bool {
        match entry {
            RestrictExpr::Token(name) => self.accepts(name),
            RestrictExpr::UseConditional {
                flag,
                negated,
                entries,
            } => enabled(flag) == *negated || self.accepts_restrict(entries, enabled),
        }
    }

    /// Collect entries not covered by this accept list (mirrors `licenses_needed`)
    pub fn restrict_needed(
        &self,
        entries: &[RestrictExpr],
        enabled: &dyn Fn(&str) -> bool,
    ) -> Vec<String> {
        entries
            .iter()
            .flat_map(|e| self.restrict_needed_entry(e, enabled))
            .collect()
    }

    fn restrict_needed_entry(
        &self,
        entry: &RestrictExpr,
        enabled: &dyn Fn(&str) -> bool,
    ) -> Vec<String> {
        match entry {
            RestrictExpr::Token(name) => {
                if self.accepts(name) {
                    Vec::new()
                } else {
                    vec![name.clone()]
                }
            }
            RestrictExpr::UseConditional {
                flag,
                negated,
                entries,
            } => {
                if enabled(flag) == *negated {
                    Vec::new()
                } else {
                    self.restrict_needed(entries, enabled)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# comment
FREE-SOFTWARE @FSF @OSI
FSF MIT GPL-2+
OSI Apache-2.0
"#;

    #[test]
    fn parse_and_expand_nested_groups() {
        let reg = LicenseGroupRegistry::parse(SAMPLE);
        let free = reg.expand("FREE-SOFTWARE");
        let names: HashSet<_> = free.iter().map(|l| l.as_str().to_string()).collect();
        assert!(names.contains("MIT"));
        assert!(names.contains("GPL-2+"));
        assert!(names.contains("Apache-2.0"));
    }

    #[test]
    fn accept_license_expands_groups_and_denies() {
        let reg = LicenseGroupRegistry::parse(SAMPLE);
        let acc = AcceptSet::from_tokens(&["@FREE-SOFTWARE".into(), "-GPL-2+".into()], &reg);
        assert!(acc.accepts("MIT"));
        assert!(!acc.accepts("GPL-2+"));
        assert!(!acc.accepts("unknown"));
    }

    #[test]
    fn accept_license_star_with_deny() {
        let reg = LicenseGroupRegistry::default();
        let acc = AcceptSet::from_tokens(&["*".into(), "-MIT".into()], &reg);
        assert!(acc.accepts("GPL-2"));
        assert!(!acc.accepts("MIT"));
    }

    #[test]
    fn package_license_clear_replaces_global_allow_all() {
        let reg = LicenseGroupRegistry::parse(SAMPLE);
        let mut global = AcceptSet::from_tokens(&["*".into()], &reg);
        assert!(global.accepts("all-rights-reserved"));
        let pkg = AcceptSet::from_tokens(&["-*".into(), "@FREE-SOFTWARE".into()], &reg);
        assert!(pkg.cleared);
        global.merge(&pkg);
        assert!(global.accepts("MIT"));
        assert!(
            !global.accepts("all-rights-reserved"),
            "package.license '-* @FREE' must clear global '*'"
        );
    }

    #[test]
    fn accept_license_dash_star_clears_accumulated() {
        let reg = LicenseGroupRegistry::default();
        // `-*` discards everything accumulated so far (allows and the `*`
        // allow-all), so later tokens rebuild from empty: only BSD is accepted.
        let acc =
            AcceptSet::from_tokens(&["*".into(), "MIT".into(), "-*".into(), "BSD".into()], &reg);
        assert!(acc.accepts("BSD"), "BSD added after the clear-all");
        assert!(!acc.accepts("MIT"), "MIT cleared by -*");
        assert!(
            !acc.allow_all,
            "the `*` allow-all is cleared too, so unknowns are not accepted"
        );
        assert!(!acc.accepts("GPL-2"), "nothing else is accepted after -*");
    }

    // Regression: a conditional `LICENSE` must be evaluated with the package's
    // effective USE. ffmpeg's `gpl? ( GPL-2+ fdk? ( all-rights-reserved ) )
    // !gpl? ( LGPL-2.1+ )` is FREE with `gpl` on / `fdk` off, even though the
    // disabled `fdk` branch names a non-FREE license. Walking every branch
    // (USE-blind) wrongly rejected it under ACCEPT_LICENSE="@FREE".
    #[test]
    fn conditional_license_respects_use() {
        let reg = LicenseGroupRegistry::default();
        // Only the free licenses are accepted (stand-in for @FREE).
        let acc = AcceptSet::from_tokens(&["GPL-2+".into(), "LGPL-2.1+".into()], &reg);
        let expr =
            LicenseExpr::parse("gpl? ( GPL-2+ fdk? ( all-rights-reserved ) ) !gpl? ( LGPL-2.1+ )")
                .unwrap();

        // gpl on, fdk off → active license is GPL-2+ only → accepted.
        let on_off = |f: &str| f == "gpl";
        assert!(acc.accepts_expr(&expr, &on_off));
        assert!(acc.licenses_needed(&expr, &on_off).is_empty());

        // gpl on, fdk on → the all-rights-reserved branch is active → rejected.
        let on_on = |f: &str| matches!(f, "gpl" | "fdk");
        assert!(!acc.accepts_expr(&expr, &on_on));
        assert_eq!(
            acc.licenses_needed(&expr, &on_on),
            vec!["all-rights-reserved".to_string()]
        );

        // gpl off → only the !gpl branch (LGPL-2.1+) is active → accepted.
        let off = |_: &str| false;
        assert!(acc.accepts_expr(&expr, &off));
        assert!(acc.licenses_needed(&expr, &off).is_empty());
    }
}
