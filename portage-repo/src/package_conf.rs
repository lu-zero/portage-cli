//! Parser and editor for `/etc/portage/package.*` configuration files
//!
//! These files share a common line-oriented format:
//!
//! ```text
//! # comment
//! atom [value value ...]
//! ```
//!
//! where `atom` is a PMS dependency atom and `value` is any whitespace-free
//! token (USE flag, keyword, licence, env-file name, etc.).  The path may be
//! either a single file or a directory; in the directory case every regular
//! file is loaded in alphabetical order (dotfiles and `~` backups skipped —
//! [`crate::list_config_files`] / [`crate::ConfigFilesMode::Flat`]).
//!
//! Inline comments after values are preserved verbatim.  Round-trip
//! serialisation is byte-identical to the input when no edits are made.

use std::ops::Range;

use camino::{Utf8Path, Utf8PathBuf};
use winnow::ascii::space0;
use winnow::combinator::{alt, opt};
use winnow::error::ContextError;
use winnow::prelude::*;
use winnow::stream::{LocatingSlice, Location};
use winnow::token::{take_till, take_while};

use portage_atom::Dep;

use crate::{Error, Result};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A parsed `/etc/portage/package.*` file (or directory of files)
pub struct PackageConf {
    /// Source text — all files concatenated if loaded from a directory
    src: String,
    entries: Vec<Entry>,
}

/// One entry in the tiled decomposition of the source text
enum Entry {
    /// A comment, blank, or unparseable line
    ///
    /// Preserved verbatim in `src` on serialisation.
    Opaque(Range<usize>),
    /// A `atom [value ...]` line
    Data {
        /// Full byte span in `src` including the trailing `\n`
        span: Range<usize>,
        /// Byte span of the atom token within `src`
        atom_span: Range<usize>,
        /// Parsed atom
        ///
        /// Boxed: `Dep` carries a `Version` (grown by the `SmallVec`/
        /// `SmolStr` perf changes), which otherwise made this variant far
        /// larger than `Opaque` (clippy's `large_enum_variant`).
        atom: Box<Dep>,
        /// Each value token (byte span + text)
        values: Vec<Token>,
    },
}

/// A single whitespace-delimited token in a data line
#[derive(Debug, Clone)]
pub struct Token {
    /// Byte span in the owning `PackageConf::src`
    pub span: Range<usize>,
    /// Token text (borrowed from source)
    pub text: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

impl PackageConf {
    /// Load a single file
    pub fn load_file(path: &Utf8Path) -> Result<Self> {
        let src = std::fs::read_to_string(path).map_err(|e| Error::Io {
            path: path.to_path_buf().into_std_path_buf(),
            source: e,
        })?;
        Self::parse(src)
    }

    /// Load a directory, returning one `PackageConf` per file in alphabetical order
    pub fn load_dir(path: &Utf8Path) -> Result<Vec<(Utf8PathBuf, PackageConf)>> {
        use crate::repo::util::{ConfigFilesMode, list_config_files};

        list_config_files(path.as_std_path(), ConfigFilesMode::Flat)?
            .into_iter()
            .map(|f| {
                let f = Utf8PathBuf::try_from(f).map_err(|e| Error::Io {
                    path: e.into_path_buf(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "non-utf8 config path",
                    ),
                })?;
                let pc = Self::load_file(&f)?;
                Ok((f, pc))
            })
            .collect()
    }

    /// Load from a file path or a directory of files
    ///
    /// Directory entries are read in alphabetical order and concatenated
    /// ([`ConfigFilesMode::Flat`](crate::ConfigFilesMode)).
    /// Use `load_dir` when you need per-file identity for editing.
    pub fn load(path: &Utf8Path) -> Result<Self> {
        use crate::repo::util::{ConfigFilesMode, list_config_files};

        let files = list_config_files(path.as_std_path(), ConfigFilesMode::Flat)?;
        if files.is_empty() && !path.exists() {
            return Self::load_file(path); // preserve previous missing-file error
        }
        if files.len() == 1 && files[0] == path.as_std_path() {
            return Self::load_file(path);
        }
        let mut combined = String::new();
        for f in files {
            let chunk =
                std::fs::read_to_string(&f).map_err(|e| Error::Io { path: f, source: e })?;
            combined.push_str(&chunk);
        }
        Self::parse(combined)
    }

    /// Parse from an owned string
    pub fn parse(src: String) -> Result<Self> {
        let entries = parse_entries(&src);
        Ok(Self { src, entries })
    }

    /// Iterate over all data entries — skips comments and blank lines
    pub fn entries(&self) -> impl Iterator<Item = EntryRef<'_>> {
        self.entries.iter().filter_map(|e| {
            if let Entry::Data {
                atom,
                values,
                atom_span,
                ..
            } = e
            {
                Some(EntryRef {
                    src: &self.src,
                    atom,
                    atom_span: atom_span.clone(),
                    values,
                })
            } else {
                None
            }
        })
    }

