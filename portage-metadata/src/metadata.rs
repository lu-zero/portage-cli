use std::collections::HashSet;

use crate::interner::{DefaultInterner, Interned, Interner};
use portage_atom::{DepEntry, LazyDepList, Slot};

use crate::eapi::Eapi;
use crate::iuse::IUse;
use crate::keyword::Keyword;
use crate::license::LicenseExpr;
use crate::phase::Phase;
use crate::required_use::RequiredUseExpr;
use crate::restrict::RestrictExpr;
use crate::src_uri::LazySrcUriList;

/// Metadata for a single ebuild, as produced by the metadata cache
///
/// Contains all the PMS-defined metadata variables that a package manager
/// extracts from an ebuild. Mandatory fields (`eapi`, `description`, `slot`)
/// are always present; optional fields use `Option` or `Vec`.
///
/// See [PMS 7.2](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EbuildMetadata<I = DefaultInterner>
where
    I: Interner,
{
    /// EAPI version
    ///
    /// See [PMS 7.3.1](https://projects.gentoo.org/pms/9/pms.html#eapi).
    pub eapi: Eapi,

    /// Package description (mandatory)
    ///
    /// See [PMS 7.2](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
    pub description: String,

    /// Package slot (mandatory)
    ///
    /// See [PMS 7.2](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
    pub slot: Slot,

    /// Homepage URL(s)
    pub homepage: Vec<String>,

    /// Source URI expression
    ///
    /// A [`LazySrcUriList`]: only planned/selected packages ever need this
    /// parsed (download-size accounting, `em search`), not the whole tree.
    pub src_uri: LazySrcUriList,

    /// License expression
    pub license: Option<LicenseExpr>,

    /// Architecture keywords
    pub keywords: Vec<Keyword<I>>,

    /// USE flags declared by the ebuild
    pub iuse: Vec<IUse<I>>,

    /// REQUIRED_USE expression (EAPI 4+)
    pub required_use: Option<RequiredUseExpr>,

    /// RESTRICT entries
    pub restrict: Vec<RestrictExpr>,

    /// PROPERTIES entries
    pub properties: Vec<RestrictExpr>,

    /// Build-time dependencies (`DEPEND`)
    ///
    /// A [`LazyDepList`]: only a small fraction of a repo's ebuilds ever
    /// have their dependencies examined by a resolve's solver, so the raw
    /// text is parsed on first access rather than eagerly for every ebuild
    /// `load_repos()` reads.
    ///
    /// See [PMS 8.1](https://projects.gentoo.org/pms/9/pms.html#dependency-classes).
    pub depend: LazyDepList,

    /// Runtime dependencies (`RDEPEND`)
    pub rdepend: LazyDepList,

    /// Build-host dependencies (`BDEPEND`, EAPI 7+)
    pub bdepend: LazyDepList,

    /// Post-merge dependencies (`PDEPEND`)
    pub pdepend: LazyDepList,

    /// Install-time dependencies (`IDEPEND`, EAPI 8)
    pub idepend: LazyDepList,

    /// Eclasses directly listed in the ebuild's `inherit` statement
    ///
    /// Stored as `INHERIT=` in the md5-dict cache format.  This is a portage
    /// auxdb extension; it is not specified by PMS.
    ///
    /// See [PMS 10.1](https://projects.gentoo.org/pms/latest/pms.html#the-inherit-command).
    pub inherit: Vec<String>,

    /// All transitively inherited eclass names (direct + nested)
    ///
    /// Corresponds to the [`INHERITED`](https://projects.gentoo.org/pms/latest/pms.html#magic-ebuild-defined-variables)
    /// ebuild variable (PMS 7.4).  In the md5-dict cache format (PMS 14.3)
    /// this key is excluded; the names are derived from `_eclasses_` instead.
    ///
    /// See [PMS 10.1](https://projects.gentoo.org/pms/latest/pms.html#the-inherit-command)
    /// and [PMS 14.3](https://projects.gentoo.org/pms/latest/pms.html#md5-dict-cache-file-format).
    ///
    /// Interned, sharing keys with [`crate::cache::CacheEntry::eclasses`] —
    /// see that field's doc for why.
    pub inherited: Vec<Interned<I>>,

    /// Defined phase functions
    pub defined_phases: Vec<Phase>,
}

