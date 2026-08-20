//! Persistent "active" `--prefix` / `--local` for dogfooding
//!
//! Registers a default topology so bare `em <pkg>` picks up a prefix/local
//! without repeating flags every time.
//! Explicit `--prefix` / `--local` / `--root` always win over the registration.
//!
//! State lives under `$XDG_STATE_HOME/em/active` (default
//! `~/.local/state/em/active`). `em active env` prints shell exports so an
//! interactive session can also put the prefix's `usr/bin` on `PATH`.
//!
//! ## Multiple Registrations
//!
//! The state file supports multiple registered entries with an active pointer.
//! Uses TOML format (the `format` field is reserved for future migrations;
//! this layout starts at `format = 1` since no prior format was ever published):
//! ```toml
//! # em active registrations
//! format = 1
//! active = "my-prefix"
//!
//! [[entries]]
//! name = "my-prefix"
//! kind = "prefix"
//! path = "/home/user/gentoo-prefix"
//!
//! [[entries]]
//! name = "my-local"
//! kind = "local"
//! path = "/home/user/.gentoo"
//! ```
//!
//! Entries can be referenced by:
//! - **Name**: `em active set my-prefix`
//! - **Index**: `em active set 0` (0-based)
//! - **Path**: `em active set /home/user/.gentoo` (exact match)

use std::fmt;
use std::io::Write;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::cli::{ActiveCommand, Cli};
use crate::util::write_atomic;

/// State file format version
const FORMAT_VERSION: u32 = 1;

/// Kind of registered active topology
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActiveKind {
    /// `--prefix` overlay (BROOT stays the host; install target is the path)
    Prefix,
    /// `--local` standalone Gentoo-Prefix (own BROOT + EPREFIX)
    Local,
}

impl ActiveKind {
    fn as_str(self) -> &'static str {
        match self {
            ActiveKind::Prefix => "prefix",
            ActiveKind::Local => "local",
        }
    }
}

impl fmt::Display for ActiveKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single registered active topology entry
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveEntry {
    /// User-assigned or generated name for this entry
    pub name: String,
    /// The kind of topology (prefix or local)
    pub kind: ActiveKind,
    /// Absolute path to the prefix/local
    #[serde(with = "camino_utf8pathbuf")]
    pub path: Utf8PathBuf,
}

impl ActiveEntry {
    /// Create a new entry with a generated name from the path
    pub fn new(kind: ActiveKind, path: Utf8PathBuf) -> Self {
        // Generate a name from the path basename
        let name = path
            .file_name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| path.to_string());
        Self { name, kind, path }
    }

    /// Create a new entry with an explicit name
    pub fn with_name(name: String, kind: ActiveKind, path: Utf8PathBuf) -> Self {
        Self { name, kind, path }
    }

    /// Human-readable one-liner for display
    pub fn display_line(&self) -> String {
        format!("{} {}", self.kind, self.path)
    }
}

/// Serde helper for Utf8PathBuf
mod camino_utf8pathbuf {
    use camino::Utf8PathBuf;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(path: &Utf8PathBuf, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(path.as_str())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Utf8PathBuf, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Visitor;
        struct Utf8PathBufVisitor;
        impl<'de> Visitor<'de> for Utf8PathBufVisitor {
            type Value = Utf8PathBuf;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a UTF-8 path string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(Utf8PathBuf::from(value))
            }
        }
        deserializer.deserialize_str(Utf8PathBufVisitor)
    }
}

/// Reference to an entry - can be by name, index, or path
#[derive(Debug, Clone, PartialEq)]
pub enum EntryReference {
    /// Reference by entry name
    Name(String),
    /// Reference by 0-based index
    Index(usize),
    /// Reference by exact path
    Path(Utf8PathBuf),
}

impl FromStr for EntryReference {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        // Try to parse as index first
        if let Ok(index) = s.parse::<usize>() {
            return Ok(EntryReference::Index(index));
        }

        // Check if it looks like an absolute path
        if s.starts_with('/') {
            return Ok(EntryReference::Path(Utf8PathBuf::from(s)));
        }

        // Otherwise treat as name
        Ok(EntryReference::Name(s.to_string()))
    }
}

/// The complete active state: multiple entries with one active
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveStore {
    /// Format version for future migrations
    #[serde(default = "default_format")]
    pub format: u32,
    /// Name of the currently active entry (None means no active)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// All registered entries
    pub entries: Vec<ActiveEntry>,
}

fn default_format() -> u32 {
    FORMAT_VERSION
}

