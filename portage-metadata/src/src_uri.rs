use std::fmt;

use portage_atom::Lazy;
use winnow::ascii::multispace0;
use winnow::combinator::{alt, cut_err, delimited, dispatch, opt, peek, preceded, repeat};
use winnow::error::StrContext;
use winnow::prelude::*;
use winnow::token::{any, take_while};

use crate::error::{Error, Result};

/// A single entry in a `SRC_URI` expression
///
/// `SRC_URI` specifies the source files needed to build a package. Entries
/// may be plain URIs, renamed URIs (EAPI 2+: `url -> filename`), or
/// USE-conditional groups. EAPI 8+ supports selective URI restrictions
/// with `fetch+` and `mirror+` prefixes.
///
/// See [PMS 7.3.2](https://projects.gentoo.org/pms/9/pms.html#srcuri)
/// and [PMS 8.2](https://projects.gentoo.org/pms/9/pms.html#dependency-specification-format).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrcUriEntry {
    /// A plain URI. The filename is derived from the last path component
    Uri {
        /// The download URL
        url: String,
        /// The target filename (last path component of the URL)
        filename: String,
        /// URI restriction prefix (EAPI 8+): `None`, `Some("fetch")`, or `Some("mirror")`
        restriction: Option<String>,
    },
    /// A renamed URI (EAPI 2+): `url -> target`
    Renamed {
        /// The download URL
        url: String,
        /// The local filename to save as
        target: String,
        /// URI restriction prefix (EAPI 8+): `None`, `Some("fetch")`, or `Some("mirror")`
        restriction: Option<String>,
    },
    /// `flag? ( entries... )` or `!flag? ( entries... )` conditional group
    UseConditional {
        /// USE flag name
        flag: String,
        /// `true` for `!flag?` (negated conditional)
        negated: bool,
        /// Entries guarded by this flag
        entries: Vec<SrcUriEntry>,
    },
    /// A bare parenthesized group `( entries... )`
    Group(Vec<SrcUriEntry>),
}

/// `SRC_URI`'s raw md5-cache text, parsed to `Vec<SrcUriEntry>` on first
/// access
///
/// Same rationale as `portage_atom::LazyDepList`, built on the same
/// [`Lazy`] primitive: only a small fraction of a repo's ebuilds (the ones
/// a plan actually merges) ever need their `SRC_URI` parsed —
/// download-size accounting and `em search` are the only consumers, both
/// over a small already-selected set, not the whole tree.
///
/// A malformed field parses to an empty list rather than propagating an
/// error, same tradeoff as `LazyDepList`. A wrapper, not a type alias like
/// `LazyDepList`: `Lazy` is defined in `portage-atom`, and Rust only allows
/// inherent `impl`s in the crate that defines the type — an alias here
/// wouldn't let this crate add `Self::list()`.
#[derive(Debug, Clone, Default)]
pub struct LazySrcUriList(Lazy<Vec<SrcUriEntry>>);

impl LazySrcUriList {
    /// Wrap raw md5-cache field text, parsed on first [`Self::list`] call
    pub fn from_raw(s: &str) -> Self {
        Self(Lazy::from_raw(s))
    }

    /// Whether the field is empty, without forcing the parse
    pub fn is_empty_raw(&self) -> bool {
        self.0.is_empty_raw()
    }

    /// The parsed list, computed and memoized on first call
    pub fn list(&self) -> &[SrcUriEntry] {
        self.0.get(|s| SrcUriEntry::parse(s).unwrap_or_default())
    }
}

impl PartialEq for LazySrcUriList {
    fn eq(&self, other: &Self) -> bool {
        self.list() == other.list()
    }
}

impl Eq for LazySrcUriList {}

impl SrcUriEntry {
    /// Parse a `SRC_URI` expression string into a list of entries
    ///
    /// # Examples
    ///
    /// ```
    /// use portage_metadata::SrcUriEntry;
    ///
    /// let entries = SrcUriEntry::parse(
    ///     "https://example.com/foo-1.0.tar.gz ssl? ( https://example.com/ssl.patch )"
    /// ).unwrap();
    /// assert_eq!(entries.len(), 2);
    /// ```
    pub fn parse(input: &str) -> Result<Vec<SrcUriEntry>> {
        parse_src_uri_string.parse(input).map_err(|e| {
            Error::InvalidSrcUri(crate::diagnostic::ParseDiagnostic::from_winnow(
                "SRC_URI", e,
            ))
        })
    }

