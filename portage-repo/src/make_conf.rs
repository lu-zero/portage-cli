//! Parser and editor for `/etc/portage/make.conf`
//!
//! Uses `brush-parser`'s winnow-based parser for syntactic analysis of
//! variable assignments.  Comments are preserved precisely: brush-parser's
//! winnow parser now records comment byte spans into `Program.comments`, so
//! we know exactly where each comment lives without gap-filling heuristics.
//!
//! # File or directory
//!
//! `make.conf(5)` allows `/etc/portage/make.conf` (and the legacy path) to be
//! either a single file or a directory of fragments. Directory form uses
//! [`crate::ConfigFilesMode::Flat`]: regular files directly in the directory,
//! lexical order, skipping `.…` and `…~` (same as other Portage config dirs).
//! Nested subdirectories are not walked — matching the man page wording; Portage
//! happens to recurse via a shared helper, but flat is the documented contract.
//!
//! [`MakeConf::load`] concatenates directory fragments for reading. For USE
//! edits against a multi-fragment directory, prefer
//! [`MakeConf::apply_use_changes_at`], which patches a single fragment.
//!
//! # How statement spans are computed
//!
//! Each `CompoundListItem` carries an AST span covering its tokens.  For
//! `CFLAGS="-O2"  # optimization`, the AST span ends at `"`.  The trailing
//! `  # optimization` is in `Program.comments`; `\n` terminates the logical
//! line.  We extend each statement span to include any trailing comment
//! (`extend_past_comment`) and then to the next `\n` (`extend_to_newline`).
//!
//! Everything between statement spans (blank lines, leading whitespace) is
//! `Entry::Opaque` and reproduced verbatim.

use brush_builtins::ShellBuilderExt as _;
use std::ops::Range;
use std::path::{Path, PathBuf};

use brush_parser::ast::{
    AssignmentName, AssignmentValue, Command, CommandPrefixOrSuffixItem, SourceLocation,
};
use brush_parser::{ParserOptions, SourceInfo};
use camino::{Utf8Path, Utf8PathBuf};

use crate::{ConfigFilesMode, Error, Result, list_config_files};

/// Default path to the active make.conf
pub const DEFAULT_MAKE_CONF: &str = "/etc/portage/make.conf";
/// Legacy path, used as a fallback when the default does not exist
pub const LEGACY_MAKE_CONF: &str = "/etc/make.conf";

/// Fragment name created when saving USE edits into an empty `make.conf/` dir
pub const MAKE_CONF_DIR_FALLBACK_FRAGMENT: &str = "zz-em";

/// A parsed and editable make.conf file
pub struct MakeConf {
    src: String,
    entries: Vec<Entry>,
}

/// Expand make.conf candidate paths (each a file or Flat directory) into the
/// ordered list of regular files to source.
///
/// Missing candidates contribute nothing. Typical call:
///
/// ```ignore
/// // Portage order: legacy first, modern second (later overrides).
/// let files = expand_make_conf_paths([
///     root.join("etc/make.conf"),
///     root.join("etc/portage/make.conf"),
/// ])?;
/// ```
pub fn expand_make_conf_paths<I, P>(candidates: I) -> Result<Vec<PathBuf>>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut out = Vec::new();
    for c in candidates {
        out.extend(list_config_files(c.as_ref(), ConfigFilesMode::Flat)?);
    }
    Ok(out)
}

/// One element in the tiled decomposition of the source file
enum Entry {
    /// Raw bytes not covered by the AST
    ///
    /// Comments, blank lines, leading / trailing whitespace between
    /// statements. Reproduced verbatim on serialisation.
    Opaque(Range<usize>),

    /// A bash statement that consists entirely of variable assignments
    /// (no command name)
    ///
    /// Covers from the first token of the statement to the end of the
    /// `\n`-terminated logical line (including any trailing inline
    /// comment).
    Statement {
        /// Byte span in `src` for the full logical line
        span: Range<usize>,
        vars: Vec<Var>,
    },
}

/// A single variable assignment within a statement
struct Var {
    name: String,
    /// `true` for `NAME+=VALUE` (append), `false` for `NAME=VALUE` (assign)
    append: bool,
    /// Byte range of the raw value in `src`, **excluding** surrounding quotes
    ///
    /// Empty range for `FOO=` (no value).
    value: Range<usize>,
    /// Byte range of the whole value word in `src`, quotes included
    word: Range<usize>,
    /// How `value` is quoted, which decides how [`MakeConf::set`] writes a new one
    quote: Quote,
}

