use crate::interner::{DefaultInterner, Interner};
use portage_atom::{DepEntry, LazyDepList, Slot};

use crate::eapi::Eapi;
use crate::error::{Error, Result};
use crate::iuse::IUse;
use crate::keyword::Keyword;
use crate::license::LicenseExpr;
use crate::metadata::EbuildMetadata;
use crate::phase::Phase;
use crate::required_use::RequiredUseExpr;
use crate::restrict::RestrictExpr;
use crate::src_uri::SrcUriEntry;

/// Borrowed line-oriented view over a raw md5-cache file's text
///
/// A cache file is `KEY=VALUE\n` per line (see
/// [PMS 14.2](https://projects.gentoo.org/pms/9/pms.html#mddict-cache-file-format)).
/// `RawCacheEntry` validates only that structure and lets callers pluck out
/// individual fields as borrowed `&str` slices.
///
/// Use this when you don't need atom-tree parsing for
/// DEPEND/RDEPEND/IUSE/KEYWORDS — e.g. just `DESCRIPTION` for a search hit,
/// or `_md5_`/`_eclasses_` for a staleness check.
///
/// To get the full typed parse, build a [`CacheEntry`] via [`CacheEntry::parse`]
/// or [`CacheEntry::from_kv_pairs`] — the latter accepts the same `(key, value)`
/// pairs this view yields, so the two layers compose cleanly.
///
/// Malformed lines (no `=`) are silently skipped. Values are not trimmed: they
/// match the bytes between `=` and end-of-line in the source.
#[derive(Debug, Clone, Copy)]
pub struct RawCacheEntry<'a> {
    text: &'a str,
}

impl<'a> RawCacheEntry<'a> {
    /// Wrap a cache file's raw text. No parsing happens until a method is called
    pub fn new(text: &'a str) -> Self {
        Self { text }
    }

    /// First value for `key`, or `None` if no line in the file matches
    pub fn field(&self, key: &str) -> Option<&'a str> {
        self.lines().find_map(|(k, v)| (k == key).then_some(v))
    }

    /// Resolve several fields in a single pass over the text
    ///
    /// Returns one `Option<&str>` per requested key, in the same order. Faster
    /// than calling [`Self::field`] in a loop when more than one key is needed.
    pub fn fields<const N: usize>(&self, keys: [&str; N]) -> [Option<&'a str>; N] {
        let mut out: [Option<&'a str>; N] = [None; N];
        for (k, v) in self.lines() {
            for i in 0..N {
                if out[i].is_none() && k == keys[i] {
                    out[i] = Some(v);
                }
            }
            if out.iter().all(Option::is_some) {
                break;
            }
        }
        out
    }

    /// Every `KEY=VALUE` pair in the file, in source order
    ///
    /// Underscored keys (`_eclasses_`, `_md5_`) are included; lines without
    /// `=` are skipped.
    pub fn lines(&self) -> impl Iterator<Item = (&'a str, &'a str)> + '_ {
        self.text.lines().filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            line.split_once('=')
        })
    }
}

/// A parsed md5-cache entry
///
/// Represents a single file from `metadata/md5-cache/<category>/<package>-<version>`.
/// Contains the full ebuild metadata plus cache-specific fields (`md5`, `eclasses`).
///
/// See [PMS 14.2](https://projects.gentoo.org/pms/9/pms.html#mddict-cache-file-format).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry<I = DefaultInterner>
where
    I: Interner,
{
    /// The ebuild metadata
    pub metadata: EbuildMetadata<I>,

    /// MD5 checksum of the ebuild file (from `_md5_`)
    pub md5: Option<String>,

    /// All transitively inherited eclasses with their checksums (from `_eclasses_`)
    ///
    /// Each tuple is `(eclass_name, md5_checksum)`.  Pairs are tab-separated
    /// as described in [PMS 14.3](https://projects.gentoo.org/pms/latest/pms.html#md5-dict-cache-file-format).
    pub eclasses: Vec<(String, String)>,
}