impl Default for ActiveStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ActiveStore {
    /// Create a new empty store
    pub fn new() -> Self {
        Self {
            format: FORMAT_VERSION,
            entries: Vec::new(),
            active: None,
        }
    }

    /// Get the currently active entry, if any
    pub fn active_entry(&self) -> Option<&ActiveEntry> {
        self.active
            .as_ref()
            .and_then(|name| self.entries.iter().find(|e| e.name == *name))
    }

    /// Find entry index by reference
    pub fn find_index(&self, reference: &EntryReference) -> Option<usize> {
        match reference {
            EntryReference::Name(name) => self.entries.iter().position(|e| e.name == *name),
            EntryReference::Index(index) => Some(*index),
            EntryReference::Path(path) => self.entries.iter().position(|e| &e.path == path),
        }
    }

    /// Set the active entry by reference
    pub fn set_active(&mut self, reference: &EntryReference) -> Result<()> {
        let name = match reference {
            EntryReference::Name(name) => {
                if self.entries.iter().any(|e| e.name == *name) {
                    Some(name.clone())
                } else {
                    bail!("no entry named {}", name);
                }
            }
            EntryReference::Index(index) => self
                .entries
                .get(*index)
                .map(|e| e.name.clone())
                .ok_or_else(|| anyhow::anyhow!("no entry at index {}", index))?
                .into(),
            EntryReference::Path(path) => self
                .entries
                .iter()
                .find(|e| &e.path == path)
                .map(|e| e.name.clone())
                .ok_or_else(|| anyhow::anyhow!("no entry with path {}", path))?
                .into(),
        };
        self.active = name;
        Ok(())
    }

    /// Add a new entry. Returns the entry's name
    pub fn add_entry(&mut self, entry: ActiveEntry) -> String {
        // Check if an entry with this path already exists
        if let Some(existing) = self.entries.iter_mut().find(|e| e.path == entry.path) {
            // Update existing entry's kind if different
            existing.kind = entry.kind;
            // Only adopt the new name if it isn't the auto-generated default for
            // this path, otherwise an `add` without an explicit name would
            // clobber a previously-set custom name.
            let generated = entry
                .path
                .file_name()
                .map(|n| n.to_string())
                .unwrap_or_else(|| entry.path.to_string());
            if entry.name != generated {
                existing.name = entry.name;
            }
            return existing.name.clone();
        }

        self.entries.push(entry);
        self.entries.last().unwrap().name.clone()
    }

    /// Remove an entry by reference
    pub fn remove_entry(&mut self, reference: &EntryReference) -> Result<ActiveEntry> {
        let index = self.find_index(reference).ok_or_else(|| {
            anyhow::anyhow!(
                "entry not found: {}",
                match reference {
                    EntryReference::Name(n) => n.clone(),
                    EntryReference::Index(i) => i.to_string(),
                    EntryReference::Path(p) => p.to_string(),
                }
            )
        })?;

        let entry = self.entries.remove(index);

        // If we removed the active entry, clear the active pointer
        if Some(&entry.name) == self.active.as_ref() {
            self.active = None;
        }

        Ok(entry)
    }

    /// Clear the active pointer but keep entries
    pub fn clear_active(&mut self) -> bool {
        let had_active = self.active.is_some();
        self.active = None;
        had_active
    }

    /// Clear all entries and active pointer
    pub fn clear_all(&mut self) -> usize {
        let count = self.entries.len();
        self.entries.clear();
        self.active = None;
        count
    }
}

/// `$XDG_STATE_HOME/em`, or `~/.local/state/em` when unset
///
/// Override via `XDG_STATE_HOME` (tests pin this to a temp dir).
pub fn state_dir() -> Utf8PathBuf {
    crate::xdg::em_state_dir()
}

/// Path of the active-state file
pub fn state_file() -> Utf8PathBuf {
    state_dir().join("active")
}

/// Load the active state from disk
///
/// Returns `Ok(None)` when no file exists.
pub fn load() -> Result<Option<ActiveStore>> {
    load_store()
}

/// Internal function to load the store
pub(crate) fn load_store() -> Result<Option<ActiveStore>> {
    let path = state_file();
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path.as_std_path())
        .with_context(|| format!("reading active state {path}"))?;

    parse_state_toml(&text).map(Some)
}

/// Load the active context
///
/// Returns the currently active entry as an ActiveContext, or None if no active entry.
pub fn load_active_context() -> Result<Option<ActiveContext>> {
    let store = load()?;
    Ok(match store {
        Some(ref s) => s.active_entry().map(|e| ActiveContext {
            kind: e.kind,
            path: e.path.clone(),
        }),
        None => None,
    })
}