    /// Append the distfile names this entry contributes for a given USE state
    ///
    /// `enabled(flag)` reports whether `flag` is enabled in the package's
    /// effective USE; `flag? ( … )` / `!flag? ( … )` groups are descended only
    /// when their guard is active. A plain URI contributes its derived
    /// filename, a renamed URI its target. Callers typically dedup the result
    /// (the same distfile may be referenced more than once).
    pub fn collect_filenames(&self, enabled: &dyn Fn(&str) -> bool, out: &mut Vec<String>) {
        match self {
            SrcUriEntry::Uri { filename, .. } => out.push(filename.clone()),
            SrcUriEntry::Renamed { target, .. } => out.push(target.clone()),
            SrcUriEntry::UseConditional {
                flag,
                negated,
                entries,
            } => {
                if enabled(flag) != *negated {
                    for e in entries {
                        e.collect_filenames(enabled, out);
                    }
                }
            }
            SrcUriEntry::Group(entries) => {
                for e in entries {
                    e.collect_filenames(enabled, out);
                }
            }
        }
    }
}

/// Extract filename from a URL (last path component)
fn filename_from_url(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or(url)
        .to_string()
}

impl fmt::Display for SrcUriEntry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SrcUriEntry::Uri {
                url, restriction, ..
            } => {
                if let Some(prefix) = restriction {
                    write!(f, "{prefix}+")?;
                }
                write!(f, "{url}")
            }
            SrcUriEntry::Renamed {
                url,
                target,
                restriction,
            } => {
                if let Some(prefix) = restriction {
                    write!(f, "{prefix}+")?;
                }
                write!(f, "{url} -> {target}")
            }
            SrcUriEntry::UseConditional {
                flag,
                negated,
                entries,
            } => {
                if *negated {
                    write!(f, "!")?;
                }
                write!(f, "{flag}? ( ")?;
                for (i, entry) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{entry}")?;
                }
                write!(f, " )")
            }
            SrcUriEntry::Group(entries) => {
                write!(f, "( ")?;
                for (i, entry) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{entry}")?;
                }
                write!(f, " )")
            }
        }
    }
}

// Winnow parsers

fn is_uri_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            ':' | '/'
                | '.'
                | '-'
                | '_'
                | '~'
                | '$'
                | '&'
                | '\''
                | '*'
                | '+'
                | ','
                | ';'
                | '='
                | '%'
                | '@'
                | '#'
                | '?'
                | '!'   // RFC 3986 sub-delim; go-module.eclass mirror:// URIs use it
                        // to escape uppercase letters (e.g. github.com/!puerkito!bio/...)
                | '['   // used in legacy Debian-mirror URLs (e.g. vdr-calc-0[1].0.1-rc5.tgz)
                | ']'
        )
}

fn is_filename_char(c: char) -> bool {
    // '{' and '}' are permitted for rename targets where the ebuild author
    // wrote {P} instead of ${P}; portage accepts such filenames in practice.
    // '@' appears in real rename targets (e.g. sec-keys/openpgp-keys-kernel
    // uses `-> gregkh@kernel.org.key`). '!' and '%' appear in go-module.eclass
    // generated rename targets (percent-encoded Go module paths with escaped
    // uppercase letters, e.g. `-> github.com%2F!puerkito!bio%2Fgoquery%2F...`).
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+' | '{' | '}' | '@' | '!' | '%')
}

fn is_flag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+' || c == '@'
}

fn parse_uri(input: &mut &str) -> ModalResult<String> {
    take_while(1.., is_uri_char)
        .verify(|s: &str| !s.starts_with('@'))
        .map(|s: &str| s.to_string())
        .parse_next(input)
}

fn parse_restriction_prefix(input: &mut &str) -> ModalResult<Option<String>> {
    opt(alt((
        "fetch+".map(|_| "fetch".to_string()),
        "mirror+".map(|_| "mirror".to_string()),
    )))
    .parse_next(input)
}