/// Accumulator for key-value pairs before building a `CacheEntry`
///
/// Holds `&str` slices into the source data — no intermediate String
/// allocations.  Call `finish()` to parse and build the typed entry.
struct ParseState<'a> {
    eapi: &'a str,
    description: Option<&'a str>,
    slot: Option<&'a str>,
    homepage: &'a str,
    src_uri: &'a str,
    license: &'a str,
    keywords: &'a str,
    iuse: &'a str,
    required_use: &'a str,
    restrict: &'a str,
    properties: &'a str,
    depend: &'a str,
    rdepend: &'a str,
    bdepend: &'a str,
    pdepend: &'a str,
    idepend: &'a str,
    inherit: &'a str,
    defined_phases: &'a str,
    md5: Option<&'a str>,
    eclasses_raw: &'a str,
}

impl<'a> ParseState<'a> {
    fn new() -> Self {
        Self {
            eapi: "",
            description: None,
            slot: None,
            homepage: "",
            src_uri: "",
            license: "",
            keywords: "",
            iuse: "",
            required_use: "",
            restrict: "",
            properties: "",
            depend: "",
            rdepend: "",
            bdepend: "",
            pdepend: "",
            idepend: "",
            inherit: "",
            defined_phases: "",
            md5: None,
            eclasses_raw: "",
        }
    }

    fn feed(&mut self, key: &str, value: &'a str) {
        match key {
            "EAPI" => self.eapi = value,
            "DESCRIPTION" => self.description = Some(value),
            "SLOT" => self.slot = Some(value),
            "HOMEPAGE" => self.homepage = value,
            "SRC_URI" => self.src_uri = value,
            "LICENSE" => self.license = value,
            "KEYWORDS" => self.keywords = value,
            "IUSE" => self.iuse = value,
            "REQUIRED_USE" => self.required_use = value,
            "RESTRICT" => self.restrict = value,
            "PROPERTIES" => self.properties = value,
            "DEPEND" => self.depend = value,
            "RDEPEND" => self.rdepend = value,
            "BDEPEND" => self.bdepend = value,
            "PDEPEND" => self.pdepend = value,
            "IDEPEND" => self.idepend = value,
            "INHERIT" => self.inherit = value,
            "DEFINED_PHASES" => self.defined_phases = value,
            "_md5_" => self.md5 = Some(value),
            "_eclasses_" => self.eclasses_raw = value,
            _ => {}
        }
    }

    fn finish<I: Interner>(self) -> Result<CacheEntry<I>> {
        let eapi_val = if self.eapi.is_empty() {
            Eapi::Zero
        } else {
            self.eapi
                .parse::<Eapi>()
                .map_err(|_| Error::InvalidEapi(self.eapi.to_string()))?
        };

        let description_val = self
            .description
            .ok_or_else(|| Error::MissingField("DESCRIPTION".to_string()))?
            .to_string();

        let slot_val = match self.slot {
            Some(s) => parse_slot(s)?,
            None => return Err(Error::MissingField("SLOT".to_string())),
        };

        let homepage_val: Vec<String> = if self.homepage.is_empty() {
            Vec::new()
        } else {
            self.homepage
                .split_whitespace()
                .map(|s| s.to_string())
                .collect()
        };

        let src_uri_val = if self.src_uri.is_empty() {
            Vec::new()
        } else {
            SrcUriEntry::parse(self.src_uri)?
        };

        let license_val = if self.license.is_empty() {
            None
        } else {
            Some(LicenseExpr::parse(self.license)?)
        };

        let keywords_val: Vec<Keyword<I>> = if self.keywords.is_empty() {
            Vec::new()
        } else {
            self.keywords
                .split_whitespace()
                .map(|token| Keyword::parse(token))
                .collect::<Result<_>>()?
        };

        let iuse_val: Vec<IUse<I>> = if self.iuse.is_empty() {
            Vec::new()
        } else {
            self.iuse
                .split_whitespace()
                .map(|token| IUse::parse(token))
                .collect::<Result<_>>()?
        };

        let required_use_val = if self.required_use.is_empty() {
            None
        } else {
            Some(RequiredUseExpr::parse(self.required_use)?)
        };

        let restrict_val = if self.restrict.is_empty() {
            Vec::new()
        } else {
            RestrictExpr::parse(self.restrict)?
        };

        let properties_val = if self.properties.is_empty() {
            Vec::new()
        } else {
            RestrictExpr::parse(self.properties)?
        };

        let eclasses = parse_eclasses(self.eclasses_raw);

        let inherit_val: Vec<String> = self
            .inherit
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        // PMS 14.3: md5-dict format excludes the INHERITED key; the
        // transitive eclass list is carried by _eclasses_ instead.
        let inherited_val: Vec<String> = eclasses.iter().map(|(name, _)| name.clone()).collect();

        let defined_phases_val = Phase::parse_line(self.defined_phases)?;

        Ok(CacheEntry {
            metadata: EbuildMetadata {
                eapi: eapi_val,
                description: description_val,
                slot: slot_val,
                homepage: homepage_val,
                src_uri: src_uri_val,
                license: license_val,
                keywords: keywords_val,
                iuse: iuse_val,
                required_use: required_use_val,
                restrict: restrict_val,
                properties: properties_val,
                depend: LazyDepList::from_raw(self.depend),
                rdepend: LazyDepList::from_raw(self.rdepend),
                bdepend: LazyDepList::from_raw(self.bdepend),
                pdepend: LazyDepList::from_raw(self.pdepend),
                idepend: LazyDepList::from_raw(self.idepend),
                inherit: inherit_val,
                inherited: inherited_val,
                defined_phases: defined_phases_val,
            },
            md5: self.md5.map(|s| s.to_string()),
            eclasses,
        })
    }
}