    /// Find the first data entry whose CPN matches `atom`
    pub fn find(&self, atom: &Dep) -> Option<EntryRef<'_>> {
        self.entries().find(|e| e.atom.cpn == atom.cpn)
    }

    /// Return all data entries whose CPN matches `atom`
    ///
    /// Use this instead of `find` when a package may appear multiple times
    /// with different version constraints (e.g. slot-pinned keyword entries).
    pub fn find_all<'a>(&'a self, atom: &'a Dep) -> impl Iterator<Item = EntryRef<'a>> {
        self.entries().filter(move |e| e.atom.cpn == atom.cpn)
    }

    /// Add or update a data line for `atom`
    ///
    /// If an entry for `atom` (by CPN) already exists, its value list is
    /// replaced.  Otherwise a new line is appended.
    pub fn set(&mut self, atom: &Dep, values: &[&str]) {
        let new_line = format!("{} {}\n", atom, values.join(" "));

        let existing = self
            .entries
            .iter()
            .position(|e| matches!(e, Entry::Data { atom: a, .. } if a.cpn == atom.cpn));

        match existing {
            Some(idx) => {
                let Entry::Data { span, .. } = &self.entries[idx] else {
                    unreachable!()
                };
                let span = span.clone();
                self.src.replace_range(span, &new_line);
                self.rebuild();
            }
            None => {
                if !self.src.ends_with('\n') && !self.src.is_empty() {
                    self.src.push('\n');
                }
                self.src.push_str(&new_line);
                self.rebuild();
            }
        }
    }

    /// Remove the data line for `atom` (matched by CPN)
    pub fn remove(&mut self, atom: &Dep) -> bool {
        let existing = self
            .entries
            .iter()
            .position(|e| matches!(e, Entry::Data { atom: a, .. } if a.cpn == atom.cpn));
        if let Some(idx) = existing {
            let Entry::Data { span, .. } = &self.entries[idx] else {
                unreachable!()
            };
            let span = span.clone();
            self.src.replace_range(span, "");
            self.rebuild();
            true
        } else {
            false
        }
    }

    /// Save to a file
    pub fn save(&self, path: &Utf8Path) -> Result<()> {
        std::fs::write(path, &self.src).map_err(|e| Error::Io {
            path: path.to_path_buf().into_std_path_buf(),
            source: e,
        })
    }

    fn rebuild(&mut self) {
        self.entries = parse_entries(&self.src);
    }
}

/// A view of a single data entry within a [`PackageConf`]
pub struct EntryRef<'a> {
    src: &'a str,
    /// The parsed atom
    pub atom: &'a Dep,
    atom_span: Range<usize>,
    values: &'a [Token],
}

impl<'a> EntryRef<'a> {
    /// The atom as it appears in the source text
    pub fn atom_raw(&self) -> &'a str {
        &self.src[self.atom_span.clone()]
    }

    /// Iterate over value tokens
    pub fn values(&self) -> impl Iterator<Item = &'a str> {
        self.values.iter().map(|t| t.text.as_str())
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

type Stream<'a> = LocatingSlice<&'a str>;

fn parse_entries(src: &str) -> Vec<Entry> {
    // We drive a cursor manually so LocatingSlice byte offsets stay absolute.
    let mut cursor = 0usize;
    let mut entries = Vec::new();

    while cursor < src.len() {
        let slice = &src[cursor..];
        let mut stream = LocatingSlice::new(slice);

        if let Ok(entry) = data_line(&mut stream) {
            let consumed = slice.len() - stream.as_ref().len();
            entries.push(Entry::Data {
                span: cursor..cursor + consumed,
                atom_span: cursor + entry.atom_span.start..cursor + entry.atom_span.end,
                atom: Box::new(entry.atom),
                values: entry
                    .values
                    .into_iter()
                    .map(|t| Token {
                        span: cursor + t.span.start..cursor + t.span.end,
                        text: t.text,
                    })
                    .collect(),
            });
            cursor += consumed;
            continue;
        }

        // Opaque: consume to end of line.
        let end = match slice.find('\n') {
            Some(i) => i + 1,
            None => slice.len(),
        };
        entries.push(Entry::Opaque(cursor..cursor + end));
        cursor += end;
    }

    debug_assert!(validate_spans(src, &entries));
    entries
}

fn validate_spans(src: &str, entries: &[Entry]) -> bool {
    entries.iter().all(|e| {
        let span = match e {
            Entry::Data { span, .. } => span,
            Entry::Opaque(span) => span,
        };
        span.start <= span.end && span.end <= src.len()
    })
}

// ---------------------------------------------------------------------------

struct DataLineResult {
    atom_span: Range<usize>,
    atom: Dep,
    values: Vec<Token>,
}