/// How an assignment's value word is quoted
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Quote {
    /// One plain word, or a mix of quoted and unquoted parts: a new value
    /// replaces the whole word, double-quoted when it needs quoting
    None,
    /// `'…'` with no `'` inside
    Single,
    /// `"…"` with no unescaped `"` inside
    Double,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

impl MakeConf {
    /// Read and parse `path` (regular file or Flat directory of fragments)
    ///
    /// Directory fragments are concatenated in lexical order. An empty
    /// directory yields an empty config (no error).
    pub fn load(path: &Utf8Path) -> Result<Self> {
        let files = list_config_files(path.as_std_path(), ConfigFilesMode::Flat)?;
        if files.is_empty() {
            if path.is_dir() {
                return Self::parse(String::new());
            }
            // Preserve a real I/O error for a missing/unreadable file.
            let src = std::fs::read_to_string(path).map_err(|e| Error::Io {
                path: path.to_path_buf().into_std_path_buf(),
                source: e,
            })?;
            return Self::parse(src);
        }
        if files.len() == 1 && files[0] == path.as_std_path() {
            let src = std::fs::read_to_string(path).map_err(|e| Error::Io {
                path: path.to_path_buf().into_std_path_buf(),
                source: e,
            })?;
            return Self::parse(src);
        }
        let mut combined = String::new();
        for f in files {
            let chunk =
                std::fs::read_to_string(&f).map_err(|e| Error::Io { path: f, source: e })?;
            combined.push_str(&chunk);
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
        }
        Self::parse(combined)
    }

    /// Load whichever of [`DEFAULT_MAKE_CONF`] / [`LEGACY_MAKE_CONF`] exists
    pub fn load_default() -> Result<Self> {
        for path in [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF] {
            let p = Utf8Path::new(path);
            if p.exists() {
                return Self::load(p);
            }
        }
        Err(Error::Io {
            path: Utf8Path::new(DEFAULT_MAKE_CONF)
                .to_path_buf()
                .into_std_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no make.conf found"),
        })
    }

    /// Parse from an owned string (useful for testing)
    pub fn parse(src: String) -> Result<Self> {
        let program = parse_program(&src)?;
        let entries = build_entries(&src, &program);
        Ok(Self { src, entries })
    }

    /// Return the raw (unexpanded) value for the last non-append assignment
    /// to `name`, using bash last-wins semantics.
    ///
    /// The returned slice has surrounding quotes stripped but is otherwise
    /// verbatim — e.g. `${COMMON_FLAGS}` is not expanded.
    pub fn get(&self, name: &str) -> Option<&str> {
        for entry in self.entries.iter().rev() {
            let Entry::Statement { vars, .. } = entry else {
                continue;
            };
            for var in vars.iter().rev() {
                if var.name == name && !var.append {
                    return Some(&self.src[var.value.clone()]);
                }
            }
        }
        None
    }

    /// Apply this file's assignments to `env` the way `bash source` would
    ///
    /// Sources the raw file text through a minimal, non-interactive
    /// [`brush_core::Shell`] (bash's standard builtins only — no
    /// ebuild-specific ones, no [`crate::Repository`] dependency, since
    /// make.conf is never ebuild content), then overlays every variable
    /// left in the shell afterward onto `env`.
    ///
    /// `env`'s current contents are seeded into the shell first, so a
    /// caller-provided value (e.g. an ambient `CHOST`) is visible to
    /// self-references and `NAME+=VALUE` appends onto it. Because this is a
    /// real shell, the full range of bash semantics applies (`${NAME}`
    /// expansion, parameter expansion, command substitution, arithmetic),
    /// not just the subset a hand-rolled scanner would special-case.
    pub async fn apply_to(
        &self,
        env: &mut std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        let mut shell = brush_core::Shell::builder()
            .do_not_inherit_env(true)
            .profile(brush_core::ProfileLoadBehavior::Skip)
            .rc(brush_core::RcLoadBehavior::Skip)
            .parser(brush_core::parser::ParserImpl::Winnow)
            // Pinned rather than left to default to the process's ambient
            // cwd: a make.conf evaluation has no real notion of "current
            // directory", and `/` is guaranteed to exist regardless of what
            // the caller's process cwd happens to be at the moment.
            .working_dir(std::path::PathBuf::from("/"))
            .default_builtins(brush_builtins::BuiltinSet::BashMode)
            .build()
            .await
            .map_err(|e| Error::Shell(e.to_string()))?;

        for (name, value) in env.iter() {
            shell
                .set_env_global(name, brush_core::ShellVariable::new(value.as_str()))
                .map_err(|e| Error::Shell(e.to_string()))?;
        }

        let params = shell.default_exec_params();
        let result = shell
            .run_string(
                self.src.as_str(),
                &brush_core::SourceInfo::from("make.conf"),
                &params,
            )
            .await
            .map_err(|e| Error::Shell(e.to_string()))?;
        crate::error::require_completed("make.conf", result)?;

        let names: Vec<String> = shell
            .env()
            .iter()
            .map(|(name, _)| name.clone())
            .filter(|name| !crate::build::shell::is_bash_internal_var(name))
            .collect();
        for name in names {
            if let Some(value) = shell.env().get_str(&name, &shell) {
                env.insert(name, value.into_owned());
            }
        }

        Ok(())
    }

    /// Update `name` to `value`, preserving surrounding quotes and
    /// trailing comments
    ///
    /// Updates the first occurrence and removes any later duplicates
    /// (first occurrence wins after the edit).
    ///
    /// If `name` is not present, appends `NAME="value"\n` at the end.
    ///
    /// `value` is raw shell text, like what [`Self::get`] returns, and is quoted so the file still parses
    /// and the assignment stays one word. If that would not parse, it is written as an escaped literal;
    /// the file is never left half-edited.
    pub fn set(&mut self, name: &str, value: &str) {
        let saved = self.src.clone();
        self.set_encoded(name, value, encode_raw, false);
        if !self.assigns(name) {
            self.src = saved.clone();
            self.rebuild();
            self.set_encoded(name, value, encode_literal, false);
            if !self.assigns(name) {
                self.src = saved;
                self.rebuild();
            }
        }
    }

    /// Set `name` to `value` taken as literal text: `$`, `` ` `` and `\` in it do not expand
    ///
    /// For values that are data rather than shell text, such as mirror URLs; otherwise as
    /// [`Self::set`].
    pub fn set_literal(&mut self, name: &str, value: &str) {
        let saved = self.src.clone();
        // Portage expands `$` even inside single quotes, so the word is rewritten double-quoted.
        self.set_encoded(name, value, encode_literal, true);
        if !self.assigns(name) {
            self.src = saved;
            self.rebuild();
        }
    }

    /// Whether the source parses and its last plain assignment to `name` is
    /// one word (so a later edit can find it again)
    fn assigns(&self, name: &str) -> bool {
        parse_program(&self.src).is_ok() && self.get(name).is_some()
    }

    /// Write `value` for `name` with `encode`; `whole_word` replaces the whole value word, quotes
    /// included, rather than only what is inside them
    fn set_encoded(
        &mut self,
        name: &str,
        value: &str,
        encode: fn(&str, Quote) -> String,
        whole_word: bool,
    ) {
        // Collect all statement indices that touch `name`, in source order.
        let positions: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                if let Entry::Statement { vars, .. } = e {
                    if vars.iter().any(|v| v.name == name && !v.append) {
                        Some(i)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        match positions.as_slice() {
            [] => {
                // Name not found — append a new assignment.
                if !self.src.is_empty() && !self.src.ends_with('\n') {
                    self.src.push('\n');
                }
                let append = format!("{}=\"{}\"\n", name, encode(value, Quote::Double));
                self.src.push_str(&append);
                // Rebuild entries from scratch (spans have shifted).
                self.rebuild();
            }
            [first, rest @ ..] => {
                // Remove duplicates from the end first (to keep offsets stable
                // while we work backwards), then update the first occurrence.
                for &idx in rest.iter().rev() {
                    let Entry::Statement { span, .. } = &self.entries[idx] else {
                        unreachable!()
                    };
                    let span = span.clone();
                    self.src.replace_range(span, "");
                    self.rebuild();
                }
                // Now update the first occurrence.
                let Entry::Statement { vars, .. } = &self.entries[*first] else {
                    unreachable!(
                        "Entry at index {first} should be a Statement since we found it via filter_map"
                    )
                };
                // SAFETY: We found this entry because vars.iter().any(|v| v.name == name && !v.append) was true,
                // so the find below must also succeed.
                let var = vars
                    .iter()
                    .find(|v| v.name == name && !v.append)
                    .expect("inconsistent state: entry matched in filter but variable not found");
                // The value span excludes any surrounding quotes, so replacing
                // it preserves the original quoting style; a mixed or plain
                // word is replaced whole.
                let (range, text) = match var.quote {
                    Quote::Single | Quote::Double if !whole_word => {
                        (var.value.clone(), encode(value, var.quote))
                    }
                    _ => (var.word.clone(), encode(value, Quote::None)),
                };
                self.src.replace_range(range, &text);
                self.rebuild();
            }
        }
    }

    /// Add/remove USE flags in `USE`, portage's simple bare-flag model
    ///
    /// No separate profile-level `+`/`-` handling here — a leading `+` on
    /// an `add`/`remove` token is stripped and ignored, matching
    /// `em useflags`'s own single-string USE editor. Returns the new
    /// `USE` value for the caller to report.
    pub fn apply_use_changes(&mut self, add: &[String], remove: &[String]) -> String {
        let mut flags = flag_words(self.get("USE").unwrap_or(""));

        for flag in remove {
            let flag = flag.trim_start_matches('+');
            flags.retain(|f| f != flag && f != &format!("+{flag}"));
        }
        for flag in add {
            let flag = flag.trim_start_matches('+');
            flags.retain(|f| f != flag && f != &format!("+{flag}") && f != &format!("-{flag}"));
            flags.push(flag.to_string());
        }

        let new_use = flags.join(" ");
        self.set("USE", &new_use);
        new_use
    }

    /// Fold `add`/`subtract`/`drop` onto the current effective USE at
    /// `path` without writing anything
    ///
    /// The same trichotomy `em pkg use` applies to `package.use` entries:
    /// `add` (positive), `subtract` (explicit negative, `-flag`), `drop`
    /// (removes both forms, reverting to the profile default). Returns
    /// `(old, new)` joined values for `var`, so a caller can preview or
    /// diff the change before deciding to write it.
    ///
    /// `var` is any space-separated flag-list variable — `USE` itself, or a
    /// USE_EXPAND variable such as `VIDEO_CARDS`; both are edited the same
    /// way in make.conf, just under a different name.
    pub fn preview_use_changes_at(
        path: &Utf8Path,
        var: &str,
        add: &[String],
        subtract: &[String],
        drop: &[String],
    ) -> Result<(String, String)> {
        // Effective value for the edit is the full directory/file fold.
        let current_fold =
            Self::load(path).unwrap_or_else(|_| Self::parse(String::new()).expect("empty parse"));
        let current_flags = flag_words(current_fold.get(var).unwrap_or(""));
        let old_value = current_flags.join(" ");
        let new_flags = apply_use_flag_ops(current_flags, add, subtract, drop);
        Ok((old_value, new_flags.join(" ")))
    }

    /// Apply add/subtract/drop to `var` at `path` (file or Flat directory)
    /// and write back. See [`Self::preview_use_changes_at`] for the edit
    /// semantics and what `var` may be.
    ///
    /// Multi-fragment directories: edits the **last** fragment that assigns
    /// `var` (bash last-wins), or the last fragment if none do; an empty
    /// directory gets a new [`MAKE_CONF_DIR_FALLBACK_FRAGMENT`]. The
    /// effective value after the edit is returned (full fold across all
    /// fragments).
    pub fn apply_use_changes_at(
        path: &Utf8Path,
        var: &str,
        add: &[String],
        subtract: &[String],
        drop: &[String],
    ) -> Result<String> {
        let files = list_config_files(path.as_std_path(), ConfigFilesMode::Flat)?;
        let target: Utf8PathBuf = if path.is_dir() {
            if files.is_empty() {
                path.join(MAKE_CONF_DIR_FALLBACK_FRAGMENT)
            } else {
                let mut pick = files.last().cloned().expect("non-empty");
                for f in &files {
                    // Load each fragment alone (not the directory) to see who
                    // assigns `var`.
                    if let Ok(utf) = Utf8PathBuf::try_from(f.clone())
                        && let Ok(frag) = Self::load(&utf)
                        && frag.get(var).is_some()
                    {
                        pick = f.clone();
                    }
                }
                Utf8PathBuf::try_from(pick).map_err(|e| Error::Io {
                    path: e.into_path_buf(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "non-utf8 make.conf fragment path",
                    ),
                })?
            }
        } else {
            path.to_owned()
        };

        let (_, new_value) = Self::preview_use_changes_at(path, var, add, subtract, drop)?;

        // Write the resulting value into a single fragment (last-wins for load).
        let mut frag = if target.as_std_path().is_file() {
            Self::load(&target)?
        } else {
            Self::parse(String::new())?
        };
        frag.set(var, &new_value);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent.as_std_path()).map_err(|e| Error::Io {
                path: parent.to_path_buf().into_std_path_buf(),
                source: e,
            })?;
        }
        frag.save(&target)?;
        Ok(new_value)
    }

    /// Save to `path`
    ///
    /// If `path` is a directory: write a single fragment when the directory
    /// has zero or one fragment (empty → [`MAKE_CONF_DIR_FALLBACK_FRAGMENT`]);
    /// refuse multi-fragment wholesale rewrite (use
    /// [`Self::apply_use_changes_at`] for USE edits).
    pub fn save(&self, path: &Utf8Path) -> Result<()> {
        let target: Utf8PathBuf = if path.is_dir() {
            let files = list_config_files(path.as_std_path(), ConfigFilesMode::Flat)?;
            match files.as_slice() {
                [] => path.join(MAKE_CONF_DIR_FALLBACK_FRAGMENT),
                [one] => Utf8PathBuf::try_from(one.clone()).map_err(|e| Error::Io {
                    path: e.into_path_buf(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "non-utf8 make.conf fragment path",
                    ),
                })?,
                _ => {
                    return Err(Error::Io {
                        path: path.to_path_buf().into_std_path_buf(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "cannot rewrite multi-fragment make.conf directory as a whole; \
                             edit a fragment or use apply_use_changes_at for USE",
                        ),
                    });
                }
            }
        } else {
            path.to_owned()
        };
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent.as_std_path());
        }
        std::fs::write(target.as_std_path(), self.to_string()).map_err(|e| Error::Io {
            path: target.into_std_path_buf(),
            source: e,
        })
    }

    /// Reparse `self.src` after an in-place mutation; text that no longer parses becomes one opaque entry
    fn rebuild(&mut self) {
        self.entries = match parse_program(&self.src) {
            Ok(program) => build_entries(&self.src, &program),
            Err(_) => vec![Entry::Opaque(0..self.src.len())],
        };
    }
}