impl<I: Interner> CacheEntry<I> {
    fn parse_impl(input: &str) -> Result<CacheEntry<I>> {
        let mut state = ParseState::new();
        for (key, value) in RawCacheEntry::new(input).lines() {
            state.feed(key, value);
        }
        state.finish()
    }

    /// Serialize this cache entry back to md5-cache format
    ///
    /// Produces a string suitable for writing to a cache file.
    /// Empty-valued fields are omitted.
    pub fn serialize(&self) -> String {
        let m = &self.metadata;
        let mut lines = Vec::new();

        // Always emit mandatory fields
        lines.push(format!(
            "DEFINED_PHASES={}",
            format_phases(&m.defined_phases)
        ));

        if !m.depend.is_empty_raw() {
            lines.push(format!("DEPEND={}", format_dep_entries(m.depend.list())));
        }

        lines.push(format!("DESCRIPTION={}", m.description));
        lines.push(format!("EAPI={}", m.eapi));

        if !m.homepage.is_empty() {
            lines.push(format!("HOMEPAGE={}", m.homepage.join(" ")));
        }

        if !m.iuse.is_empty() {
            let iuse_str: Vec<String> = m.iuse.iter().map(|i| i.to_string()).collect();
            lines.push(format!("IUSE={}", iuse_str.join(" ")));
        }

        if !m.keywords.is_empty() {
            let kw_str: Vec<String> = m.keywords.iter().map(|k| k.to_string()).collect();
            lines.push(format!("KEYWORDS={}", kw_str.join(" ")));
        }

        if let Some(ref lic) = m.license {
            lines.push(format!("LICENSE={}", lic));
        }

        if !m.pdepend.is_empty_raw() {
            lines.push(format!("PDEPEND={}", format_dep_entries(m.pdepend.list())));
        }

        if !m.rdepend.is_empty_raw() {
            lines.push(format!("RDEPEND={}", format_dep_entries(m.rdepend.list())));
        }

        if let Some(ref ru) = m.required_use {
            lines.push(format!("REQUIRED_USE={}", ru));
        }

        if !m.restrict.is_empty() {
            let r_str: Vec<String> = m.restrict.iter().map(|r| r.to_string()).collect();
            lines.push(format!("RESTRICT={}", r_str.join(" ")));
        }

        lines.push(format!("SLOT={}", m.slot));

        if !m.src_uri.is_empty() {
            let uri_str: Vec<String> = m.src_uri.iter().map(|u| u.to_string()).collect();
            lines.push(format!("SRC_URI={}", uri_str.join(" ")));
        }

        if !m.bdepend.is_empty_raw() {
            lines.push(format!("BDEPEND={}", format_dep_entries(m.bdepend.list())));
        }

        if !m.idepend.is_empty_raw() {
            lines.push(format!("IDEPEND={}", format_dep_entries(m.idepend.list())));
        }

        if !m.properties.is_empty() {
            let p_str: Vec<String> = m.properties.iter().map(|p| p.to_string()).collect();
            lines.push(format!("PROPERTIES={}", p_str.join(" ")));
        }

        if !m.inherit.is_empty() {
            lines.push(format!("INHERIT={}", m.inherit.join(" ")));
        }

        if !self.eclasses.is_empty() {
            let parts: Vec<String> = self
                .eclasses
                .iter()
                .flat_map(|(name, checksum)| vec![name.clone(), checksum.clone()])
                .collect();
            lines.push(format!("_eclasses_={}", parts.join("\t")));
        }

        if let Some(ref md5) = self.md5 {
            lines.push(format!("_md5_={}", md5));
        }

        lines.push(String::new()); // trailing newline
        lines.join("\n")
    }
}

