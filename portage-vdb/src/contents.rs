//! Parser for the VDB `CONTENTS` file
//!
//! Format (one entry per line):
//! ```text
//! obj /path/to/file md5hash mtime
//! dir /path/to/dir
//! sym /path/to/link -> target mtime
//! fif /path/to/pipe
//! dev /path/to/device
//! ```

use camino::{Utf8Path, Utf8PathBuf};

/// Kind of filesystem entry in a CONTENTS file
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentsKind {
    /// Regular file (`obj`). Carries an MD5 hash and mtime
    Obj,
    /// Directory (`dir`)
    Dir,
    /// Symbolic link (`sym`). Carries a target path and mtime
    Sym,
    /// FIFO/pipe (`fif`)
    Fifo,
    /// Device node (`dev`)
    Dev,
}

/// A single entry from a `CONTENTS` file
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentsEntry {
    /// Kind of entry (obj, dir, sym, fif, dev)
    pub kind: ContentsKind,
    /// Absolute path of the installed file/directory/symlink
    pub path: Utf8PathBuf,
    /// MD5 digest (only for `Obj` entries)
    pub md5: Option<String>,
    /// File size or symlink target mtime as a Unix timestamp
    pub mtime: Option<u64>,
    /// Symlink target (only for `Sym` entries)
    pub target: Option<Utf8PathBuf>,
}

/// A single entry from a `CONTENTS` file, borrowed from the file text
///
/// The scanning counterpart to [`ContentsEntry`]: a package's CONTENTS runs
/// to hundreds of thousands of lines, and a caller that only asks a question
/// of each entry — who owns this path, which entries are symlinks — has no
/// use for the owned `Utf8PathBuf`/`String` per field that [`ContentsEntry`]
/// allocates. Both come from [`ContentsRef::parse_line`], so Portage's
/// fields-from-the-right rules have exactly one implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentsRef<'a> {
    /// Kind of entry (obj, dir, sym, fif, dev)
    pub kind: ContentsKind,
    /// Absolute path of the installed file/directory/symlink
    pub path: &'a Utf8Path,
    /// MD5 digest (only for `Obj` entries)
    pub md5: Option<&'a str>,
    /// File size or symlink target mtime as a Unix timestamp
    pub mtime: Option<u64>,
    /// Symlink target (only for `Sym` entries)
    pub target: Option<&'a Utf8Path>,
}

impl<'a> ContentsRef<'a> {
    /// Parse a single CONTENTS line. See [`ContentsEntry::parse_line`] for the
    /// format; this is the implementation both share.
    ///
    /// Hand-written rather than `winnow`, unlike every other parser here.
    /// CONTENTS is delimited from the *right* — a path may contain spaces,
    /// so `obj`'s md5/mtime and `sym`'s mtime are only identifiable as the
    /// last tokens, and `sym` splits on the *last* `" -> "`. A forward
    /// combinator can't express that without these same `rsplit_once`
    /// calls, or a rejoin that corrupts paths with consecutive spaces.
    ///
    /// Returns `None` for blank or unrecognised lines.
    pub fn parse_line(line: &'a str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        if let Some(rest) = line.strip_prefix("obj ") {
            // obj PATH MD5 MTIME — path may contain spaces.
            let (path_and_md5, mtime_str) = rest.rsplit_once(' ')?;
            let (path_str, md5) = path_and_md5.rsplit_once(' ')?;
            if path_str.is_empty() || md5.chars().any(char::is_whitespace) {
                return None;
            }
            return Some(ContentsRef {
                kind: ContentsKind::Obj,
                path: Utf8Path::new(path_str),
                md5: Some(md5),
                mtime: mtime_str.parse().ok(),
                target: None,
            });
        }

        if let Some(rest) = line.strip_prefix("sym ") {
            // sym PATH -> TARGET MTIME — path and target may contain spaces;
            // use the last " -> " so a path that itself contains " -> " still
            // parses like Portage's greedy first group.
            let (path_and_target, mtime_str) = rest.rsplit_once(' ')?;
            let (path_str, target_str) = path_and_target.rsplit_once(" -> ")?;
            if path_str.is_empty() {
                return None;
            }
            return Some(ContentsRef {
                kind: ContentsKind::Sym,
                path: Utf8Path::new(path_str),
                md5: None,
                mtime: mtime_str.parse().ok(),
                target: Some(Utf8Path::new(target_str)),
            });
        }