/// Persist the entire store to disk
pub fn save(store: &ActiveStore) -> Result<()> {
    let dir = state_dir();
    std::fs::create_dir_all(dir.as_std_path())
        .with_context(|| format!("creating active state dir {dir}"))?;

    let body = format_state_toml(store)?;
    write_atomic(&state_file(), body)
}

/// Format store as TOML string
fn format_state_toml(store: &ActiveStore) -> Result<String> {
    let mut out = String::new();
    out.push_str("# em active registrations\n");
    let body = toml::to_string(store).context("serializing active state to TOML")?;
    out.push_str(&body);
    Ok(out)
}

/// Parse TOML state file format
fn parse_state_toml(text: &str) -> Result<ActiveStore> {
    let store: ActiveStore = toml::from_str(text)
        .map_err(|e| anyhow::anyhow!("active state: failed to parse TOML: {}", e))?;

    // Validate format version
    if store.format > FORMAT_VERSION {
        bail!(
            "active state: unsupported format version {} (max {})",
            store.format,
            FORMAT_VERSION
        );
    }

    // Validate all paths are absolute
    for entry in &store.entries {
        if !entry.path.is_absolute() {
            bail!("active state: path must be absolute, got {}", entry.path);
        }
    }

    Ok(store)
}

/// Legacy ActiveContext for backward compatibility with existing code
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveContext {
    pub kind: ActiveKind,
    pub path: Utf8PathBuf,
}

impl From<&ActiveEntry> for ActiveContext {
    fn from(entry: &ActiveEntry) -> Self {
        ActiveContext {
            kind: entry.kind,
            path: entry.path.clone(),
        }
    }
}

/// Resolve a user-supplied path to an absolute UTF-8 path
///
/// Canonicalizes when the path exists so `../foo` registrations stay stable;
/// otherwise joins against the current working directory.
pub fn absolutize(path: &Utf8Path) -> Result<Utf8PathBuf> {
    if path.as_str().is_empty() {
        bail!("active path must not be empty");
    }
    if path.exists() {
        let canon = path
            .canonicalize_utf8()
            .with_context(|| format!("canonicalizing {path}"))?;
        return Ok(canon);
    }
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    let cwd = std::env::current_dir().context("reading current directory")?;
    let joined = cwd.join(path.as_str());
    Utf8PathBuf::from_path_buf(joined)
        .map_err(|p| anyhow::anyhow!("active path is not valid UTF-8: {}", p.display()))
}

/// Register `path` as an available entry, without activating it
///
/// `em setup` calls this so a prefix it just created shows up in
/// `em active list` immediately: the registry exists so you can *choose*
/// between prefixes, which first requires knowing they are there. Creating one
/// by hand and then having to re-state its path to register it is busywork.
///
/// The active pointer is deliberately left alone. Building a prefix is not a
/// statement that you want bare `em` to start using it, and silently
/// repointing the default topology at a new tree is exactly the kind of
/// surprise the explicit-registration design was meant to avoid.
///
/// Idempotent: re-running setup on the same directory updates that entry in
/// place (keeping a name the user chose) rather than adding a duplicate.
/// Returns the entry name, or `None` when no state could be written — a
/// read-only `$XDG_STATE_HOME` must not fail a setup that otherwise worked.
pub fn register_available(kind: ActiveKind, path: &Utf8Path) -> Option<String> {
    let path = absolutize(path).ok()?;
    let mut store = load().ok().flatten().unwrap_or_default();
    let already_active = store.active_entry().map(|e| e.path.clone());
    let name = store.add_entry(ActiveEntry::new(kind, path));
    // `add_entry` never changes the pointer, but assert the intent here so a
    // future change to it cannot quietly start hijacking the active context.
    debug_assert_eq!(store.active_entry().map(|e| e.path.clone()), already_active);
    save(&store).ok()?;
    Some(name)
}

/// Default `--local` path (`~/.gentoo`), matching [`Cli`]'s flag semantics
pub fn default_local_path() -> Utf8PathBuf {
    crate::xdg::home().join(".gentoo")
}