/// The whitespace-separated words of a raw flag-list value
///
/// A backslash-newline continues the line in a double-quoted or plain word, so it is dropped
/// rather than read as a `\` flag.
fn flag_words(raw: &str) -> Vec<String> {
    let mut joined = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\n') => continue,
                Some(next) => {
                    joined.push(c);
                    joined.push(next);
                    continue;
                }
                None => {}
            }
        }
        joined.push(c);
    }
    joined.split_whitespace().map(str::to_string).collect()
}

/// Apply the `add`/`subtract`/`drop` trichotomy to a flat USE token list
///
/// The same rule `em pkg use` already applies to `package.use` entries,
/// kept here in lockstep so `em use` (this module) and `em pkg use`
/// (`PackageConf`) never drift onto different semantics for the same
/// three verbs.
fn apply_use_flag_ops(
    mut values: Vec<String>,
    add: &[String],
    subtract: &[String],
    drop: &[String],
) -> Vec<String> {
    for op in add.iter().chain(subtract).chain(drop) {
        let base = op.trim_start_matches('-');
        values.retain(|v| v.trim_start_matches('-') != base);
    }
    for flag in add {
        let base = flag.trim_start_matches('-');
        values.push(base.to_owned());
    }
    for flag in subtract {
        let base = flag.trim_start_matches('-');
        values.push(format!("-{base}"));
    }
    values
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_program(src: &str) -> Result<brush_parser::ast::Program> {
    let opts = ParserOptions::default();
    brush_parser::winnow_str::parse_program(src, &opts, &SourceInfo::default())
        .map_err(|e| Error::Shell(e.to_string()))
}

/// Build the tiled [`Entry`] list for `src` given its parsed `program`
///
/// Comment spans from `program.comments` are used to precisely extend each
/// statement span past any trailing inline comment before the final `\n`.
fn build_entries(src: &str, program: &brush_parser::ast::Program) -> Vec<Entry> {
    // Sorted comment end-positions for fast lookup.
    let mut comment_ends: Vec<usize> = program.comments.iter().map(|s| s.end.byte_offset).collect();
    comment_ends.sort_unstable();

    // Collect (logical-line-span, vars) for each pure-assignment statement.
    let mut stmts: Vec<(Range<usize>, Vec<Var>)> = Vec::new();

    for complete_cmd in &program.complete_commands {
        for item in &complete_cmd.0 {
            let Some(loc) = item.location() else {
                continue;
            };

            let start = loc.start.byte_offset;
            // Extend past any trailing comment on this line, then to the `\n`.
            let end =
                extend_to_newline(src, extend_past_comment(loc.end.byte_offset, &comment_ends));

            // Only handle the simple case: single pipeline (no `&&` / `||`).
            if !item.0.additional.is_empty() {
                continue;
            }
            let pipeline = &item.0.first;
            // Only one command per pipeline, no pipes.
            if pipeline.seq.len() != 1 {
                continue;
            }
            let Command::Simple(simple) = &pipeline.seq[0] else {
                continue;
            };
            // Pure assignment (no command name), or `export NAME=VALUE…`, which Portage's
            // getconfig reads as the same assignment.
            let items = match &simple.word_or_name {
                None => simple.prefix.as_ref().map(|p| &p.0),
                Some(word) if word.value == "export" && simple.prefix.is_none() => {
                    match &simple.suffix {
                        Some(suffix)
                            if suffix.0.iter().all(|i| {
                                matches!(i, CommandPrefixOrSuffixItem::AssignmentWord(..))
                            }) =>
                        {
                            Some(&suffix.0)
                        }
                        _ => None,
                    }
                }
                Some(_) => continue,
            };

            let mut vars = Vec::new();
            for item in items.into_iter().flatten() {
                if let CommandPrefixOrSuffixItem::AssignmentWord(assignment, _) = item
                    && let Some(v) = var_from_assignment(src, assignment)
                {
                    vars.push(v);
                }
            }

            if !vars.is_empty() {
                stmts.push((start..end, vars));
            }
        }
    }

    // Statements come out of the AST in source order; sort defensively.
    stmts.sort_by_key(|(r, _)| r.start);

    // Interleave with opaque gaps to tile the whole file.
    let mut entries = Vec::with_capacity(stmts.len() * 2 + 1);
    let mut cursor = 0usize;

    for (span, vars) in stmts {
        if cursor < span.start {
            entries.push(Entry::Opaque(cursor..span.start));
        }
        cursor = span.end;
        entries.push(Entry::Statement { span, vars });
    }

    if cursor < src.len() {
        entries.push(Entry::Opaque(cursor..src.len()));
    }

    entries
}

/// If there is a comment whose end byte is the nearest to `ast_end` on the
/// same line, return that comment's end position; otherwise return `ast_end`.
///
/// This moves the span boundary past a trailing inline comment like
/// `  # optimization` so that the comment is included in the statement span
/// and not leaked into the following opaque gap.
fn extend_past_comment(ast_end: usize, comment_ends: &[usize]) -> usize {
    // Find the smallest comment_end that is >= ast_end.
    match comment_ends.partition_point(|&e| e < ast_end) {
        i if i < comment_ends.len() => comment_ends[i],
        _ => ast_end,
    }
}

/// Advance `end` to include the next `\n` (and the intervening whitespace
/// between the last token / comment and the newline itself).
fn extend_to_newline(src: &str, end: usize) -> usize {
    match src[end..].find('\n') {
        Some(i) => end + i + 1,
        None => src.len(),
    }
}

/// Derive a [`Var`] from an AST `Assignment`
///
/// Computes the value span from `assignment.loc` and the name length
/// (avoids relying on `Word.loc` which may not always be populated).
fn var_from_assignment(src: &str, a: &brush_parser::ast::Assignment) -> Option<Var> {
    // Skip array assignments (uncommon in make.conf, hard to edit safely).
    if matches!(a.value, AssignmentValue::Array(_)) {
        return None;
    }

    let name = match &a.name {
        AssignmentName::VariableName(n) => n.clone(),
        // Array element assignments (`ARR[0]=...`) not relevant for make.conf.
        AssignmentName::ArrayElementName(..) => return None,
    };

    // a.loc spans the entire `NAME=VALUE` (or `NAME+=VALUE`) token.
    let token_start = a.loc.start.byte_offset;
    let token_end = a.loc.end.byte_offset;

    // Bytes consumed by the name and the `=` (plus `+` for append).
    let prefix_len = name.len() + if a.append { 2 } else { 1 };
    let value_start = token_start + prefix_len;

    if value_start >= token_end {
        return Some(Var {
            name,
            append: a.append,
            value: value_start..value_start,
            word: value_start..value_start,
            quote: Quote::None,
        });
    }

    // Strip one layer of surrounding quotes from the value span.
    let raw = &src[value_start..token_end];
    let (inner_start, inner_end) = if raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')))
    {
        (value_start + 1, token_end - 1)
    } else {
        (value_start, token_end)
    };

    let inner = &src[inner_start..inner_end];
    let quote = if inner_start == value_start {
        Quote::None
    } else if raw.starts_with('\'') {
        if inner.contains('\'') {
            Quote::None
        } else {
            Quote::Single
        }
    } else if has_unescaped_dquote(inner) || ends_in_lone_backslash(inner) {
        Quote::None
    } else {
        Quote::Double
    };

    Some(Var {
        name,
        append: a.append,
        value: inner_start..inner_end,
        word: value_start..token_end,
        quote,
    })
}

/// Whether `s` has a `"` not preceded by an odd run of backslashes
fn has_unescaped_dquote(s: &str) -> bool {
    let mut escaped = false;
    for c in s.chars() {
        match c {
            '\\' => escaped = !escaped,
            '"' if !escaped => return true,
            _ => escaped = false,
        }
    }
    false
}

/// Whether `s` ends in an odd run of backslashes (which would escape a closing `"`)
fn ends_in_lone_backslash(s: &str) -> bool {
    s.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1
}

/// Spell raw shell text `value` for a value word quoted as `quote`, keeping
/// `$` expansions: only what would end the quoting or split the word changes
fn encode_raw(value: &str, quote: Quote) -> String {
    match quote {
        Quote::Single => value.replace('\'', "'\\''"),
        Quote::Double => {
            let mut out = String::with_capacity(value.len());
            let mut escaped = false;
            for c in value.chars() {
                match c {
                    '\\' => escaped = !escaped,
                    '"' if !escaped => out.push('\\'),
                    _ => escaped = false,
                }
                out.push(c);
            }
            if ends_in_lone_backslash(&out) {
                out.push('\\');
            }
            out
        }
        Quote::None => {
            let plain = !value.is_empty()
                && !value
                    .chars()
                    .any(|c| c.is_whitespace() || "'\"\\`;&|<>()#".contains(c));
            // An empty plain word would make Portage's getconfig take the next token as the
            // value (`USE=` then `FEATURES=…` on the next line), so it is written `""`.
            if plain {
                value.to_owned()
            } else {
                format!("\"{}\"", encode_raw(value, Quote::Double))
            }
        }
    }
}

/// Spell `value` as literal text for a value word quoted as `quote`: nothing in it expands
fn encode_literal(value: &str, quote: Quote) -> String {
    match quote {
        Quote::Single => value.replace('\'', "'\\''"),
        Quote::Double => {
            let mut out = String::with_capacity(value.len());
            for c in value.chars() {
                match c {
                    // Portage's getconfig keeps `\`` as two characters, so a backquote is
                    // spelled in a single-quoted run, literal to both it and bash.
                    '`' => out.push_str("\"'`'\""),
                    '\\' | '"' | '$' => {
                        out.push('\\');
                        out.push(c);
                    }
                    _ => out.push(c),
                }
            }
            out
        }
        Quote::None => format!("\"{}\"", encode_literal(value, Quote::Double)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Serialises back to the source text; byte-identical to the input if no
/// edits were made via [`MakeConf::set`].
impl std::fmt::Display for MakeConf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for entry in &self.entries {
            match entry {
                Entry::Opaque(span) => f.write_str(&self.src[span.clone()])?,
                Entry::Statement { span, .. } => f.write_str(&self.src[span.clone()])?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> MakeConf {
        MakeConf::parse(src.to_owned()).expect("parse failed")
    }

    #[test]
    fn get_simple() {
        let mc = parse("CFLAGS=\"-O2\"\n");
        assert_eq!(mc.get("CFLAGS"), Some("-O2"));
    }

    #[test]
    fn get_unquoted() {
        let mc = parse("MAKEOPTS=-j8\n");
        assert_eq!(mc.get("MAKEOPTS"), Some("-j8"));
    }

    #[test]
    fn get_single_quoted() {
        let mc = parse("FOO='bar baz'\n");
        assert_eq!(mc.get("FOO"), Some("bar baz"));
    }

    #[test]
    fn get_with_var_ref() {
        let mc = parse("CFLAGS=\"${COMMON_FLAGS}\"\n");
        // get() returns the raw unexpanded value
        assert_eq!(mc.get("CFLAGS"), Some("${COMMON_FLAGS}"));
    }

    #[tokio::test]
    async fn apply_to_expands_common_flags_pattern() {
        let mc = parse("COMMON_FLAGS=\"-O2 -march=x\"\nCFLAGS=\"${COMMON_FLAGS}\"\n");
        let mut env = std::collections::BTreeMap::new();
        mc.apply_to(&mut env).await.unwrap();
        assert_eq!(env.get("CFLAGS").map(String::as_str), Some("-O2 -march=x"));
    }

    #[tokio::test]
    async fn apply_to_append_concatenates_without_space() {
        let mc = parse("CFLAGS=\"-O2\"\nCFLAGS+=\" -march=x\"\n");
        let mut env = std::collections::BTreeMap::new();
        mc.apply_to(&mut env).await.unwrap();
        assert_eq!(env.get("CFLAGS").map(String::as_str), Some("-O2 -march=x"));
    }

    #[tokio::test]
    async fn apply_to_unknown_var_expands_empty() {
        let mc = parse("CFLAGS=\"${NOT_SET}-O2\"\n");
        let mut env = std::collections::BTreeMap::new();
        mc.apply_to(&mut env).await.unwrap();
        assert_eq!(env.get("CFLAGS").map(String::as_str), Some("-O2"));
    }

    #[tokio::test]
    async fn apply_to_resolves_parameter_expansion_forms() {
        // A real shell (unlike the old hand-rolled `${VAR}`-only scanner)
        // understands `${NAME:-default}` too: unset `COMMON_FLAGS` falls
        // back to "fallback" instead of staying a literal string.
        let mc = parse("CFLAGS=\"${COMMON_FLAGS:-fallback}\"\n");
        let mut env = std::collections::BTreeMap::new();
        mc.apply_to(&mut env).await.unwrap();
        assert_eq!(env.get("CFLAGS").map(String::as_str), Some("fallback"));
    }

    #[tokio::test]
    async fn apply_to_seeded_env_is_visible_to_self_refs() {
        // A caller-seeded value (e.g. ambient CHOST) is visible to expansion,
        // and a plain assignment overwrites it (bash semantics).
        let mc = parse("GREETING=\"hello ${NAME}\"\n");
        let mut env = std::collections::BTreeMap::new();
        env.insert("NAME".to_string(), "world".to_string());
        mc.apply_to(&mut env).await.unwrap();
        assert_eq!(env.get("GREETING").map(String::as_str), Some("hello world"));
    }

    #[tokio::test]
    async fn apply_to_resolves_command_substitution() {
        // A real shell, unlike the old scanner, evaluates $(...) too.
        let mc = parse("GREETING=\"hello $(echo world)\"\n");
        let mut env = std::collections::BTreeMap::new();
        mc.apply_to(&mut env).await.unwrap();
        assert_eq!(env.get("GREETING").map(String::as_str), Some("hello world"));
    }

    #[test]
    fn get_last_wins() {
        let mc = parse("FOO=\"first\"\nFOO=\"second\"\n");
        assert_eq!(mc.get("FOO"), Some("second"));
    }

    #[test]
    fn get_missing() {
        let mc = parse("FOO=\"bar\"\n");
        assert_eq!(mc.get("BAR"), None);
    }

    #[test]
    fn comment_lines_preserved_in_roundtrip() {
        let src = "# This is a comment\nCFLAGS=\"-O2\"  # inline\nUSE=\"ssl\"\n";
        let mc = parse(src);
        assert_eq!(mc.to_string(), src);
    }

    #[test]
    fn blank_lines_preserved_in_roundtrip() {
        let src = "\nCFLAGS=\"-O2\"\n\nUSE=\"ssl\"\n";
        let mc = parse(src);
        assert_eq!(mc.to_string(), src);
    }

    #[test]
    fn multiline_value_preserved() {
        let src = "USE=\"\n    python\n    rust\n\"\n";
        let mc = parse(src);
        assert_eq!(mc.to_string(), src);
    }

    #[test]
    fn set_updates_value_preserves_inline_comment() {
        let src = "CFLAGS=\"-O2\"  # was -O2\n";
        let mut mc = parse(src);
        mc.set("CFLAGS", "-O3");
        let out = mc.to_string();
        assert!(out.contains("CFLAGS=\"-O3\""));
        assert!(out.contains("# was -O2"));
    }

    /// Sources `src` with bash and returns `name`'s value, or `None` if bash
    /// reports any error (as Portage's `getconfig` would refuse the file).
    fn bash_value(src: &str, name: &str) -> Option<String> {
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "set -e; eval \"$1\" 2>&1 >/dev/null; printf %s \"${{{name}}}\""
            ))
            .arg("make.conf")
            .arg(src)
            .env_clear()
            .output()
            .ok()?;
        (out.status.success() && out.stderr.is_empty())
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    #[test]
    fn set_quotes_a_plain_value_that_gains_a_space() {
        // `em use -a gtk` on `USE=ssl` must not write `USE=ssl gtk`, which runs
        // `gtk` and makes Portage reject make.conf ("Invalid token").
        let mut mc = parse("USE=ssl\nFEATURES=\"y\"\n");
        mc.apply_use_changes(&["gtk".to_string()], &[]);
        assert_eq!(mc.to_string(), "USE=\"ssl gtk\"\nFEATURES=\"y\"\n");
        assert_eq!(mc.get("USE"), Some("ssl gtk"));
        assert_eq!(mc.get("FEATURES"), Some("y"));
        // A second edit still finds the assignment instead of appending a new one.
        mc.apply_use_changes(&["qt".to_string()], &[]);
        assert_eq!(mc.to_string(), "USE=\"ssl gtk qt\"\nFEATURES=\"y\"\n");
    }

    #[test]
    fn set_keeps_a_plain_value_plain() {
        let mut mc = parse("MAKEOPTS=-j8\n");
        mc.set("MAKEOPTS", "-j4");
        assert_eq!(mc.to_string(), "MAKEOPTS=-j4\n");
        // Emptied, it is quoted: Portage reads a bare `NAME=` as taking the next token.
        mc.set("MAKEOPTS", "");
        assert_eq!(mc.to_string(), "MAKEOPTS=\"\"\n");
    }

    #[test]
    fn set_escapes_the_quote_of_the_value_word() {
        // A single quote inside single quotes, a double quote inside double
        // quotes: the file used to be cut short and stop parsing.
        let mut mc = parse("GENTOO_MIRRORS='https://old/'\nUSE=\"x\"\nFEATURES=\"y\"\n");
        mc.set("GENTOO_MIRRORS", "https://ex/it's/");
        let out = mc.to_string();
        assert!(out.ends_with("USE=\"x\"\nFEATURES=\"y\"\n"), "{out}");
        assert_eq!(
            bash_value(&out, "GENTOO_MIRRORS").as_deref(),
            Some("https://ex/it's/")
        );
        assert_eq!(bash_value(&out, "FEATURES").as_deref(), Some("y"));

        let mut mc = parse("A=\"old\"\nB=\"y\"\n");
        mc.set("A", "say \"hi\"");
        let out = mc.to_string();
        assert_eq!(bash_value(&out, "A").as_deref(), Some("say \"hi\""));
        assert_eq!(bash_value(&out, "B").as_deref(), Some("y"));
        // A trailing backslash would escape the closing quote.
        mc.set("A", "dir\\");
        let out = mc.to_string();
        assert_eq!(bash_value(&out, "B").as_deref(), Some("y"), "{out}");
    }

    #[test]
    fn set_keeps_references_in_a_raw_value() {
        let mut mc = parse("CFLAGS=${COMMON_FLAGS}\n");
        mc.set("CFLAGS", "${COMMON_FLAGS} -g");
        assert_eq!(mc.to_string(), "CFLAGS=\"${COMMON_FLAGS} -g\"\n");
        let mut mc = parse("CFLAGS=\"${COMMON_FLAGS}\"\n");
        mc.set("CFLAGS", "${COMMON_FLAGS} -g");
        assert_eq!(mc.to_string(), "CFLAGS=\"${COMMON_FLAGS} -g\"\n");
    }

    #[test]
    fn set_writes_an_unparseable_raw_value_as_a_literal() {
        let mut mc = parse("A=\"x\"\nB=\"y\"\n");
        mc.set("A", "$(oops");
        let out = mc.to_string();
        assert_eq!(bash_value(&out, "A").as_deref(), Some("$(oops"), "{out}");
        assert_eq!(bash_value(&out, "B").as_deref(), Some("y"));
    }

    #[test]
    fn set_appends_on_a_new_line_after_a_file_without_one() {
        let mut mc = parse("FOO=\"bar\"");
        mc.set("NEW_VAR", "a b");
        assert_eq!(mc.to_string(), "FOO=\"bar\"\nNEW_VAR=\"a b\"\n");
        assert_eq!(mc.get("FOO"), Some("bar"));
    }

    #[test]
    fn set_after_multibyte_text_edits_the_right_bytes() {
        // Source positions are byte offsets; text before the edit that is not
        // ASCII must not shift it.
        let src = "# José Müller — 編集\nNAME=\"Größe\" # ü\nGENTOO_MIRRORS=\"https://old/\" # é\nUSE=\"x\"\n";
        let mut mc = parse(src);
        assert_eq!(mc.to_string(), src);
        mc.set("GENTOO_MIRRORS", "https://new/");
        assert_eq!(
            mc.to_string(),
            "# José Müller — 編集\nNAME=\"Größe\" # ü\nGENTOO_MIRRORS=\"https://new/\" # é\nUSE=\"x\"\n"
        );
        assert_eq!(mc.get("NAME"), Some("Größe"));
    }

    #[test]
    fn set_writes_an_emptied_plain_value_as_empty_quotes() {
        // Portage's getconfig reads `USE=` followed by a newline as taking the next token.
        let mut mc = parse("USE=ssl\nFEATURES=\"y\"\n");
        mc.apply_use_changes(&[], &["ssl".to_string()]);
        assert_eq!(mc.to_string(), "USE=\"\"\nFEATURES=\"y\"\n");
        assert_eq!(
            bash_value(&mc.to_string(), "FEATURES").as_deref(),
            Some("y")
        );
    }

    #[test]
    fn export_assignment_is_edited_in_place() {
        let mut mc = parse("export USE=\"a\"\nFEATURES=\"y\"\n");
        assert_eq!(mc.get("USE"), Some("a"));
        mc.apply_use_changes(&["c".to_string()], &[]);
        assert_eq!(mc.to_string(), "export USE=\"a c\"\nFEATURES=\"y\"\n");
    }

    #[test]
    fn export_of_a_name_alone_is_not_an_assignment() {
        let mc = parse("export USE\n");
        assert_eq!(mc.get("USE"), None);
    }

    #[test]
    fn flag_words_drop_line_continuations() {
        let mut mc = parse("USE=\"a \\\n b\"\nFEATURES=\"y\"\n");
        assert_eq!(mc.apply_use_changes(&["c".to_string()], &[]), "a b c");
        assert_eq!(bash_value(&mc.to_string(), "USE").as_deref(), Some("a b c"));
        assert_eq!(flag_words("x\\\\ y"), vec!["x\\\\", "y"]);
    }

    #[test]
    fn set_literal_keeps_dollar_and_backslash() {
        let url = "https://a.example/$x/`id`/\\";
        for src in [
            "",
            "GENTOO_MIRRORS=old\n",
            "GENTOO_MIRRORS='old'\n",
            "GENTOO_MIRRORS=\"old\"\n",
        ] {
            let mut mc = parse(src);
            mc.set_literal("GENTOO_MIRRORS", url);
            assert_eq!(
                bash_value(&mc.to_string(), "GENTOO_MIRRORS").as_deref(),
                Some(url),
                "from {src:?}: {}",
                mc
            );
        }
    }

    #[test]
    fn set_appends_when_missing() {
        let src = "FOO=\"bar\"\n";
        let mut mc = parse(src);
        mc.set("NEW_VAR", "hello");
        let out = mc.to_string();
        assert!(out.contains("NEW_VAR=\"hello\""));
        assert!(out.contains("FOO=\"bar\""));
    }

    #[test]
    fn apply_use_changes_adds_and_removes() {
        let mut mc = parse("USE=\"ssl nls\"\n");
        let new_use = mc.apply_use_changes(&["python".to_string()], &["nls".to_string()]);
        assert_eq!(new_use, "ssl python");
        assert_eq!(mc.get("USE"), Some("ssl python"));
    }

    #[test]
    fn apply_use_changes_strips_leading_plus_and_dedupes() {
        let mut mc = parse("USE=\"-ssl\"\n");
        // Adding "+ssl" over an existing "-ssl" replaces it, not both.
        let new_use = mc.apply_use_changes(&["+ssl".to_string()], &[]);
        assert_eq!(new_use, "ssl");
    }

    #[test]
    fn apply_use_changes_on_empty_use() {
        let mut mc = parse("FOO=\"bar\"\n");
        let new_use = mc.apply_use_changes(&["nls".to_string()], &[]);
        assert_eq!(new_use, "nls");
        assert_eq!(mc.get("USE"), Some("nls"));
    }

    #[test]
    fn set_deduplicates() {
        let src = "A=\"first\"\nA=\"second\"\n";
        let mut mc = parse(src);
        mc.set("A", "third");
        let out = mc.to_string();
        assert_eq!(out.matches("A=").count(), 1);
        assert!(out.contains("A=\"third\""));
    }

    #[test]
    fn load_directory_concatenates_fragments_lexical_order() {
        let dir = tempfile::tempdir().unwrap();
        let mc_dir = dir.path().join("make.conf");
        std::fs::create_dir(&mc_dir).unwrap();
        std::fs::write(mc_dir.join("20-b"), "CFLAGS=\"-O3\"\n").unwrap();
        std::fs::write(mc_dir.join("10-a"), "USE=\"ssl\"\n").unwrap();
        std::fs::write(mc_dir.join(".hidden"), "USE=\"nope\"\n").unwrap();
        std::fs::write(mc_dir.join("30~"), "USE=\"backup\"\n").unwrap();

        let path = Utf8Path::from_path(&mc_dir).unwrap();
        let mc = MakeConf::load(path).unwrap();
        assert_eq!(mc.get("USE"), Some("ssl"));
        assert_eq!(mc.get("CFLAGS"), Some("-O3"));
    }

    #[test]
    fn expand_make_conf_paths_file_and_dir() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("etc/make.conf");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "A=1\n").unwrap();
        let modern = dir.path().join("etc/portage/make.conf");
        std::fs::create_dir_all(&modern).unwrap();
        std::fs::write(modern.join("10"), "B=2\n").unwrap();
        std::fs::write(modern.join("20"), "C=3\n").unwrap();

        // Portage order: legacy file first, then modern dir fragments.
        let files = expand_make_conf_paths([
            dir.path().join("etc/make.conf"),
            dir.path().join("etc/portage/make.conf"),
        ])
        .unwrap();
        assert_eq!(files.len(), 3);
        assert!(files[0].ends_with("make.conf"));
        assert!(files[1].ends_with("10"));
        assert!(files[2].ends_with("20"));
    }

    #[test]
    fn apply_use_changes_at_directory_patches_use_fragment() {
        let dir = tempfile::tempdir().unwrap();
        let mc_dir = dir.path().join("make.conf");
        std::fs::create_dir(&mc_dir).unwrap();
        std::fs::write(mc_dir.join("10-flags"), "CFLAGS=\"-O2\"\n").unwrap();
        std::fs::write(mc_dir.join("20-use"), "USE=\"ssl\"\n").unwrap();

        let path = Utf8Path::from_path(&mc_dir).unwrap();
        let new_use =
            MakeConf::apply_use_changes_at(path, "USE", &["nls".to_string()], &[], &[]).unwrap();
        assert_eq!(new_use, "ssl nls");
        // USE fragment updated; flags fragment untouched.
        let use_frag = std::fs::read_to_string(mc_dir.join("20-use")).unwrap();
        assert!(use_frag.contains("ssl nls"), "{use_frag}");
        assert_eq!(
            std::fs::read_to_string(mc_dir.join("10-flags")).unwrap(),
            "CFLAGS=\"-O2\"\n"
        );
    }

    #[test]
    fn apply_use_changes_at_empty_dir_creates_fallback_fragment() {
        let dir = tempfile::tempdir().unwrap();
        let mc_dir = dir.path().join("make.conf");
        std::fs::create_dir(&mc_dir).unwrap();
        let path = Utf8Path::from_path(&mc_dir).unwrap();
        let new_use =
            MakeConf::apply_use_changes_at(path, "USE", &["python".to_string()], &[], &[]).unwrap();
        assert_eq!(new_use, "python");
        let frag = mc_dir.join(MAKE_CONF_DIR_FALLBACK_FRAGMENT);
        assert!(frag.is_file());
        assert!(std::fs::read_to_string(frag).unwrap().contains("python"));
    }
}