        for (prefix, kind) in [
            ("dir ", ContentsKind::Dir),
            ("fif ", ContentsKind::Fifo),
            ("dev ", ContentsKind::Dev),
        ] {
            if let Some(path_str) = line.strip_prefix(prefix) {
                if path_str.is_empty() {
                    return None;
                }
                return Some(ContentsRef {
                    kind,
                    path: Utf8Path::new(path_str),
                    md5: None,
                    mtime: None,
                    target: None,
                });
            }
        }

        None
    }

    /// Every entry in a full CONTENTS file, borrowed from `contents`
    ///
    /// The lazy counterpart to [`ContentsEntry::parse`] — nothing is
    /// allocated, and a caller that short-circuits (`any`, `find`) stops
    /// parsing at the matching line.
    pub fn parse(contents: &'a str) -> impl Iterator<Item = ContentsRef<'a>> {
        contents.lines().filter_map(ContentsRef::parse_line)
    }

    /// Copy into an owned [`ContentsEntry`]
    pub fn to_entry(&self) -> ContentsEntry {
        ContentsEntry {
            kind: self.kind,
            path: self.path.to_path_buf(),
            md5: self.md5.map(str::to_string),
            mtime: self.mtime,
            target: self.target.map(Utf8Path::to_path_buf),
        }
    }
}

/// Serialize a slice of entries back to a CONTENTS file string
pub fn format_contents(entries: &[ContentsEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        out.push_str(&e.format_line());
        out.push('\n');
    }
    out
}

impl ContentsEntry {
    /// Serialize this entry to a single CONTENTS line (no trailing newline)
    pub fn format_line(&self) -> String {
        match self.kind {
            ContentsKind::Obj => format!(
                "obj {} {} {}",
                self.path,
                self.md5.as_deref().unwrap_or("0"),
                self.mtime.unwrap_or(0),
            ),
            ContentsKind::Dir => format!("dir {}", self.path),
            ContentsKind::Sym => format!(
                "sym {} -> {} {}",
                self.path,
                self.target.as_deref().map(|p| p.as_str()).unwrap_or(""),
                self.mtime.unwrap_or(0),
            ),
            ContentsKind::Fifo => format!("fif {}", self.path),
            ContentsKind::Dev => format!("dev {}", self.path),
        }
    }

    /// Parse a single line from a CONTENTS file
    ///
    /// Paths may contain spaces. Portage's format (see `dblink._contents_re`)
    /// is field-oriented from the right for `obj`/`sym`:
    /// - `obj <path…> <md5> <mtime>` — md5 is one non-space token, mtime digits
    /// - `sym <path…> -> <target…> <mtime>` — last space-separated token is mtime;
    ///   path/target split on the last ` -> ` (matches Portage's greedy path)
    /// - `dir`/`fif`/`dev` `<path…>` — everything after the kind is the path
    ///
    /// Returns `None` for blank or unrecognised lines.
    pub fn parse_line(line: &str) -> Option<Self> {
        ContentsRef::parse_line(line).map(|e| e.to_entry())
    }