/// `PATH` entries contributed by the prefix's `etc/env.d`, highest priority
/// first.
///
/// Packages that live outside `usr/bin` reach `PATH` only this way — LLVM is
/// the worked example: every `llvm-core/llvm` slot installs its binaries under
/// `usr/lib/llvm/<slot>/bin` and ships an `env.d/60llvm-NNNN` to advertise
/// them, and `em select clang` writes a `59llvm-selected` that sorts ahead to
/// express a choice. Without reading env.d, a prefix's compilers are installed
/// but unreachable, and any selection is inert.
///
/// Files are read in name order, which *is* the priority order by construction
/// (that is what the numeric prefixes are for). Only `PATH` is consumed here:
/// `LDPATH` belongs to `ld.so.conf` via `env-update`, not to a shell.
fn env_d_path_entries(prefix: &Utf8Path) -> Vec<String> {
    let dir = prefix.join("etc/env.d");
    let Ok(entries) = std::fs::read_dir(dir.as_std_path()) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .collect();
    files.sort();

    let mut out = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in content.lines() {
            let line = line.trim();
            let Some(value) = line.strip_prefix("PATH=") else {
                continue;
            };
            for entry in value.trim_matches('"').split(':').filter(|e| !e.is_empty()) {
                if !out.iter().any(|e| e == entry) {
                    out.push(entry.to_string());
                }
            }
        }
    }
    out
}

/// Directories from the prefix's `etc/ld.so.conf`, via the `ldconfig` crate
/// (same as `maint/env.rs`'s `refresh_ld_cache`). Deliberately excludes the
/// host's own `/etc/ld.so.conf` — see todo/for-sonnet.md 2026-08-08.
fn ld_so_conf_entries(prefix: &Utf8Path) -> Vec<String> {
    let conf = prefix.join("etc/ld.so.conf");
    ldconfig::SearchPaths::from_file(&conf, None)
        .map(|paths| paths.iter().map(|p| p.to_string()).collect())
        .unwrap_or_default()
}

/// Shell snippet suitable for `eval "$(em active env)"` (bash/zsh)
///
/// Prepends the prefix's `etc/env.d` `PATH` entries — which is how anything
/// outside `usr/bin` becomes reachable, and how an `em select clang` choice
/// takes effect — followed by the prefix's own `usr/bin` and `bin`, exports
/// `LD_LIBRARY_PATH`, and exports the `EM_ACTIVE_KIND` / `EM_ACTIVE_PATH`
/// markers. Does **not** set `EPREFIX` globally — that would confuse host
/// tools; `em` itself reads the state file.
pub fn env_exports(ctx: &ActiveContext) -> String {
    let p = ctx.path.as_str();
    // Escape path components for use inside double quotes, then append the
    // `${PATH:+:$PATH}` expansion *unescaped* so the shell still expands it.
    let mut parts: Vec<String> = env_d_path_entries(&ctx.path)
        .iter()
        .map(|e| shell_escape_double_inner(e))
        .collect();
    parts.push(shell_escape_double_inner(&format!("{p}/usr/bin")));
    parts.push(shell_escape_double_inner(&format!("{p}/bin")));
    let path = parts.join(":");

    let ld_library_path = ld_so_conf_entries(&ctx.path)
        .iter()
        .map(|e| shell_escape_double_inner(e))
        .collect::<Vec<_>>()
        .join(":");
    let ld_library_path_export = if ld_library_path.is_empty() {
        String::new()
    } else {
        format!(
            "export LD_LIBRARY_PATH=\"{ld_library_path}${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}\"\n"
        )
    };

    format!(
        "# em active env — eval this in bash/zsh\n\
         export EM_ACTIVE_KIND={}\n\
         export EM_ACTIVE_PATH={}\n\
         export PATH=\"{path}${{PATH:+:$PATH}}\"\n\
         {ld_library_path_export}",
        ctx.kind.as_str(),
        shell_double_quote(p),
    )
}

/// Double-quote a string for POSIX shell (escape `\`, `"`, `$`, `` ` ``)
fn shell_double_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&shell_escape_double_inner(s));
    out.push('"');
    out
}