fn data_line(input: &mut Stream<'_>) -> winnow::Result<DataLineResult> {
    // Leading whitespace (no newlines).
    space0.parse_next(input)?;

    // Atom: first non-whitespace token, must not start with '#'.
    let peek_ch = input.as_ref().chars().next();
    if matches!(peek_ch, None | Some('#') | Some('\n')) {
        return Err(ContextError::new());
    }

    let atom_start = input.current_token_start();
    let atom_raw: &str = take_while(1.., |c: char| !c.is_whitespace()).parse_next(input)?;
    let atom_end = input.current_token_start();

    let atom = Dep::parse(atom_raw).map_err(|_| ContextError::new())?;

    // Values: space-separated tokens until '#' or newline.
    let mut values: Vec<Token> = Vec::new();
    loop {
        let ws: &str = take_while(0.., |c: char| c == ' ' || c == '\t').parse_next(input)?;
        if ws.is_empty() {
            break;
        }
        let next = input.as_ref().chars().next();
        if matches!(next, None | Some('\n') | Some('#')) {
            break;
        }
        let val_start = input.current_token_start();
        let text: &str =
            take_while(1.., |c: char| !c.is_whitespace() && c != '#').parse_next(input)?;
        let val_end = input.current_token_start();
        values.push(Token {
            span: val_start..val_end,
            text: text.to_owned(),
        });
    }

    // Optional inline comment.
    opt(('#', take_till(0.., '\n'))).parse_next(input)?;

    // Newline or EOF.
    alt(('\n'.void(), winnow::combinator::eof.void())).parse_next(input)?;

    Ok(DataLineResult {
        atom_span: atom_start..atom_end,
        atom,
        values,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Serialises back to the source text; byte-identical to the input if no
/// edits were made.
impl std::fmt::Display for PackageConf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.src)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> PackageConf {
        PackageConf::parse(s.to_owned()).unwrap()
    }

    #[test]
    fn simple_entry() {
        let pc = parse("sys-apps/bubblewrap suid\n");
        let mut it = pc.entries();
        let e = it.next().unwrap();
        assert_eq!(e.atom.to_string(), "sys-apps/bubblewrap");
        assert_eq!(e.values().collect::<Vec<_>>(), ["suid"]);
        assert!(it.next().is_none());
    }

    #[test]
    fn versioned_atom() {
        let pc = parse(">=sys-libs/libcap-2.76 static-libs\n");
        let e = pc.entries().next().unwrap();
        assert_eq!(e.atom_raw(), ">=sys-libs/libcap-2.76");
        assert_eq!(e.values().collect::<Vec<_>>(), ["static-libs"]);
    }

    #[test]
    fn multiple_values() {
        let pc = parse("sys-boot/grub -themes -fonts -branding\n");
        let e = pc.entries().next().unwrap();
        assert_eq!(
            e.values().collect::<Vec<_>>(),
            ["-themes", "-fonts", "-branding"]
        );
    }

    #[test]
    fn comment_lines_skipped() {
        let src = "# comment\nsys-apps/foo bar\n# another\n";
        let pc = parse(src);
        assert_eq!(pc.entries().count(), 1);
        assert_eq!(pc.to_string(), src);
    }

    #[test]
    fn inline_comment_preserved() {
        let src = "sys-apps/foo bar # why\n";
        let pc = parse(src);
        let e = pc.entries().next().unwrap();
        assert_eq!(e.values().collect::<Vec<_>>(), ["bar"]);
        assert_eq!(pc.to_string(), src);
    }

    #[test]
    fn blank_lines_preserved() {
        let src = "\nsys-apps/foo bar\n\nsys-libs/baz qux\n";
        let pc = parse(src);
        assert_eq!(pc.entries().count(), 2);
        assert_eq!(pc.to_string(), src);
    }

    #[test]
    fn mask_entry_no_values() {
        let pc = parse(">cross-riscv64-unknown-linux-gnu/gcc-16.0.1_p20260308\n");
        let e = pc.entries().next().unwrap();
        assert_eq!(e.values().count(), 0);
    }

    #[test]
    fn roundtrip_unmodified() {
        let src = "# comment\nsys-apps/bubblewrap suid\n\n>=dev-libs/foo-1.0 bar baz\n";
        let pc = parse(src);
        assert_eq!(pc.to_string(), src);
    }

    #[test]
    fn set_updates_existing() {
        let mut pc = parse("sys-apps/bubblewrap suid\n");
        let atom = Dep::parse("sys-apps/bubblewrap").unwrap();
        pc.set(&atom, &["suid", "seccomp"]);
        assert!(pc.to_string().contains("suid seccomp"));
        assert_eq!(pc.entries().count(), 1);
    }

    #[test]
    fn set_appends_new() {
        let mut pc = parse("sys-apps/foo bar\n");
        let atom = Dep::parse("sys-libs/baz").unwrap();
        pc.set(&atom, &["qux"]);
        assert_eq!(pc.entries().count(), 2);
        assert!(pc.to_string().contains("sys-libs/baz qux"));
    }

    #[test]
    fn remove_entry() {
        let mut pc = parse("sys-apps/foo bar\nsys-libs/baz qux\n");
        let atom = Dep::parse("sys-apps/foo").unwrap();
        assert!(pc.remove(&atom));
        assert_eq!(pc.entries().count(), 1);
        assert!(!pc.to_string().contains("sys-apps/foo"));
    }
}
