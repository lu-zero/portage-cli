//! Parser and editor for `/etc/portage/make.conf`.
//!
//! Uses `brush-parser`'s winnow-based parser for syntactic analysis of
//! variable assignments.  Comments are preserved precisely: brush-parser's
//! winnow parser now records comment byte spans into `Program.comments`, so
//! we know exactly where each comment lives without gap-filling heuristics.
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

use std::ops::Range;

use brush_parser::ast::{
    AssignmentName, AssignmentValue, Command, CommandPrefixOrSuffixItem, SourceLocation,
};
use brush_parser::{ParserOptions, SourceInfo};
use camino::Utf8Path;

use crate::{Error, Result};

/// Default path to the active make.conf.
pub const DEFAULT_MAKE_CONF: &str = "/etc/portage/make.conf";
/// Legacy path, used as a fallback when the default does not exist.
pub const LEGACY_MAKE_CONF: &str = "/etc/make.conf";

/// A parsed and editable make.conf file.
pub struct MakeConf {
    src: String,
    entries: Vec<Entry>,
}

/// One element in the tiled decomposition of the source file.
enum Entry {
    /// Raw bytes not covered by the AST: comments, blank lines, leading /
    /// trailing whitespace between statements.  Reproduced verbatim on
    /// serialisation.
    Opaque(Range<usize>),

    /// A bash statement that consists entirely of variable assignments
    /// (no command name).  Covers from the first token of the statement to
    /// the end of the `\n`-terminated logical line (including any trailing
    /// inline comment).
    Statement {
        /// Byte span in `src` for the full logical line.
        span: Range<usize>,
        vars: Vec<Var>,
    },
}

/// A single variable assignment within a statement.
struct Var {
    name: String,
    /// `true` for `NAME+=VALUE` (append), `false` for `NAME=VALUE` (assign).
    append: bool,
    /// Byte range of the raw value in `src`, **excluding** surrounding quotes.
    /// Empty range for `FOO=` (no value).
    value: Range<usize>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

impl MakeConf {
    /// Read and parse `path`.
    pub fn load(path: &Utf8Path) -> Result<Self> {
        let src = std::fs::read_to_string(path).map_err(|e| Error::Io {
            path: path.to_path_buf().into_std_path_buf(),
            source: e,
        })?;
        Self::parse(src)
    }

    /// Load whichever of [`DEFAULT_MAKE_CONF`] / [`LEGACY_MAKE_CONF`] exists.
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

    /// Parse from an owned string (useful for testing).
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

    /// Apply this file's assignments to `env` the way `bash source` would:
    /// sources the raw file text through a minimal, non-interactive
    /// [`brush_core::Shell`] (bash's standard builtin set only — no
    /// ebuild-specific builtins, no [`crate::Repository`] dependency, since
    /// make.conf is never ebuild content), then overlays every variable left
    /// in the shell afterward onto `env`.
    ///
    /// `env`'s current contents are seeded into the shell first, so a
    /// caller-provided value (e.g. an ambient `CHOST`) is visible to
    /// self-references and `NAME+=VALUE` appends onto it. Because this is a
    /// real shell, the full range of bash semantics applies: `${NAME}`/`$NAME`
    /// expansion, `${NAME:-default}`-style parameter expansion, command
    /// substitution, arithmetic, and so on — not just the `${NAME}` subset a
    /// hand-rolled scanner would need to special-case one construct at a
    /// time.
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
            .build()
            .await
            .map_err(|e| Error::Shell(e.to_string()))?;
        brush_builtins::register_default_builtins(&mut shell, brush_builtins::BuiltinSet::BashMode);

        for (name, value) in env.iter() {
            shell
                .set_env_global(name, brush_core::ShellVariable::new(value.as_str()))
                .map_err(|e| Error::Shell(e.to_string()))?;
        }

        let params = shell.default_exec_params();
        shell
            .run_string(
                self.src.as_str(),
                &brush_core::SourceInfo::from("make.conf"),
                &params,
            )
            .await
            .map_err(|e| Error::Shell(e.to_string()))?;

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