impl CacheEntry<DefaultInterner> {
    /// Parse a md5-cache file's contents into a `CacheEntry`
    ///
    /// The input is the full text of a cache file. Lines are `KEY=VALUE`
    /// pairs in arbitrary order. Empty values may be omitted entirely.
    ///
    /// # Examples
    ///
    /// ```
    /// use portage_metadata::CacheEntry;
    ///
    /// let input = "\
    /// EAPI=7
    /// DESCRIPTION=Example package
    /// SLOT=0
    /// DEFINED_PHASES=compile install
    /// KEYWORDS=~amd64
    /// ";
    /// let entry = CacheEntry::parse(input).unwrap();
    /// assert_eq!(entry.metadata.description, "Example package");
    /// ```
    pub fn parse(input: &str) -> Result<Self> {
        Self::parse_impl(input)
    }

    /// Build a `CacheEntry` from an iterator of `(key, value)` string pairs
    ///
    /// Avoids the text-format round-trip of `parse` — useful when building
    /// entries from in-memory data (e.g., shell environment variables).
    /// Unknown keys are silently ignored, matching `parse` behaviour.
    pub fn from_kv_pairs<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> Result<Self> {
        let mut state = ParseState::new();
        for (key, value) in pairs {
            state.feed(key, value);
        }
        state.finish()
    }
}

/// Check that a slot or subslot name is valid per PMS 3.1.3
///
/// Slot names may contain `[A-Za-z0-9+_.-]` and must not begin with `-`, `.`, or `+`.
fn is_valid_slot_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let first = s.as_bytes()[0];
    if first == b'-' || first == b'.' || first == b'+' {
        return false;
    }
    s.bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'_' | b'.' | b'-'))
}

/// Parse a SLOT value into a `Slot`
fn parse_slot(s: &str) -> Result<Slot> {
    if s.is_empty() {
        return Err(Error::MissingField("SLOT".to_string()));
    }
    if let Some((slot, subslot)) = s.split_once('/') {
        if !is_valid_slot_name(slot) || !is_valid_slot_name(subslot) {
            return Err(Error::InvalidSlot(s.to_string()));
        }
        Ok(Slot::with_subslot(slot, subslot))
    } else {
        if !is_valid_slot_name(s) {
            return Err(Error::InvalidSlot(s.to_string()));
        }
        Ok(Slot::new(s))
    }
}

/// Parse the `_eclasses_` value: tab-separated pairs of `name\tchecksum`
fn parse_eclasses(s: &str) -> Vec<(String, String)> {
    if s.is_empty() {
        return Vec::new();
    }
    let parts: Vec<&str> = s.split('\t').collect();
    parts
        .chunks(2)
        .filter_map(|chunk| {
            if chunk.len() == 2 {
                Some((chunk[0].to_string(), chunk[1].to_string()))
            } else {
                None
            }
        })
        .collect()
}

/// Format DEFINED_PHASES for serialization
fn format_phases(phases: &[Phase]) -> String {
    if phases.is_empty() {
        "-".to_string()
    } else {
        phases
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<&str>>()
            .join(" ")
    }
}