fn parse_filename(input: &mut &str) -> ModalResult<String> {
    take_while(1.., is_filename_char)
        .map(|s: &str| s.to_string())
        .parse_next(input)
}

/// Parse a single URI, optionally followed by `-> filename`
fn parse_uri_entry(input: &mut &str) -> ModalResult<SrcUriEntry> {
    (
        parse_restriction_prefix,
        parse_uri,
        opt(preceded((multispace0, "->", multispace0), parse_filename)),
    )
        .map(|(restriction, url, rename)| {
            if let Some(target) = rename {
                SrcUriEntry::Renamed {
                    url,
                    target,
                    restriction,
                }
            } else {
                let filename = filename_from_url(&url);
                SrcUriEntry::Uri {
                    url,
                    filename,
                    restriction,
                }
            }
        })
        .parse_next(input)
}

/// Parse `[!]flag? ( entries... )`
fn parse_use_conditional(input: &mut &str) -> ModalResult<SrcUriEntry> {
    let negated = opt('!').parse_next(input)?.is_some();
    let flag: String = take_while(1.., is_flag_char)
        .verify(|name: &str| {
            name.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
        })
        .map(|s: &str| s.to_string())
        .parse_next(input)?;
    '?'.parse_next(input)?;
    multispace0.parse_next(input)?;
    let entries = cut_err(delimited('(', parse_src_uri_entries, (multispace0, ')')))
        .context(StrContext::Label("USE conditional group"))
        .parse_next(input)?;
    Ok(SrcUriEntry::UseConditional {
        flag,
        negated,
        entries,
    })
}

/// Parse `( entries... )` — bare parenthesized group
fn parse_group(input: &mut &str) -> ModalResult<SrcUriEntry> {
    delimited(
        '(',
        parse_src_uri_entries,
        cut_err((multispace0, ')')).context(StrContext::Label("closing ')'")),
    )
    .map(SrcUriEntry::Group)
    .parse_next(input)
}

/// Parse a single SRC_URI entry
fn parse_src_uri_entry(input: &mut &str) -> ModalResult<SrcUriEntry> {
    dispatch! {peek(any);
        '(' => parse_group,
        _ => alt((
            parse_use_conditional,
            parse_uri_entry,
        )),
    }
    .parse_next(input)
}

/// Parse zero or more SRC_URI entries separated by whitespace
fn parse_src_uri_entries(input: &mut &str) -> ModalResult<Vec<SrcUriEntry>> {
    repeat(0.., preceded(multispace0, parse_src_uri_entry)).parse_next(input)
}