/// Escape characters that are special inside double-quoted POSIX shell strings
fn shell_escape_double_inner(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '"' | '$' | '`' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Dispatch `em active …`
pub fn run(command: Option<&ActiveCommand>, globals: &Cli) -> Result<()> {
    match command {
        None | Some(ActiveCommand::Show) => run_show(),
        Some(ActiveCommand::Set { reference }) => run_set(reference, globals),
        Some(ActiveCommand::Clear { all }) => run_clear(*all),
        Some(ActiveCommand::Env) => run_env(),
        Some(ActiveCommand::List) => run_list(),
        Some(ActiveCommand::Add { name }) => run_add(name, globals),
        Some(ActiveCommand::Remove { reference }) => run_remove(reference),
    }
}

fn run_show() -> Result<()> {
    let store = match load()? {
        Some(s) => s,
        None => {
            println!("(no active prefix/local registered)");
            println!("Register one with: em --prefix DIR active set");
            println!("                 or: em --local= active set");
            println!("                 or: em --local /path active set");
            return Ok(());
        }
    };

    match store.active_entry() {
        Some(entry) => {
            println!("{}", entry.display_line());
        }
        None => {
            println!("(no active prefix/local registered)");
            if !store.entries.is_empty() {
                println!("Available entries:");
                for (i, entry) in store.entries.iter().enumerate() {
                    println!("  {}: {} ({})", i, entry.display_line(), entry.name);
                }
                println!("Activate one with: em active set <name|index|path>");
            } else {
                println!("Register one with: em --prefix DIR active set");
                println!("                 or: em --local= active set");
                println!("                 or: em --local /path active set");
            }
        }
    }
    Ok(())
}

fn run_list() -> Result<()> {
    let store = match load()? {
        Some(s) => s,
        None => {
            println!("(no entries registered)");
            return Ok(());
        }
    };

    if store.entries.is_empty() {
        println!("(no entries registered)");
        return Ok(());
    }

    for (i, entry) in store.entries.iter().enumerate() {
        let marker = if Some(&entry.name) == store.active.as_ref() {
            " *"
        } else {
            "  "
        };
        println!(
            "{} {}: {} ({}) {}",
            marker, i, entry.name, entry.kind, entry.path
        );
    }

    if let Some(ref active_name) = store.active {
        if let Some(active) = store.active_entry() {
            println!("\nActive: {} ({})", active_name, active.display_line());
        }
    } else {
        println!("\nNo active entry set");
    }

    Ok(())
}

fn run_set(reference: &Option<String>, globals: &Cli) -> Result<()> {
    // If reference is provided, activate existing entry
    if let Some(ref_str) = reference {
        let ref_parsed: EntryReference = ref_str
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid reference: {}", ref_str))?;

        let mut store = load()?.unwrap_or_default();
        store.set_active(&ref_parsed)?;
        save(&store)?;

        let active = store.active_entry().unwrap();
        println!("active {} → {}", active.kind, active.path);
        println!("bare `em` now uses this context; override with --prefix/--local/--root");
        println!("shell PATH: eval \"$(em active env)\"");
        return Ok(());
    }

    // Otherwise, register new entry from flags (legacy behavior)
    let ctx = resolve_set_target(globals)?;
    let path_str = ctx.path.to_string();
    if !ctx.path.exists() {
        crate::style::warn_line!(
            "{} does not exist yet — register it anyway (run em setup first if needed)",
            ctx.path
        );
    }

    let mut store = load()?.unwrap_or_default();
    let entry = ActiveEntry::new(ctx.kind, ctx.path);
    let name = store.add_entry(entry);
    store.active = Some(name.clone());
    save(&store)?;

    println!("active {} → {}", ctx.kind, path_str);
    println!("bare `em` now uses this context; override with --prefix/--local/--root");
    println!("shell PATH: eval \"$(em active env)\"");
    Ok(())
}

fn run_clear(all: bool) -> Result<()> {
    if all {
        let mut store = load()?.unwrap_or_default();
        let count = store.clear_all();
        if count > 0 {
            save(&store)?;
            println!("cleared {} entries", count);
        } else {
            println!("(no entries to clear)");
        }
    } else {
        let mut store = load()?.unwrap_or_default();
        if store.clear_active() {
            save(&store)?;
            println!("cleared active context");
        } else {
            println!("(no active context to clear)");
        }
    }
    Ok(())
}

fn run_env() -> Result<()> {
    let store = load()?.ok_or_else(|| {
        anyhow::anyhow!(
            "no active prefix/local registered — run \\n             `em --prefix DIR active set` (or `em --local active set`) first"
        )
    })?;

    let active = store.active_entry().ok_or_else(|| {
        anyhow::anyhow!("no active entry set — use `em active set <name|index|path>` first")
    })?;

    let ctx: ActiveContext = active.into();
    let mut out = std::io::stdout().lock();
    write!(out, "{}", env_exports(&ctx))?;
    Ok(())
}

fn run_add(name: &Option<String>, globals: &Cli) -> Result<()> {
    let ctx = resolve_set_target(globals)?;
    let path_str = ctx.path.to_string();
    if !ctx.path.exists() {
        crate::style::warn_line!(
            "{} does not exist yet — register it anyway (run em setup first if needed)",
            ctx.path
        );
    }

    let entry_name = name.clone().unwrap_or_else(|| {
        ctx.path
            .file_name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| path_str.clone())
    });

    let mut store = load()?.unwrap_or_default();
    let entry = ActiveEntry::with_name(entry_name.clone(), ctx.kind, ctx.path);
    let added_name = store.add_entry(entry);

    // If this is the first entry, auto-activate it
    if store.entries.len() == 1 {
        store.active = Some(added_name.clone());
    }

    save(&store)?;

    println!("added {}: {} {}", added_name, ctx.kind, path_str);
    if store.active == Some(added_name.clone()) {
        println!("  (auto-activated as first entry)");
    }
    println!("Activate with: em active set {}", added_name);
    Ok(())
}