/// Format dependency entries for serialization
fn format_dep_entries(entries: &[DepEntry]) -> String {
    let strs: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
    strs.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eapi::Eapi;
    use crate::keyword::Stability;

    const EXAMPLE_CACHE: &str = "\
DEFINED_PHASES=install test unpack
DEPEND=>=sys-devel/clang-10.0.0_rc1:* dev-python/setuptools
DESCRIPTION=Python bindings for sys-devel/clang
EAPI=7
HOMEPAGE=https://llvm.org/
IUSE=test python_targets_python3_6 python_targets_python3_7
KEYWORDS=~amd64 ~x86
LICENSE=Apache-2.0-with-LLVM-exceptions UoI-NCSA
RDEPEND=>=sys-devel/clang-10.0.0_rc1:*
REQUIRED_USE=|| ( python_targets_python3_6 python_targets_python3_7 )
RESTRICT=!test? ( test )
SLOT=0
SRC_URI=https://github.com/llvm/llvm-project/archive/llvmorg-10.0.0-rc1.tar.gz
_eclasses_=llvm.org\t4e92abc\tmultibuild\t40fe1234
_md5_=4539d849d3cea8ac84debad9b3154143
";

    #[test]
    fn parse_example() {
        let entry = CacheEntry::parse(EXAMPLE_CACHE).unwrap();
        assert_eq!(entry.metadata.eapi, Eapi::Seven);
        assert_eq!(
            entry.metadata.description,
            "Python bindings for sys-devel/clang"
        );
        assert_eq!(entry.metadata.slot.slot, "0");
        assert_eq!(entry.metadata.slot.subslot, None);
        assert_eq!(entry.metadata.homepage, vec!["https://llvm.org/"]);
        assert_eq!(entry.metadata.keywords.len(), 2);
        assert_eq!(entry.metadata.keywords[0].arch.as_str(), "amd64");
        assert_eq!(entry.metadata.keywords[0].stability, Stability::Testing);
        assert_eq!(entry.metadata.iuse.len(), 3);
        assert!(entry.metadata.required_use.is_some());
        assert!(!entry.metadata.restrict.is_empty());
        assert_eq!(entry.metadata.defined_phases.len(), 3);
        assert_eq!(entry.metadata.src_uri.len(), 1);
        assert!(!entry.metadata.depend.list().is_empty());
        assert!(!entry.metadata.rdepend.list().is_empty());
        assert!(entry.metadata.bdepend.list().is_empty()); // EAPI 7 but no BDEPEND in this example
        assert_eq!(
            entry.md5,
            Some("4539d849d3cea8ac84debad9b3154143".to_string())
        );
        assert_eq!(entry.eclasses.len(), 2);
        assert_eq!(entry.eclasses[0].0, "llvm.org");
        assert_eq!(entry.eclasses[1].0, "multibuild");
        assert!(entry.metadata.inherit.is_empty());
        assert_eq!(entry.metadata.inherited, vec!["llvm.org", "multibuild"]);
    }

    #[test]
    fn parse_minimal() {
        let input = "DESCRIPTION=Minimal\nSLOT=0\n";
        let entry = CacheEntry::parse(input).unwrap();
        assert_eq!(entry.metadata.eapi, Eapi::Zero);
        assert_eq!(entry.metadata.description, "Minimal");
        assert_eq!(entry.metadata.slot.slot, "0");
    }

    #[test]
    fn missing_description() {
        let input = "EAPI=7\nSLOT=0\n";
        let err = CacheEntry::parse(input).unwrap_err();
        assert!(matches!(err, Error::MissingField(ref f) if f == "DESCRIPTION"));
    }

    #[test]
    fn missing_slot() {
        let input = "EAPI=7\nDESCRIPTION=Test\n";
        let err = CacheEntry::parse(input).unwrap_err();
        assert!(matches!(err, Error::MissingField(ref f) if f == "SLOT"));
    }

    #[test]
    fn slot_with_subslot() {
        let input = "DESCRIPTION=Test\nSLOT=0/2.1\n";
        let entry = CacheEntry::parse(input).unwrap();
        assert_eq!(entry.metadata.slot.slot, "0");
        assert_eq!(
            entry.metadata.slot.subslot,
            Some(crate::interner::Interned::intern("2.1"))
        );
    }

    #[test]
    fn parse_eclasses() {
        let eclasses = super::parse_eclasses("llvm.org\tabc123\tmultibuild\tdef456");
        assert_eq!(eclasses.len(), 2);
        assert_eq!(eclasses[0], ("llvm.org".to_string(), "abc123".to_string()));
        assert_eq!(
            eclasses[1],
            ("multibuild".to_string(), "def456".to_string())
        );
    }

    #[test]
    fn parse_eclasses_empty() {
        let eclasses = super::parse_eclasses("");
        assert!(eclasses.is_empty());
    }

    #[test]
    fn parse_eclasses_odd_count() {
        // Odd number of tab-separated values: last one is ignored
        let eclasses = super::parse_eclasses("llvm.org\tabc123\torphan");
        assert_eq!(eclasses.len(), 1);
    }

    #[test]
    fn serialize_round_trip() {
        let entry = CacheEntry::parse(EXAMPLE_CACHE).unwrap();
        let serialized = entry.serialize();
        let reparsed = CacheEntry::parse(&serialized).unwrap();
        assert_eq!(entry.metadata.eapi, reparsed.metadata.eapi);
        assert_eq!(entry.metadata.description, reparsed.metadata.description);
        assert_eq!(entry.metadata.slot, reparsed.metadata.slot);
        assert_eq!(
            entry.metadata.keywords.len(),
            reparsed.metadata.keywords.len()
        );
        assert_eq!(entry.md5, reparsed.md5);
        assert_eq!(entry.eclasses, reparsed.eclasses);
    }

    #[test]
    fn defined_phases_dash() {
        let input = "DESCRIPTION=Test\nSLOT=0\nDEFINED_PHASES=-\n";
        let entry = CacheEntry::parse(input).unwrap();
        assert!(entry.metadata.defined_phases.is_empty());
    }

    #[test]
    fn unknown_keys_ignored() {
        let input = "DESCRIPTION=Test\nSLOT=0\nFOO=bar\n";
        let entry = CacheEntry::parse(input).unwrap();
        assert_eq!(entry.metadata.description, "Test");
    }

    #[test]
    fn empty_lines_ignored() {
        let input = "\nDESCRIPTION=Test\n\nSLOT=0\n\n";
        let entry = CacheEntry::parse(input).unwrap();
        assert_eq!(entry.metadata.description, "Test");
    }

    #[test]
    fn license_parsing() {
        let input = "DESCRIPTION=Test\nSLOT=0\nLICENSE=Apache-2.0-with-LLVM-exceptions UoI-NCSA\n";
        let entry = CacheEntry::parse(input).unwrap();
        assert!(entry.metadata.license.is_some());
    }

    #[test]
    fn eapi8_idepend() {
        let input = "EAPI=8\nDESCRIPTION=Test\nSLOT=0\nIDEPEND=sys-apps/systemd\n";
        let entry = CacheEntry::parse(input).unwrap();
        assert_eq!(entry.metadata.eapi, Eapi::Eight);
        assert_eq!(entry.metadata.idepend.list().len(), 1);
    }

    #[test]
    fn inherit_direct_only() {
        let input = "DESCRIPTION=Test\nSLOT=0\nINHERIT=foo bar\n";
        let entry = CacheEntry::parse(input).unwrap();
        assert_eq!(entry.metadata.inherit, vec!["foo", "bar"]);
        assert!(entry.metadata.inherited.is_empty());
    }

    #[test]
    fn inherited_from_eclasses() {
        let input = "DESCRIPTION=Test\nSLOT=0\n_eclasses_=alpha\tdeadbeef\tbeta\tcafe1234\n";
        let entry = CacheEntry::parse(input).unwrap();
        assert!(entry.metadata.inherit.is_empty());
        assert_eq!(entry.metadata.inherited, vec!["alpha", "beta"]);
        assert_eq!(entry.eclasses.len(), 2);
    }

    #[test]
    fn inherit_and_eclasses_together() {
        let input = "\
DESCRIPTION=Test
SLOT=0
INHERIT=foo
_eclasses_=foo\taabb\tbar\tccdd
";
        let entry = CacheEntry::parse(input).unwrap();
        assert_eq!(entry.metadata.inherit, vec!["foo"]);
        assert_eq!(entry.metadata.inherited, vec!["foo", "bar"]);
    }

    #[test]
    fn inherited_key_ignored_in_md5_dict() {
        let input = "\
DESCRIPTION=Test
SLOT=0
INHERITED=ignored_legacy
_eclasses_=real\t1234
";
        let entry = CacheEntry::parse(input).unwrap();
        assert_eq!(entry.metadata.inherited, vec!["real"]);
    }

    #[test]
    fn serialize_inherit_round_trip() {
        let input = "\
DESCRIPTION=Test
SLOT=0
INHERIT=foo bar
_eclasses_=foo\taabb\tbar\tccdd\tbaz\teeff
";
        let entry = CacheEntry::parse(input).unwrap();
        let serialized = entry.serialize();
        let reparsed = CacheEntry::parse(&serialized).unwrap();
        assert_eq!(reparsed.metadata.inherit, vec!["foo", "bar"]);
        assert_eq!(reparsed.metadata.inherited, vec!["foo", "bar", "baz"]);
        assert_eq!(reparsed.eclasses, entry.eclasses);
    }

    #[test]
    fn invalid_slot_starts_with_dash() {
        let input = "DESCRIPTION=Test\nSLOT=-invalid\n";
        assert!(CacheEntry::parse(input).is_err());
    }

    #[test]
    fn invalid_slot_starts_with_dot() {
        let input = "DESCRIPTION=Test\nSLOT=.invalid\n";
        assert!(CacheEntry::parse(input).is_err());
    }

    #[test]
    fn invalid_slot_starts_with_plus() {
        let input = "DESCRIPTION=Test\nSLOT=+invalid\n";
        assert!(CacheEntry::parse(input).is_err());
    }

    #[test]
    fn valid_slot_with_special_chars() {
        let input = "DESCRIPTION=Test\nSLOT=2.7-r1\n";
        let entry = CacheEntry::parse(input).unwrap();
        assert_eq!(entry.metadata.slot.slot, "2.7-r1");
    }

    #[test]
    fn from_kv_pairs() {
        let pairs = vec![
            ("EAPI", "8"),
            ("DESCRIPTION", "test package"),
            ("SLOT", "0"),
            ("KEYWORDS", "amd64"),
            ("DEFINED_PHASES", "-"),
        ];
        let entry = CacheEntry::from_kv_pairs(pairs.into_iter()).unwrap();
        assert_eq!(entry.metadata.eapi, Eapi::Eight);
        assert_eq!(entry.metadata.description, "test package");
        assert_eq!(entry.metadata.slot.slot, "0");
        assert!(entry.metadata.keywords.len() == 1);
    }

    #[test]
    fn raw_field_returns_single_value() {
        let raw = RawCacheEntry::new(EXAMPLE_CACHE);
        assert_eq!(
            raw.field("DESCRIPTION"),
            Some("Python bindings for sys-devel/clang")
        );
        assert_eq!(raw.field("EAPI"), Some("7"));
        assert_eq!(raw.field("_md5_"), Some("4539d849d3cea8ac84debad9b3154143"));
        assert_eq!(raw.field("NOPE"), None);
    }

    #[test]
    fn raw_fields_batch_one_pass() {
        let raw = RawCacheEntry::new(EXAMPLE_CACHE);
        let [desc, homepage, missing] = raw.fields(["DESCRIPTION", "HOMEPAGE", "MISSING"]);
        assert_eq!(desc, Some("Python bindings for sys-devel/clang"));
        assert_eq!(homepage, Some("https://llvm.org/"));
        assert_eq!(missing, None);
    }

    #[test]
    fn raw_lines_yields_every_pair_in_order() {
        let raw = RawCacheEntry::new("EAPI=8\nDESCRIPTION=hello\nSLOT=0\n");
        let pairs: Vec<(&str, &str)> = raw.lines().collect();
        assert_eq!(
            pairs,
            vec![("EAPI", "8"), ("DESCRIPTION", "hello"), ("SLOT", "0"),]
        );
    }

    #[test]
    fn raw_lines_skips_blank_and_malformed_lines() {
        let raw = RawCacheEntry::new("\nFOO=bar\nnoequals\n  \nBAZ=qux\n");
        let pairs: Vec<(&str, &str)> = raw.lines().collect();
        assert_eq!(pairs, vec![("FOO", "bar"), ("BAZ", "qux")]);
    }

    #[test]
    fn raw_value_with_embedded_equals_is_kept_intact() {
        // values may legally contain `=`; split_once gives us up to the first.
        let raw = RawCacheEntry::new("LICENSE=MIT\nDESCRIPTION=key=value pair\n");
        assert_eq!(raw.field("DESCRIPTION"), Some("key=value pair"));
    }

    #[test]
    fn raw_view_composes_with_full_parse() {
        // CacheEntry::parse_impl is just from_kv_pairs on top of
        // RawCacheEntry::lines, so the two layers must agree.
        let raw = RawCacheEntry::new(EXAMPLE_CACHE);
        let entry = CacheEntry::from_kv_pairs(raw.lines()).unwrap();
        assert_eq!(
            entry.metadata.description,
            raw.field("DESCRIPTION").unwrap()
        );
        assert_eq!(entry.md5.as_deref(), raw.field("_md5_"));
    }

    // H3: texlive-core's cache entry must parse without error.
    // If any field fails to parse, the entire entry is silently skipped
    // (let Ok(entry) = entry else { continue }) and ALL its transitive deps vanish.
    #[test]
    fn texlive_core_cache_entry_parses() {
        // Verbatim content from metadata/md5-cache/app-text/texlive-core-2024-r2.
        // Key concern: RDEPEND contains `:=[use,use]` combined slot+use atoms
        // and USE-conditional blocks — any parse failure silently drops the entry.
        let raw = "\
BDEPEND=sys-apps/ed sys-devel/flex virtual/pkgconfig >=app-portage/elt-patches-20250306
DEFINED_PHASES=configure install postinst postrm prepare setup
DESCRIPTION=A complete TeX distribution
EAPI=8
HOMEPAGE=https://tug.org/texlive/
IUSE=cjk X doc source tk +luajittex xetex xindy
KEYWORDS=~alpha ~amd64 ~arm ~arm64 ~hppa ~loong ~ppc ~ppc64 ~riscv ~s390 ~sparc ~x86
LICENSE=MIT
RDEPEND=X? ( x11-libs/libX11 x11-libs/libXmu ) sci-libs/mpfi virtual/zlib:= >=media-libs/harfbuzz-1.4.5:=[icu,graphite] >=media-libs/libpng-1.2.43-r2:0= media-libs/gd[png] media-gfx/graphite2:= media-gfx/potrace:= >=x11-libs/cairo-1.12 >=x11-libs/pixman-0.18 dev-libs/zziplib:= app-text/libpaper:= dev-libs/gmp:= dev-libs/mpfr:= >=dev-libs/ptexenc-1.4.6 xetex? ( >=app-text/teckit-2.5.10 media-libs/fontconfig ) xindy? ( dev-lisp/clisp:= ) media-libs/freetype:2 >=dev-libs/icu-50:= >=dev-libs/kpathsea-6.4.0:= virtual/perl-Getopt-Long dev-perl/File-HomeDir dev-perl/Log-Dispatch dev-perl/Unicode-LineBreak dev-perl/YAML-Tiny tk? ( dev-lang/tk dev-perl/Tk )
SLOT=0
SRC_URI=https://mirrors.ctan.org/systems/texlive/Source/texlive-20240311-source.tar.xz
_md5_=abc123";
        let entry = CacheEntry::parse(raw).unwrap_or_else(|e| {
            panic!("texlive-core cache entry failed to parse: {e}\n\nRaw:\n{raw}")
        });
        // Spot-check that RDEPEND was actually parsed.
        assert!(
            !entry.metadata.rdepend.list().is_empty(),
            "RDEPEND should not be empty"
        );
        assert_eq!(entry.metadata.iuse.len(), 8); // cjk X doc source tk luajittex xetex xindy
        assert!(
            entry
                .metadata
                .iuse
                .iter()
                .any(|iu| iu.name() == "luajittex"),
            "luajittex must be in IUSE"
        );
    }

    #[test]
    fn kpathsea_cache_entry_parses() {
        // Verbatim content from metadata/md5-cache/dev-libs/kpathsea-6.4.0_p20240311-r1.
        // Key concern: SLOT=0/6.4.0 (subslot) and ~x64-macos / ~x64-solaris keywords.
        let raw = "\
BDEPEND=>=app-portage/elt-patches-20250306
DEFINED_PHASES=configure install postinst postrm prepare
DESCRIPTION=Path searching library for TeX-related files
EAPI=8
IUSE=doc source static-libs
KEYWORDS=~alpha amd64 arm arm64 ~hppa ~loong ~mips ppc ppc64 ~riscv ~s390 ~sparc x86 ~x64-macos ~x64-solaris
LICENSE=LGPL-2.1
RDEPEND=virtual/tmpfiles
SLOT=0/6.4.0
SRC_URI=https://example.com/kpathsea.tar.xz
_md5_=abc123";
        let entry = CacheEntry::parse(raw)
            .unwrap_or_else(|e| panic!("kpathsea cache entry failed to parse: {e}\n\nRaw:\n{raw}"));
        // Subslot must be preserved.
        assert_eq!(entry.metadata.slot.slot.as_str(), "0");
        assert_eq!(
            entry.metadata.slot.subslot.map(|s| s.as_str().to_owned()),
            Some("6.4.0".to_string())
        );
        // Exotic keywords like ~x64-macos must NOT cause a parse error.
        assert!(
            entry.metadata.keywords.len() >= 10,
            "all keywords including ~x64-macos should parse"
        );
    }
}