    /// Update `name` to `value`, preserving surrounding quotes and trailing
    /// comments.  Updates the first occurrence and removes any later
    /// duplicates (first occurrence wins after the edit).
    ///
    /// If `name` is not present, appends `NAME="value"\n` at the end.
    pub fn set(&mut self, name: &str, value: &str) {
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
                let append = format!("{}=\"{}\"\n", name, value);
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
                // it preserves the original quoting style untouched.
                self.src.replace_range(var.value.clone(), value);
                self.rebuild();
            }
        }
    }

    /// Add/remove USE flags in `USE`, portage's simple bare-flag model (no
    /// separate profile-level `+`/`-` handling here — a leading `+` on an
    /// `add`/`remove` token is stripped and ignored, matching `em useflags`'s
    /// own single-string USE editor). Returns the new `USE` value for the
    /// caller to report.
    pub fn apply_use_changes(&mut self, add: &[String], remove: &[String]) -> String {
        let current = self.get("USE").unwrap_or("").to_string();
        let mut flags: Vec<String> = current.split_whitespace().map(str::to_string).collect();

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

    /// Save to `path`.
    pub fn save(&self, path: &Utf8Path) -> Result<()> {
        std::fs::write(path, self.to_string()).map_err(|e| Error::Io {
            path: path.to_path_buf().into_std_path_buf(),
            source: e,
        })
    }

    /// Reparse `self.src` after an in-place mutation.
    fn rebuild(&mut self) {
        if let Ok(program) = parse_program(&self.src) {
            self.entries = build_entries(&self.src, &program);
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_program(src: &str) -> Result<brush_parser::ast::Program> {
    let opts = ParserOptions::default();
    brush_parser::winnow_str::parse_program(src, &opts, &SourceInfo::default())
        .map_err(|e| Error::Shell(e.to_string()))
}

/// Build the tiled [`Entry`] list for `src` given its parsed `program`.
///
/// Comment spans from `program.comments` are used to precisely extend each
/// statement span past any trailing inline comment before the final `\n`.
fn build_entries(src: &str, program: &brush_parser::ast::Program) -> Vec<Entry> {
    // Sorted comment end-positions for fast lookup.
    let mut comment_ends: Vec<usize> = program.comments.iter().map(|s| s.end.index).collect();
    comment_ends.sort_unstable();

    // Collect (logical-line-span, vars) for each pure-assignment statement.
    let mut stmts: Vec<(Range<usize>, Vec<Var>)> = Vec::new();

    for complete_cmd in &program.complete_commands {
        for item in &complete_cmd.0 {
            let Some(loc) = item.location() else {
                continue;
            };

            let start = loc.start.index;
            // Extend past any trailing comment on this line, then to the `\n`.
            let end = extend_to_newline(src, extend_past_comment(loc.end.index, &comment_ends));

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
            // Pure assignment: no command name.
            if simple.word_or_name.is_some() {
                continue;
            }

            let mut vars = Vec::new();
            if let Some(prefix) = &simple.prefix {
                for item in &prefix.0 {
                    if let CommandPrefixOrSuffixItem::AssignmentWord(assignment, _) = item
                        && let Some(v) = var_from_assignment(src, assignment)
                    {
                        vars.push(v);
                    }
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

/// Derive a [`Var`] from an AST `Assignment`, computing the value span from
/// `assignment.loc` and the name length (avoids relying on `Word.loc` which
/// may not always be populated).
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
    let token_start = a.loc.start.index;
    let token_end = a.loc.end.index;

    // Bytes consumed by the name and the `=` (plus `+` for append).
    let prefix_len = name.len() + if a.append { 2 } else { 1 };
    let value_start = token_start + prefix_len;

    if value_start >= token_end {
        return Some(Var {
            name,
            append: a.append,
            value: value_start..value_start,
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

    Some(Var {
        name,
        append: a.append,
        value: inner_start..inner_end,
    })
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
}