fn run_remove(reference: &String) -> Result<()> {
    let ref_parsed: EntryReference = reference
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid reference: {}", reference))?;

    let mut store = load()?.unwrap_or_default();
    if store.entries.is_empty() {
        println!("(no entries registered)");
        return Ok(());
    }

    let entry = store.remove_entry(&ref_parsed)?;
    save(&store)?;

    println!("removed {}: {}", entry.name, entry.display_line());
    Ok(())
}

/// Resolve what `em active set` should register from the global flags
///
/// Precedence: `--local` > `--prefix` (matches [`Cli::topology_source`]).
/// `--root` is intentionally not registerable — active is for unprivileged
/// prefix/local dogfooding only.
fn resolve_set_target(globals: &Cli) -> Result<ActiveContext> {
    if let Some(local) = globals.local.as_deref() {
        let path = if local.is_empty() {
            default_local_path()
        } else {
            absolutize(Utf8Path::new(local))?
        };
        let path = finalize_abs_path(path)?;
        return Ok(ActiveContext {
            kind: ActiveKind::Local,
            path,
        });
    }
    if let Some(p) = globals.prefix.as_deref() {
        let path = finalize_abs_path(absolutize(Utf8Path::new(p))?)?;
        return Ok(ActiveContext {
            kind: ActiveKind::Prefix,
            path,
        });
    }
    bail!(
        "em active set/add needs a target: pass --prefix DIR or --local [DIR]\\n\
         examples:\\n  em --prefix /home/me/prefix active set\\n  em --local= active set\\n  em --local /other active set\\n\
         note: bare `em --local active set` steals `active` as the path — use `em --local=` or put a path"
    );
}

/// Canonicalize when the path exists; otherwise keep the absolute form
fn finalize_abs_path(path: Utf8PathBuf) -> Result<Utf8PathBuf> {
    if path.exists() {
        path.canonicalize_utf8()
            .with_context(|| format!("canonicalizing {path}"))
    } else if path.is_absolute() {
        Ok(path)
    } else {
        absolutize(&path)
    }
}

#[cfg(test)]
mod tests {

    // `em setup` registers what it built so it is selectable, but must not
    // take over the active pointer — creating a prefix is not a decision to
    // start using it.
    #[test]
    fn registering_an_entry_never_moves_the_active_pointer() {
        let mut store = ActiveStore::new();
        store.add_entry(ActiveEntry::new(
            ActiveKind::Local,
            Utf8PathBuf::from("/home/u/.gentoo"),
        ));
        store
            .set_active(&EntryReference::Path(Utf8PathBuf::from("/home/u/.gentoo")))
            .unwrap();
        let before = store.active_entry().unwrap().path.clone();

        let name = store.add_entry(ActiveEntry::new(
            ActiveKind::Prefix,
            Utf8PathBuf::from("/var/tmp/newprefix"),
        ));
        assert_eq!(name, "newprefix", "name comes from the directory");
        assert_eq!(
            store.active_entry().unwrap().path,
            before,
            "adding must not hijack the active context"
        );
        assert_eq!(store.entries.len(), 2);

        // Re-running setup on the same directory updates in place.
        store.add_entry(ActiveEntry::new(
            ActiveKind::Prefix,
            Utf8PathBuf::from("/var/tmp/newprefix"),
        ));
        assert_eq!(store.entries.len(), 2, "no duplicate for the same path");
    }

    // env.d is how a prefix's compilers become reachable at all, and the
    // numeric prefixes *are* the priority order — an `em select clang`
    // choice (`59llvm-selected`) has to land ahead of the slot's own
    // `60llvm-*`, and both ahead of plain `usr/bin`.
    #[test]
    fn env_exports_honour_env_d_priority_order() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = camino::Utf8Path::from_path(dir.path()).unwrap().to_owned();
        let env_d = prefix.join("etc/env.d");
        std::fs::create_dir_all(env_d.as_std_path()).unwrap();
        std::fs::write(
            env_d.join("60llvm-9977").as_std_path(),
            "PATH=\"/p/usr/lib/llvm/22/bin\"\nLDPATH=\"/p/usr/lib/llvm/22/lib64\"\n",
        )
        .unwrap();
        std::fs::write(
            env_d.join("59llvm-selected").as_std_path(),
            "PATH=\"/p/usr/lib/llvm/21/bin\"\n",
        )
        .unwrap();