    /// Parse a full CONTENTS file into entries
    ///
    /// Allocates one owned entry per line; [`ContentsRef::parse`] is the
    /// borrowing form for callers that only scan.
    pub fn parse(contents: &str) -> Vec<Self> {
        ContentsRef::parse(contents).map(|e| e.to_entry()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_obj() {
        let entry = ContentsEntry::parse_line(
            "obj /etc/skel/.bashrc d210b9cd7fc07420736480f2062d7d7f 1778566175",
        )
        .unwrap();
        assert_eq!(entry.kind, ContentsKind::Obj);
        assert_eq!(entry.path, Utf8PathBuf::from("/etc/skel/.bashrc"));
        assert_eq!(
            entry.md5.as_deref(),
            Some("d210b9cd7fc07420736480f2062d7d7f")
        );
        assert_eq!(entry.mtime, Some(1778566175));
        assert!(entry.target.is_none());
    }

    #[test]
    fn parse_dir() {
        let entry = ContentsEntry::parse_line("dir /etc").unwrap();
        assert_eq!(entry.kind, ContentsKind::Dir);
        assert_eq!(entry.path, Utf8PathBuf::from("/etc"));
    }

    #[test]
    fn parse_sym() {
        let entry = ContentsEntry::parse_line("sym /bin/rbash -> bash 1778566174").unwrap();
        assert_eq!(entry.kind, ContentsKind::Sym);
        assert_eq!(entry.path, Utf8PathBuf::from("/bin/rbash"));
        assert_eq!(entry.target.as_deref(), Some(camino::Utf8Path::new("bash")));
        assert_eq!(entry.mtime, Some(1778566174));
    }

    #[test]
    fn parse_sym_absolute_target() {
        let entry =
            ContentsEntry::parse_line("sym /usr/lib/libfoo.so -> libfoo.so.1 1234567890").unwrap();
        assert_eq!(entry.kind, ContentsKind::Sym);
        assert_eq!(
            entry.target.as_deref(),
            Some(camino::Utf8Path::new("libfoo.so.1"))
        );
    }

    #[test]
    fn format_obj() {
        let e = ContentsEntry {
            kind: ContentsKind::Obj,
            path: Utf8PathBuf::from("/etc/foo"),
            md5: Some("abc123".to_string()),
            mtime: Some(100),
            target: None,
        };
        assert_eq!(e.format_line(), "obj /etc/foo abc123 100");
    }

    #[test]
    fn format_dir() {
        let e = ContentsEntry {
            kind: ContentsKind::Dir,
            path: Utf8PathBuf::from("/etc"),
            md5: None,
            mtime: None,
            target: None,
        };
        assert_eq!(e.format_line(), "dir /etc");
    }

    #[test]
    fn format_sym() {
        let e = ContentsEntry {
            kind: ContentsKind::Sym,
            path: Utf8PathBuf::from("/bin/sh"),
            md5: None,
            mtime: Some(200),
            target: Some(Utf8PathBuf::from("bash")),
        };
        assert_eq!(e.format_line(), "sym /bin/sh -> bash 200");
    }

    #[test]
    fn format_roundtrip() {
        let lines = [
            "obj /etc/foo abc123 100",
            "dir /etc",
            "sym /bin/sh -> bash 200",
        ];
        for line in lines {
            let parsed = ContentsEntry::parse_line(line).unwrap();
            assert_eq!(parsed.format_line(), line);
        }
    }

    // Real Portage CONTENTS includes paths with spaces (cmake docs, etc.).
    #[test]
    fn parse_obj_path_with_spaces() {
        let line = "obj /usr/share/cmake/Help/generator/Visual Studio 9 2008.rst d5a7badc3622ea283eeb1f1668288e1d 1774616911";
        let entry = ContentsEntry::parse_line(line).unwrap();
        assert_eq!(entry.kind, ContentsKind::Obj);
        assert_eq!(
            entry.path.as_str(),
            "/usr/share/cmake/Help/generator/Visual Studio 9 2008.rst"
        );
        assert_eq!(
            entry.md5.as_deref(),
            Some("d5a7badc3622ea283eeb1f1668288e1d")
        );
        assert_eq!(entry.mtime, Some(1774616911));
        assert_eq!(entry.format_line(), line);
    }

    #[test]
    fn parse_dir_path_with_spaces() {
        let line = "dir /usr/share/foo bar";
        let entry = ContentsEntry::parse_line(line).unwrap();
        assert_eq!(entry.path.as_str(), "/usr/share/foo bar");
        assert_eq!(entry.format_line(), line);
    }

    #[test]
    fn parse_sym_path_and_target_with_spaces() {
        let line = "sym /usr/lib/foo bar.so -> /lib/foo bar.so.1 1234567890";
        let entry = ContentsEntry::parse_line(line).unwrap();
        assert_eq!(entry.path.as_str(), "/usr/lib/foo bar.so");
        assert_eq!(
            entry.target.as_deref().map(|p| p.as_str()),
            Some("/lib/foo bar.so.1")
        );
        assert_eq!(entry.mtime, Some(1234567890));
        assert_eq!(entry.format_line(), line);
    }

    #[test]
    fn format_contents_fn() {
        let entries = ContentsEntry::parse("dir /etc\nobj /etc/foo abc123 100\n");
        let out = format_contents(&entries);
        assert_eq!(out, "dir /etc\nobj /etc/foo abc123 100\n");
    }

    #[test]
    fn parse_empty_line() {
        assert!(ContentsEntry::parse_line("").is_none());
        assert!(ContentsEntry::parse_line("  ").is_none());
    }

    #[test]
    fn parse_full_contents() {
        let raw = "dir /etc\nobj /etc/foo abc123 100\nsym /etc/bar -> baz 200\n";
        let entries = ContentsEntry::parse(raw);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, ContentsKind::Dir);
        assert_eq!(entries[1].kind, ContentsKind::Obj);
        assert_eq!(entries[2].kind, ContentsKind::Sym);
    }
}