impl<I: Interner + Clone> EbuildMetadata<I> {
    /// Whether this ebuild is *live* — tracks upstream HEAD rather than a
    /// release
    ///
    /// Gentoo's convention: a `PROPERTIES="live"` token (git-r3 et al), or
    /// the caller checks the `*9999` version-shape separately. Only
    /// unconditional tokens count; `flag? ( live )` means live only under
    /// that flag and reads as not-live here.
    pub fn is_live(&self) -> bool {
        fn unconditional_live(entries: &[RestrictExpr]) -> bool {
            entries
                .iter()
                .any(|e| matches!(e, RestrictExpr::Token(t) if t == "live"))
        }
        unconditional_live(&self.properties)
    }

    /// Whether any DEPEND-class field or `SRC_URI` fell back to empty
    /// because its raw text failed to parse, rather than genuinely having
    /// no content
    ///
    /// Forces the parse of every lazy field to answer — only meant for
    /// callers already paying that cost (writing a cache entry back out,
    /// or re-validating an already-suspect entry), never the bulk
    /// resolve-time read path this laziness exists for.
    pub fn has_parse_failure(&self) -> bool {
        self.depend.list();
        self.rdepend.list();
        self.bdepend.list();
        self.pdepend.list();
        self.idepend.list();
        self.src_uri.list();
        self.depend.parse_failed()
            || self.rdepend.parse_failed()
            || self.bdepend.parse_failed()
            || self.pdepend.parse_failed()
            || self.idepend.parse_failed()
            || self.src_uri.parse_failed()
    }

    /// Which lazy fields failed to parse and why, `"NAME: message"` per
    /// field joined with `"; "` — for a caller that already knows
    /// [`Self::has_parse_failure`] is `true` and wants a real diagnostic
    /// instead of a generic message. Forces the same six fields
    /// `has_parse_failure` does (already memoized if that ran first).
    pub fn parse_failure_summary(&self) -> String {
        let mut parts = Vec::new();
        macro_rules! check {
            ($name:literal, $field:expr) => {{
                $field.list();
                if $field.parse_failed() {
                    parts.push(format!(
                        "{}: {}",
                        $name,
                        $field.parse_error().unwrap_or("unknown parse error")
                    ));
                }
            }};
        }
        check!("DEPEND", self.depend);
        check!("RDEPEND", self.rdepend);
        check!("BDEPEND", self.bdepend);
        check!("PDEPEND", self.pdepend);
        check!("IDEPEND", self.idepend);
        check!("SRC_URI", self.src_uri);
        parts.join("; ")
    }

    /// Return a copy with duplicate top-level dep entries removed (first occurrence wins)
    ///
    /// Portage and portage-repo accumulate eclass contributions by appending
    /// `E_*` values after sourcing, while the ebuild may already have expanded
    /// the same eclass variable inline (e.g. `REQUIRED_USE="${PYTHON_REQUIRED_USE}
    /// ..."`). The result is that the same constraint appears twice. pkgcraft
    /// deduplicates during its own regen; this method normalises to that form.
    pub fn dedup(&self) -> Self {
        let mut result = self.clone();
        dedup_dep(result.depend.make_mut());
        dedup_dep(result.rdepend.make_mut());
        dedup_dep(result.bdepend.make_mut());
        dedup_dep(result.pdepend.make_mut());
        dedup_dep(result.idepend.make_mut());
        if let Some(ref ru) = self.required_use {
            result.required_use = Some(ru.dedup());
        }
        if let Some(ref lic) = self.license {
            result.license = Some(lic.dedup());
        }
        result
    }
}

fn dedup_dep(entries: &mut Vec<DepEntry>) {
    let mut seen: HashSet<DepEntry> = HashSet::new();
    entries.retain(|e| seen.insert(e.clone()));
    for entry in entries.iter_mut() {
        match entry {
            DepEntry::UseConditional { children, .. } => dedup_dep(children),
            DepEntry::AllOf(children) => dedup_dep(children),
            DepEntry::AnyOf(children) => dedup_dep(children),
            DepEntry::ExactlyOneOf(children) => dedup_dep(children),
            DepEntry::AtMostOneOf(children) => dedup_dep(children),
            DepEntry::Atom(_) => {}
        }
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::cache::CacheEntry;

    fn cache_with_properties(properties: &str) -> EbuildMetadata {
        CacheEntry::parse(&format!(
            "EAPI=8\nSLOT=0\nPROPERTIES={properties}\nDESCRIPTION=t\n_md5_=x\n"
        ))
        .unwrap()
        .metadata
    }

    #[test]
    fn is_live_detects_unconditional_property() {
        assert!(cache_with_properties("live").is_live());
        assert!(cache_with_properties("mirror live").is_live());
    }

    #[test]
    fn is_live_ignores_conditional_and_absent_properties() {
        assert!(!cache_with_properties("live? ( live )").is_live());
        assert!(!cache_with_properties("").is_live());
    }
}