        let ctx = ActiveContext {
            kind: ActiveKind::Prefix,
            path: prefix.clone(),
        };
        let out = env_exports(&ctx);
        let path_line = out
            .lines()
            .find(|l| l.starts_with("export PATH="))
            .expect("PATH export");

        let selected = path_line.find("/p/usr/lib/llvm/21/bin").expect("selection");
        let slot_default = path_line.find("/p/usr/lib/llvm/22/bin").expect("60llvm");
        let usr_bin = path_line
            .find(&format!("{prefix}/usr/bin"))
            .expect("usr/bin");
        assert!(
            selected < slot_default && slot_default < usr_bin,
            "selection must outrank the slot default, both ahead of usr/bin: {path_line}"
        );
        // LDPATH is ld.so.conf's business, not a shell's.
        assert!(!path_line.contains("lib64"), "{path_line}");
    }

    use super::*;
    use crate::test_support::home_lock;
    use clap::Parser;

    struct StateGuard {
        _home: std::sync::MutexGuard<'static, ()>,
        saved_xdg: Option<String>,
        saved_home: Option<String>,
    }

    impl StateGuard {
        fn new(state_parent: &Utf8Path, home: Option<&Utf8Path>) -> Self {
            let _home = home_lock();
            let saved_xdg = std::env::var("XDG_STATE_HOME").ok();
            let saved_home = std::env::var("HOME").ok();
            unsafe {
                std::env::set_var("XDG_STATE_HOME", state_parent.as_str());
                if let Some(h) = home {
                    std::env::set_var("HOME", h.as_str());
                }
            }
            Self {
                _home,
                saved_xdg,
                saved_home,
            }
        }
    }

    impl Drop for StateGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.saved_xdg {
                    Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                    None => std::env::remove_var("XDG_STATE_HOME"),
                }
                match &self.saved_home {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    #[test]
    fn parse_toml_round_trip() {
        let toml_text = r#"
# comment
format = 1
active = "my-prefix"

[[entries]]
name = "my-prefix"
kind = "prefix"
path = "/opt/p"

[[entries]]
name = "another"
kind = "local"
path = "/home/u/.gentoo"
"#;
        let store = parse_state_toml(toml_text).unwrap();
        assert_eq!(store.active, Some("my-prefix".to_string()));
        assert_eq!(store.entries.len(), 2);
    }

    #[test]
    fn format_toml_round_trip() {
        let mut store = ActiveStore::new();
        store.add_entry(ActiveEntry::with_name(
            "test".to_string(),
            ActiveKind::Prefix,
            Utf8PathBuf::from("/test/path"),
        ));
        store.active = Some("test".to_string());
        let formatted = format_state_toml(&store).unwrap();
        let parsed = parse_state_toml(&formatted).unwrap();
        assert_eq!(parsed.active, store.active);
        assert_eq!(parsed.entries.len(), store.entries.len());
    }

    #[test]
    fn entry_reference_parsing() {
        assert_eq!(
            "0".parse::<EntryReference>().unwrap(),
            EntryReference::Index(0)
        );
        assert_eq!(
            "my-name".parse::<EntryReference>().unwrap(),
            EntryReference::Name("my-name".to_string())
        );
        assert_eq!(
            "/path/to/thing".parse::<EntryReference>().unwrap(),
            EntryReference::Path(Utf8PathBuf::from("/path/to/thing"))
        );
    }

    #[test]
    fn store_operations() {
        let mut store = ActiveStore::new();
        store.add_entry(ActiveEntry::with_name(
            "alpha".to_string(),
            ActiveKind::Prefix,
            Utf8PathBuf::from("/alpha"),
        ));
        store.add_entry(ActiveEntry::with_name(
            "beta".to_string(),
            ActiveKind::Local,
            Utf8PathBuf::from("/beta"),
        ));

        store
            .set_active(&EntryReference::Name("beta".to_string()))
            .unwrap();
        assert_eq!(store.active, Some("beta".to_string()));

        store.set_active(&EntryReference::Index(0)).unwrap();
        assert_eq!(store.active, Some("alpha".to_string()));

        let removed = store
            .remove_entry(&EntryReference::Name("alpha".to_string()))
            .unwrap();
        assert_eq!(removed.name, "alpha");
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.active, None);
    }

    #[test]
    fn save_load_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let _g = StateGuard::new(&parent, None);

        let mut store = ActiveStore::new();
        store.add_entry(ActiveEntry::with_name(
            "test".to_string(),
            ActiveKind::Prefix,
            Utf8PathBuf::from("/tmp/my-prefix"),
        ));
        store.active = Some("test".to_string());
        save(&store).unwrap();

        let loaded = load().unwrap().unwrap();
        assert_eq!(loaded.active, Some("test".to_string()));
        assert_eq!(loaded.entries.len(), 1);
    }

    #[test]
    fn env_exports_contain_path_and_kind() {
        let ctx = ActiveContext {
            kind: ActiveKind::Local,
            path: Utf8PathBuf::from("/home/me/.gentoo"),
        };
        let s = env_exports(&ctx);
        assert!(s.contains("EM_ACTIVE_KIND=local"));
        assert!(s.contains("/home/me/.gentoo"));
        assert!(s.contains("/home/me/.gentoo/usr/bin"));
        assert!(s.contains("export PATH="));
    }

    #[test]
    fn set_via_global_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let prefix = parent.join("gpfx");
        std::fs::create_dir_all(prefix.as_std_path()).unwrap();
        let _g = StateGuard::new(&parent, None);

        let cli = Cli::parse_from(["em", "--prefix", prefix.as_str(), "active", "set"]);
        match &cli.applet {
            Some(crate::cli::Applet::Active { command }) => {
                run(command.as_ref(), &cli).unwrap();
            }
            _ => panic!("expected Active applet"),
        }
        let loaded = load().unwrap().unwrap();
        let entry = loaded.active_entry().unwrap();
        assert_eq!(entry.kind, ActiveKind::Prefix);
        assert_eq!(entry.path, prefix.canonicalize_utf8().unwrap());
    }

    #[test]
    fn shell_double_quote_escapes() {
        assert_eq!(shell_double_quote(r#"a$b"c`d\e"#), r#""a\$b\"c\`d\\e""#);
    }

    #[test]
    fn env_exports_leave_path_expansion_unescaped() {
        let ctx = ActiveContext {
            kind: ActiveKind::Prefix,
            path: Utf8PathBuf::from("/opt/p"),
        };
        let s = env_exports(&ctx);
        assert!(
            s.contains(r#"export PATH="/opt/p/usr/bin:/opt/p/bin${PATH:+:$PATH}""#),
            "unexpected env export:\n{s}"
        );
    }

    // Live-verified, todo/for-sonnet.md 2026-08-08
    #[test]
    fn env_exports_include_ld_library_path_from_prefix_ld_so_conf() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = camino::Utf8Path::from_path(dir.path()).unwrap().to_owned();
        std::fs::create_dir_all(prefix.join("etc").as_std_path()).unwrap();
        std::fs::write(
            prefix.join("etc/ld.so.conf").as_std_path(),
            "# Autogenerated by em env-update; do not edit (see /etc/env.d).\n\
             include ld.so.conf.d/*.conf\n\
             /p/usr/lib/gcc/aarch64-unknown-linux-gnu/16\n\
             /p/usr/lib64\n",
        )
        .unwrap();

        let ctx = ActiveContext {
            kind: ActiveKind::Prefix,
            path: prefix,
        };
        let s = env_exports(&ctx);
        let ld_line = s
            .lines()
            .find(|l| l.starts_with("export LD_LIBRARY_PATH="))
            .expect("LD_LIBRARY_PATH export");
        assert!(ld_line.contains("/p/usr/lib/gcc/aarch64-unknown-linux-gnu/16"));
        assert!(ld_line.contains("/p/usr/lib64"));
        assert!(
            !ld_line.contains("ld.so.conf.d"),
            "must not treat the include directive as a directory: {ld_line}"
        );
        assert!(
            ld_line.ends_with(r#"${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}""#),
            "must not clobber a pre-existing LD_LIBRARY_PATH: {ld_line}"
        );
    }

    // No `ld.so.conf`: no empty `LD_LIBRARY_PATH=""` export
    #[test]
    fn env_exports_omit_ld_library_path_line_when_ld_so_conf_absent() {
        let ctx = ActiveContext {
            kind: ActiveKind::Local,
            path: Utf8PathBuf::from("/home/me/.gentoo"),
        };
        let s = env_exports(&ctx);
        assert!(
            !s.contains("LD_LIBRARY_PATH"),
            "no ld.so.conf on disk: must not mention LD_LIBRARY_PATH at all: {s}"
        );
    }
}
