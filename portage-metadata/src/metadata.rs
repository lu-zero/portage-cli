use std::collections::HashSet;

use crate::interner::{DefaultInterner, Interner};
use portage_atom::{DepEntry, LazyDepList, Slot};

use crate::eapi::Eapi;
use crate::iuse::IUse;
use crate::keyword::Keyword;
use crate::license::LicenseExpr;
use crate::phase::Phase;
use crate::required_use::RequiredUseExpr;
use crate::restrict::RestrictExpr;
use crate::src_uri::SrcUriEntry;

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
    pub src_uri: Vec<SrcUriEntry>,

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
    pub inherited: Vec<String>,

    /// Defined phase functions
    pub defined_phases: Vec<Phase>,
}

impl<I: Interner + Clone> EbuildMetadata<I> {
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