/// Parse a complete SRC_URI string
pub(crate) fn parse_src_uri_string(input: &mut &str) -> ModalResult<Vec<SrcUriEntry>> {
    let entries = parse_src_uri_entries(input)?;
    multispace0.parse_next(input)?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_set(flags: &[&str]) -> impl Fn(&str) -> bool {
        let set: std::collections::HashSet<String> = flags.iter().map(|s| s.to_string()).collect();
        move |f: &str| set.contains(f)
    }

    fn filenames(input: &str, on: &[&str]) -> Vec<String> {
        let entries = SrcUriEntry::parse(input).unwrap();
        let pred = enabled_set(on);
        let mut out = Vec::new();
        for e in &entries {
            e.collect_filenames(&pred, &mut out);
        }
        out
    }

    #[test]
    fn collect_filenames_plain_and_renamed() {
        assert_eq!(
            filenames(
                "https://e.com/foo-1.0.tar.gz https://e.com/x.tar.xz -> bar-1.0.tar.xz",
                &[]
            ),
            vec!["foo-1.0.tar.gz".to_string(), "bar-1.0.tar.xz".to_string()]
        );
    }

    #[test]
    fn collect_filenames_use_conditional() {
        let src = "base.tar.gz ssl? ( https://e.com/ssl-patch.tar.xz )";
        assert_eq!(filenames(src, &[]), vec!["base.tar.gz".to_string()]);
        assert_eq!(
            filenames(src, &["ssl"]),
            vec!["base.tar.gz".to_string(), "ssl-patch.tar.xz".to_string()]
        );
    }

    #[test]
    fn parse_single_uri() {
        let entries = SrcUriEntry::parse("https://example.com/foo-1.0.tar.gz").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Uri {
                url,
                filename,
                restriction,
            } => {
                assert_eq!(url, "https://example.com/foo-1.0.tar.gz");
                assert_eq!(filename, "foo-1.0.tar.gz");
                assert_eq!(restriction, &None);
            }
            _ => unreachable!("expected Uri"),
        }
    }

    #[test]
    fn parse_renamed_uri() {
        let entries =
            SrcUriEntry::parse("https://github.com/archive/v1.0.tar.gz -> foo-1.0.tar.gz").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Renamed {
                url,
                target,
                restriction,
            } => {
                assert_eq!(url, "https://github.com/archive/v1.0.tar.gz");
                assert_eq!(target, "foo-1.0.tar.gz");
                assert_eq!(restriction, &None);
            }
            _ => unreachable!("expected Renamed"),
        }
    }

    #[test]
    fn parse_fetch_restricted_uri() {
        let entries = SrcUriEntry::parse("fetch+https://example.com/foo-1.0.tar.gz").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Uri {
                url, restriction, ..
            } => {
                assert_eq!(url, "https://example.com/foo-1.0.tar.gz");
                assert_eq!(restriction, &Some("fetch".to_string()));
            }
            _ => unreachable!("expected Uri"),
        }
    }

    #[test]
    fn parse_mirror_restricted_uri() {
        let entries = SrcUriEntry::parse("mirror+https://example.com/foo-1.0.tar.gz").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Uri {
                url, restriction, ..
            } => {
                assert_eq!(url, "https://example.com/foo-1.0.tar.gz");
                assert_eq!(restriction, &Some("mirror".to_string()));
            }
            _ => unreachable!("expected Uri"),
        }
    }

    #[test]
    fn parse_restricted_renamed_uri() {
        let entries =
            SrcUriEntry::parse("fetch+https://github.com/archive/v1.0.tar.gz -> foo-1.0.tar.gz")
                .unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Renamed {
                url,
                target,
                restriction,
            } => {
                assert_eq!(url, "https://github.com/archive/v1.0.tar.gz");
                assert_eq!(target, "foo-1.0.tar.gz");
                assert_eq!(restriction, &Some("fetch".to_string()));
            }
            _ => unreachable!("expected Renamed"),
        }
    }

    #[test]
    fn parse_use_conditional() {
        let entries = SrcUriEntry::parse("ssl? ( https://example.com/ssl.patch )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::UseConditional {
                flag,
                negated,
                entries,
            } => {
                assert_eq!(flag, "ssl");
                assert!(!negated);
                assert_eq!(entries.len(), 1);
            }
            _ => unreachable!("expected UseConditional"),
        }
    }

    #[test]
    fn parse_negated_conditional() {
        let entries = SrcUriEntry::parse("!doc? ( https://example.com/minimal.tar.gz )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::UseConditional { flag, negated, .. } => {
                assert_eq!(flag, "doc");
                assert!(negated);
            }
            _ => unreachable!("expected UseConditional"),
        }
    }

    #[test]
    fn parse_multiple_uris() {
        let entries =
            SrcUriEntry::parse("https://example.com/a.tar.gz https://example.com/b.tar.gz")
                .unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn parse_mixed() {
        let entries = SrcUriEntry::parse(
            "https://example.com/src.tar.gz ssl? ( https://example.com/ssl.patch )",
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0], SrcUriEntry::Uri { .. }));
        assert!(matches!(&entries[1], SrcUriEntry::UseConditional { .. }));
    }

    #[test]
    fn parse_empty() {
        let entries = SrcUriEntry::parse("").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn display_uri() {
        let entry = SrcUriEntry::Uri {
            url: "https://example.com/foo.tar.gz".to_string(),
            filename: "foo.tar.gz".to_string(),
            restriction: None,
        };
        assert_eq!(entry.to_string(), "https://example.com/foo.tar.gz");
    }

    #[test]
    fn display_renamed() {
        let entry = SrcUriEntry::Renamed {
            url: "https://example.com/v1.tar.gz".to_string(),
            target: "foo-1.tar.gz".to_string(),
            restriction: None,
        };
        assert_eq!(
            entry.to_string(),
            "https://example.com/v1.tar.gz -> foo-1.tar.gz"
        );
    }

    #[test]
    fn real_world_src_uri() {
        let input = "https://github.com/llvm/llvm-project/archive/llvmorg-10.0.0-rc1.tar.gz";
        let entries = SrcUriEntry::parse(input).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Uri { filename, .. } => {
                assert_eq!(filename, "llvmorg-10.0.0-rc1.tar.gz");
            }
            _ => unreachable!("expected Uri"),
        }
    }

    #[test]
    fn display_restricted_uri() {
        let entries = SrcUriEntry::parse("fetch+https://example.com/foo.tar.gz").unwrap();
        let displayed = entries[0].to_string();
        assert_eq!(displayed, "fetch+https://example.com/foo.tar.gz");
    }

    #[test]
    fn display_restricted_renamed_uri() {
        let entries =
            SrcUriEntry::parse("mirror+https://example.com/foo.tar.gz -> bar.tar.gz").unwrap();
        let displayed = entries[0].to_string();
        assert_eq!(
            displayed,
            "mirror+https://example.com/foo.tar.gz -> bar.tar.gz"
        );
    }

    #[test]
    fn uri_with_brackets_in_filename() {
        // Legacy Debian-mirror URLs sometimes embed a revision in brackets,
        // e.g. vdr-calc-0[1].0.1-rc5.tgz.
        let entries = SrcUriEntry::parse(
            "http://vdr.websitec.de/download/vdr-calc/vdr-calc-0[1].0.1-rc5.tgz",
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Uri { filename, .. } => {
                assert_eq!(filename, "vdr-calc-0[1].0.1-rc5.tgz");
            }
            _ => unreachable!("expected Uri"),
        }
    }

    // ── Real-world tests sourced from the Gentoo tree ──────────────────

    #[test]
    fn mirror_uri_gnu() {
        // sys-libs/glibc-2.38-r13
        let entries = SrcUriEntry::parse("mirror://gnu/glibc/glibc-2.38.tar.xz").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Uri { url, filename, .. } => {
                assert_eq!(url, "mirror://gnu/glibc/glibc-2.38.tar.xz");
                assert_eq!(filename, "glibc-2.38.tar.xz");
            }
            _ => unreachable!("expected Uri"),
        }
    }

    #[test]
    fn mirror_uri_debian() {
        // x11-plugins/asclock-2.0.12-r5
        let entries =
            SrcUriEntry::parse("mirror://debian/pool/main/a/asclock/asclock_2.0.12.orig.tar.gz")
                .unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Uri { url, filename, .. } => {
                assert_eq!(
                    url,
                    "mirror://debian/pool/main/a/asclock/asclock_2.0.12.orig.tar.gz"
                );
                assert_eq!(filename, "asclock_2.0.12.orig.tar.gz");
            }
            _ => unreachable!("expected Uri"),
        }
    }

    #[test]
    fn bare_filename_no_protocol() {
        // sci-chemistry/vmd-1.9.4_alpha57-r4
        let entries = SrcUriEntry::parse(
            "vmd-1.9.4a57.src.tar.gz fetch+https://dev.gentoo.org/~pacho/vmd/vmd-1.9.4_alpha57-gentoo-patches.tar.xz",
        ).unwrap();
        assert_eq!(entries.len(), 2);
        match &entries[0] {
            SrcUriEntry::Uri {
                url,
                filename,
                restriction,
            } => {
                assert_eq!(url, "vmd-1.9.4a57.src.tar.gz");
                assert_eq!(filename, "vmd-1.9.4a57.src.tar.gz");
                assert_eq!(restriction, &None);
            }
            _ => unreachable!("expected Uri"),
        }
        match &entries[1] {
            SrcUriEntry::Uri {
                url, restriction, ..
            } => {
                assert_eq!(
                    url,
                    "https://dev.gentoo.org/~pacho/vmd/vmd-1.9.4_alpha57-gentoo-patches.tar.xz"
                );
                assert_eq!(restriction, &Some("fetch".to_string()));
            }
            _ => unreachable!("expected Uri"),
        }
    }

    #[test]
    fn nested_use_conditionals_stellarium() {
        // sci-astronomy/stellarium-25.4 (simplified)
        let input = "https://example.com/stellarium-25.4.tar.xz \
                      deep-sky? ( https://example.com/catalog-3.22.dat -> stellarium-dso-catalog-3.22.dat \
                      verify-sig? ( https://example.com/catalog-3.22.dat.asc -> stellarium-dso-catalog-3.22.dat.asc ) )";
        let entries = SrcUriEntry::parse(input).unwrap();
        assert_eq!(entries.len(), 2);
        match &entries[1] {
            SrcUriEntry::UseConditional {
                flag,
                negated,
                entries: inner,
            } => {
                assert_eq!(flag, "deep-sky");
                assert!(!negated);
                assert_eq!(inner.len(), 2);
                // The second inner entry is itself a nested conditional
                match &inner[1] {
                    SrcUriEntry::UseConditional {
                        flag,
                        negated,
                        entries: nested,
                    } => {
                        assert_eq!(flag, "verify-sig");
                        assert!(!negated);
                        assert_eq!(nested.len(), 1);
                    }
                    _ => unreachable!("expected nested UseConditional"),
                }
            }
            _ => unreachable!("expected UseConditional"),
        }
    }

    #[test]
    fn nested_negated_conditional_culmus() {
        // media-fonts/culmus-0.120-r6 (simplified)
        let input = "ancient? ( !fontforge? ( https://example.com/AncientSemiticFonts.tgz ) \
             fontforge? ( https://example.com/AncientSemiticFonts-src.tgz ) )";
        let entries = SrcUriEntry::parse(input).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::UseConditional {
                flag,
                entries: inner,
                ..
            } => {
                assert_eq!(flag, "ancient");
                assert_eq!(inner.len(), 2);
                match &inner[0] {
                    SrcUriEntry::UseConditional { flag, negated, .. } => {
                        assert_eq!(flag, "fontforge");
                        assert!(negated);
                    }
                    _ => unreachable!("expected negated UseConditional"),
                }
            }
            _ => unreachable!("expected UseConditional"),
        }
    }

    #[test]
    fn url_encoded_chars_in_uri() {
        // games-arcade/opensonic-0.1.4-r4
        let entries = SrcUriEntry::parse(
            "https://downloads.sourceforge.net/project/opensnc/Open%20Sonic/0.1.4/opensnc-src-0.1.4.tar.gz",
        ).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Uri { url, filename, .. } => {
                assert_eq!(
                    url,
                    "https://downloads.sourceforge.net/project/opensnc/Open%20Sonic/0.1.4/opensnc-src-0.1.4.tar.gz"
                );
                assert_eq!(filename, "opensnc-src-0.1.4.tar.gz");
            }
            _ => unreachable!("expected Uri"),
        }
    }

    #[test]
    fn uri_with_query_string_and_rename() {
        // sec-keys/openpgp-keys-dwmw2-20230504
        let entries = SrcUriEntry::parse(
            "https://kernel.org/.well-known/openpgpkey/hu/163ux8fk184q7f9reyj4huqggwnwb6w7?l=dwmw2 -> dwmw2@kernel.org.key",
        ).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Renamed { url, target, .. } => {
                assert_eq!(
                    url,
                    "https://kernel.org/.well-known/openpgpkey/hu/163ux8fk184q7f9reyj4huqggwnwb6w7?l=dwmw2"
                );
                assert_eq!(target, "dwmw2@kernel.org.key");
            }
            _ => unreachable!("expected Renamed"),
        }
    }

    #[test]
    fn rename_target_with_at_sign() {
        // sec-keys/openpgp-keys-thomasdickey-20260204
        let entries = SrcUriEntry::parse(
            "https://invisible-island.net/public/dickey@invisible-island.net-rsa3072.asc -> openpgp-keys-thomasdickey-20260204-dickey@invisible-island.net-rsa3072.asc",
        ).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Renamed { target, .. } => {
                assert_eq!(
                    target,
                    "openpgp-keys-thomasdickey-20260204-dickey@invisible-island.net-rsa3072.asc"
                );
            }
            _ => unreachable!("expected Renamed"),
        }
    }

    #[test]
    fn multiple_rename_targets_with_at_sign() {
        // sec-keys/openpgp-keys-kernel-20250702
        let input = "https://kernel.org/.well-known/openpgpkey/hu/e3n9xnm94c5apezqnj1pmrfuaoyfm8cf?l=gregkh -> gregkh@kernel.org.key \
                      https://kernel.org/.well-known/openpgpkey/hu/pf113mfnx1f3eb1yiwhsipa91xfc7o4x?l=torvalds -> torvalds@kernel.org.key";
        let entries = SrcUriEntry::parse(input).unwrap();
        assert_eq!(entries.len(), 2);
        match &entries[0] {
            SrcUriEntry::Renamed { target, .. } => {
                assert_eq!(target, "gregkh@kernel.org.key");
            }
            _ => unreachable!("expected Renamed"),
        }
        match &entries[1] {
            SrcUriEntry::Renamed { target, .. } => {
                assert_eq!(target, "torvalds@kernel.org.key");
            }
            _ => unreachable!("expected Renamed"),
        }
    }

    #[test]
    fn real_world_mirror_plus_prefix() {
        // games-arcade/opensonic-0.1.4-r4
        let entries = SrcUriEntry::parse(
            "https://downloads.sourceforge.net/project/opensnc/Open%20Sonic/0.1.4/opensnc-src-0.1.4.tar.gz mirror+https://dev.gentoo.org/~ionen/distfiles/loggcompat-4.4.2.tar.gz",
        ).unwrap();
        assert_eq!(entries.len(), 2);
        match &entries[1] {
            SrcUriEntry::Uri {
                url,
                filename,
                restriction,
            } => {
                assert_eq!(
                    url,
                    "https://dev.gentoo.org/~ionen/distfiles/loggcompat-4.4.2.tar.gz"
                );
                assert_eq!(filename, "loggcompat-4.4.2.tar.gz");
                assert_eq!(restriction, &Some("mirror".to_string()));
            }
            _ => unreachable!("expected Uri"),
        }
    }

    #[test]
    fn use_conditional_flag_with_at_sign() {
        // python_targets_python3_11@std is a real-world flag name pattern
        let entries =
            SrcUriEntry::parse("python_targets_python3_11@std? ( https://example.com/foo.tar.gz )")
                .unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::UseConditional { flag, negated, .. } => {
                assert_eq!(flag, "python_targets_python3_11@std");
                assert!(!negated);
            }
            _ => unreachable!("expected UseConditional"),
        }
    }

    #[test]
    fn invalid_use_conditional_flag_starting_with_at() {
        assert!(SrcUriEntry::parse("@flag? ( https://example.com/foo.tar.gz )").is_err());
    }

    #[test]
    fn rename_target_with_braces() {
        // Ebuilds occasionally write {P} instead of ${P} in rename targets;
        // portage accepts such filenames, so we should too.
        let entries = SrcUriEntry::parse(
            "https://github.com/SmallLars/openssl-ccm/archive/refs/tags/1.3.0.tar.gz -> {P}.tar.gz",
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Renamed { target, .. } => {
                assert_eq!(target, "{P}.tar.gz");
            }
            _ => unreachable!("expected Renamed"),
        }
    }

    #[test]
    fn go_module_mirror_uri_with_bang_escapes() {
        // go-module.eclass's generated EGO_SUM_SRC_URI escapes uppercase
        // letters in Go import paths as `!lowercase` (pentoo's
        // dev-go/fetchbot-1.3, live
        // sub-delim, legal unencoded in a URI.
        let entries = SrcUriEntry::parse(
            "mirror://goproxy//github.com/!puerkito!bio/goquery/@v/v1.5.1.zip -> github.com%2F!puerkito!bio%2Fgoquery%2F@v%2Fv1.5.1.zip",
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            SrcUriEntry::Renamed { url, target, .. } => {
                assert_eq!(
                    url,
                    "mirror://goproxy//github.com/!puerkito!bio/goquery/@v/v1.5.1.zip"
                );
                assert_eq!(
                    target,
                    "github.com%2F!puerkito!bio%2Fgoquery%2F@v%2Fv1.5.1.zip"
                );
            }
            _ => unreachable!("expected Renamed"),
        }
    }

    #[test]
    fn uri_with_literal_braces_from_an_unexpanded_variable_is_rejected() {
        // pentoo's dev-util/kaleido-bin-0.2.1 has a `v{$PV}` typo (meant
        // `v${PV}`); bash leaves the braces literal since `{$PV}` isn't a
        // brace-expansion pattern. Deliberately still an error: this is a
        // genuine upstream ebuild bug, not a real-world convention worth
        // tolerating (unlike '{'/'}' in *rename targets*, which stays
        // permitted above — that pattern is a deliberate ebuild-author
        // shorthand, not a bug).
        assert!(
            SrcUriEntry::parse(
                "https://example.com/releases/download/v{0.2.1}/kaleido_linux_arm64.zip -> kaleido-bin-0.2.1.zip",
            )
            .is_err()
        );
    }

    #[test]
    fn plain_display_is_a_short_headline_not_the_whole_code_frame() {
        // The `Error`/`ParseDiagnostic` `Display` is the safe default for
        // ordinary log lines and `anyhow` chains: a short one-line summary,
        // not a multi-line report. The full code frame is opt-in via a
        // miette handler at the UI boundary — see the next test.
        let err = SrcUriEntry::parse("amd64? ( v{oops}/bad.tar.gz )")
            .unwrap_err()
            .to_string();
        assert_eq!(err, "invalid SRC_URI");
    }

    #[test]
    fn parse_diagnostic_render_is_a_readable_miette_report_not_a_bare_caret_line() {
        // The actual pentoo dev-util/kaleido-bin-0.2.1 SRC_URI: winnow's own
        // Display would put the caret under a ~230-char single line, far
        // enough right it's invisible without horizontal scrolling.
        // `render_nocolor` must instead show a short code frame with the
        // caret directly under the offending '{' and a label.
        let err = SrcUriEntry::parse(
            "amd64? ( https://github.com/plotly/Kaleido/releases/download/v0.2.1/kaleido_linux_x64.zip -> kaleido-bin-0.2.1.zip ) arm64? ( https://github.com/plotly/Kaleido/releases/download/v{0.2.1}/kaleido_linux_arm64.zip -> kaleido-bin-0.2.1.zip )",
        )
        .unwrap_err();
        let rendered = err.parse_diagnostic().unwrap().render_nocolor();
        assert!(rendered.contains("invalid SRC_URI"));
        assert!(rendered.contains('{'));
        // The message is a short code frame, not the ~230-char raw source.
        assert!(rendered.lines().all(|l| l.chars().count() < 200));
        assert!(!rendered.contains('\u{1b}'));
    }

    #[test]
    fn parse_error_on_a_very_long_source_crops_a_window_around_the_span() {
        // go-module.eclass's generated SRC_URI can run to a few thousand
        // characters; without windowing, the "source" miette shows is the
        // entire value as one unbroken line — the caret ends up thousands
        // of columns right of anything a terminal shows. Regression for
        // `diagnostic::windowed`.
        let mut src = String::new();
        for i in 0..40 {
            src.push_str(&format!("https://example.com/mirror{i}/file{i}.tar.gz "));
        }
        src.push_str("v{oops}/bad.tar.gz");
        assert!(src.len() > 500);
        let err = SrcUriEntry::parse(&src).unwrap_err();
        let rendered = err.parse_diagnostic().unwrap().render_nocolor();
        assert!(rendered.contains('{'));
        assert!(rendered.lines().all(|l| l.chars().count() < 200));
    }

    // The underlying laziness mechanics (parses on first access, memoizes,
    // clone preserves already-computed state) are `Lazy<T>`'s own tests in
    // `portage-atom`'s `lazy.rs` — these cover only what's specific to
    // `SrcUriEntry` parsing.

    #[test]
    fn lazy_src_uri_parses_a_plain_uri() {
        let lazy = LazySrcUriList::from_raw("https://example.com/foo-1.0.tar.gz");
        assert!(!lazy.is_empty_raw());
        assert_eq!(lazy.list().len(), 1);
    }

    #[test]
    fn lazy_src_uri_empty_raw_never_forces() {
        let lazy = LazySrcUriList::from_raw("");
        assert!(lazy.is_empty_raw());
    }

    #[test]
    fn lazy_src_uri_malformed_field_is_empty_not_an_error() {
        let lazy = LazySrcUriList::from_raw("v{oops}/bad.tar.gz");
        assert!(lazy.list().is_empty());
    }
}
