//! Diff-and-apply plan for `em`-generated config files.
//!
//! `crossdev --init-target` used to write every file unconditionally and
//! immediately, with no way to preview or confirm it — unlike every other
//! mutating `em` path, which honours `-p`/`--pretend` (global on `Cli`) and
//! `-a`/`--ask` (`MergeFlags`, resolved by the caller via `merge_merge_flags`
//! — not global, since unlike `--pretend` it has no meaning outside a
//! merge-shaped command; see `MergeFlags::ask`'s doc comment). This collects
//! the desired state of each file/symlink/dir as a [`ConfigEntry`] (no I/O
//! beyond validation), diffs it against what's actually on disk, and only
//! then previews (`-p`), confirms (`-a`), or applies — the config-
//! regeneration equivalent of a merge plan. Shared between `crossdev` (cross
//! sysroot config) and `setup` (`--local`/`--prefix` repos.conf/make.profile).

use std::io::Write;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use crate::util::write_if_absent;

/// The full `[crossdev.<tuple>]` `Location::Alias` body for
/// `name`/`category`/`packages_line` — the single formatter both
/// `ConfigEntry::Alias`'s `change()` (comparison) and `apply()` (write) use,
/// so they can never drift apart from each other. `name` is per-target
/// (`crossdev.<tuple>`, see `crossdev::overlay_name`) so each cross target
/// gets its own section/file and multiple targets can coexist on one prefix
/// instead of the last `--setup`/`--init-target` silently orphaning the
/// previous target's alias.
fn alias_body(name: &str, category: &str, packages_line: &str) -> String {
    format!(
        "[{name}]\nalias-source = gentoo\nalias-target = {category}\n\
         alias-packages = {packages_line}\n"
    )
}

/// One file/dir/symlink a caller wants in a particular state.
#[derive(Debug)]
pub(crate) enum ConfigEntry {
    /// Regenerated every run: em owns the full content, so a rewrite always
    /// wins over whatever is currently on disk.
    File { path: Utf8PathBuf, desired: String },
    /// Written only if nothing is there yet; an existing file (any content)
    /// is left alone — either it never legitimately drifts (a bare location
    /// string), or it may belong to something else entirely em must not
    /// clobber.
    CreateOnly { path: Utf8PathBuf, desired: String },
    /// A `Location::Alias` `[crossdev.<tuple>]` repos.conf entry: refreshed
    /// when it's recognisably em's own (has an `alias-target =` key) but
    /// stale, left alone when it's foreign (e.g. a real crossdev/eselect-
    /// managed physical overlay with a `location =` key instead). `name` is
    /// per-target (see `crossdev::overlay_name`), so it also doubles as the
    /// filename stem — each target's alias lives in its own file/section.
    Alias {
        path: Utf8PathBuf,
        name: String,
        category: String,
        packages_line: String,
    },
    /// A directory that just needs to exist (e.g. an empty target VDB).
    Dir { path: Utf8PathBuf },
    /// A symlink that should point at `target`.
    Symlink {
        link: Utf8PathBuf,
        target: Utf8PathBuf,
    },
}

enum Change {
    Create,
    Update,
    Unchanged,
}

/// How aggressively to reconcile a [`ConfigEntry`] plan against what's
/// already on disk.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshPolicy {
    /// Always regenerate to match the freshly-computed desired state.
    /// Explicit `--init-target`: an intentional "make this exactly right"
    /// action, including picking up a changed package set or `--ex-pkg`
    /// selection and re-detecting drift in a hand-edited file.
    Sync,
    /// Only create what's missing; anything already on disk — hand-edited
    /// or not — is left untouched, whatever its content. `--setup`'s own
    /// implied config-laydown step: a hand edit made between an earlier
    /// explicit `--init-target` and this `--setup` must survive. Trade-off:
    /// `--setup --ex-pkg X` against an *already-initialized* target won't
    /// pick up the new extra either — run `--init-target --ex-pkg X`
    /// (`Sync`) first for that.
    FillGapsOnly,
}

impl ConfigEntry {
    fn path(&self) -> &Utf8Path {
        match self {
            ConfigEntry::File { path, .. }
            | ConfigEntry::CreateOnly { path, .. }
            | ConfigEntry::Alias { path, .. }
            | ConfigEntry::Dir { path } => path,
            ConfigEntry::Symlink { link, .. } => link,
        }
    }

    /// Whether something is already at this entry's path/link, regardless of
    /// content — the check [`RefreshPolicy::FillGapsOnly`] stops at.
    fn present(&self) -> bool {
        match self {
            ConfigEntry::File { path, .. }
            | ConfigEntry::CreateOnly { path, .. }
            | ConfigEntry::Alias { path, .. } => path.exists(),
            ConfigEntry::Dir { path } => path.is_dir(),
            ConfigEntry::Symlink { link, .. } => std::fs::symlink_metadata(link).is_ok(),
        }
    }

    fn change(&self, policy: RefreshPolicy) -> Change {
        if policy == RefreshPolicy::FillGapsOnly {
            return if self.present() {
                Change::Unchanged
            } else {
                Change::Create
            };
        }
        match self {
            ConfigEntry::File { path, desired } => match std::fs::read_to_string(path) {
                Ok(existing) if &existing == desired => Change::Unchanged,
                Ok(_) => Change::Update,
                Err(_) => Change::Create,
            },
            ConfigEntry::CreateOnly { path, .. } => {
                if path.exists() {
                    Change::Unchanged
                } else {
                    Change::Create
                }
            }
            ConfigEntry::Alias {
                path,
                name,
                category,
                packages_line,
            } => match std::fs::read_to_string(path) {
                // Exact match, not a substring check: a hand-edited
                // `alias-packages` line that merely *contains* our computed
                // line as a prefix/substring (e.g. someone appended a
                // package by hand instead of using `--ex-pkg`) must still
                // count as drift, not "already up to date"
                // a `.contains()` check here let a hand-added trailing
                // package silently survive while an edit anywhere else in
                // the line would just as silently have been clobbered, an
                // inconsistency with no principled reason to keep.
                Ok(existing) if existing == alias_body(name, category, packages_line) => {
                    Change::Unchanged
                }
                // Foreign (no `alias-target =` key at all) — never touch.
                Ok(existing) if !existing.contains("alias-target =") => Change::Unchanged,
                Ok(_) => Change::Update,
                Err(_) => Change::Create,
            },
            ConfigEntry::Dir { path } => {
                if path.is_dir() {
                    Change::Unchanged
                } else {
                    Change::Create
                }
            }
            ConfigEntry::Symlink { link, target } => match std::fs::read_link(link) {
                Ok(dst) if dst == *target.as_std_path() => Change::Unchanged,
                Ok(_) => Change::Update,
                Err(_) => Change::Create,
            },
        }
    }

    fn apply(&self) -> Result<()> {
        if let Some(parent) = self.path().parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
        }
        match self {
            ConfigEntry::File { path, desired } => {
                std::fs::write(path, desired).with_context(|| format!("writing {path}"))
            }
            ConfigEntry::CreateOnly { path, desired } => write_if_absent(path, desired),
            ConfigEntry::Alias {
                path,
                name,
                category,
                packages_line,
            } => {
                // Re-check foreign-ness at apply time too (defence in depth;
                // `change()` already filtered foreign entries out of the plan).
                if let Ok(existing) = std::fs::read_to_string(path)
                    && !existing.contains("alias-target =")
                {
                    return Ok(());
                }
                std::fs::write(path, alias_body(name, category, packages_line))
                    .with_context(|| format!("writing {path}"))
            }
            ConfigEntry::Dir { path } => {
                std::fs::create_dir_all(path).with_context(|| format!("creating {path}"))
            }
            ConfigEntry::Symlink { link, target } => symlink_force(target, link),
        }
    }
}

/// Replace whatever is at `link` with a symlink to `dst` (absolute target, so it
/// resolves the same from a sysroot offset).
pub(crate) fn symlink_force(dst: &Utf8Path, link: &Utf8Path) -> Result<()> {
    match std::fs::symlink_metadata(link) {
        Ok(_) => std::fs::remove_file(link).with_context(|| format!("removing {link}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("stat {link}")),
    }
    std::os::unix::fs::symlink(dst, link).with_context(|| format!("linking {link} -> {dst}"))
}

/// Apply every entry unconditionally (`RefreshPolicy::Sync`, no
/// diff/preview/confirm) — for a caller that is already externally gated by
/// `!globals.pretend` (the native toolchain `--setup` path, which has its
/// own, separately-established pretend handling and doesn't need its own
/// preview here).
pub(crate) fn apply_now(entries: &[ConfigEntry]) -> Result<()> {
    for e in entries {
        if !matches!(e.change(RefreshPolicy::Sync), Change::Unchanged) {
            e.apply()?;
        }
    }
    Ok(())
}

/// What happened to a collected [`ConfigEntry`] plan.
pub(crate) enum Outcome {
    /// Nothing to do (or `-p`: shown but not written).
    NothingToApply,
    /// `-p`: previewed only.
    Previewed,
    /// `-a`: user declined.
    Declined,
    /// Written for real.
    Applied,
}

impl Outcome {
    /// Whether the caller should go on to print its own "ready" summary —
    /// true only when something was genuinely written (or there was nothing
    /// to do in the first place, i.e. already up to date).
    pub(crate) fn applied(&self) -> bool {
        matches!(self, Outcome::Applied | Outcome::NothingToApply)
    }
}

/// Diff `entries` against disk under `policy` and, per `pretend`/`ask`,
/// preview, confirm, or apply them. `pretend` is `Cli`'s own `global` field
/// (one shared value, works from any position); `ask` is the caller's
/// already-merged `MergeFlags::ask` (see `merge_merge_flags`) — not global,
/// so it has to be resolved by the caller instead of read straight off
/// `&Cli`.
pub(crate) fn apply(
    entries: &[ConfigEntry],
    pretend: bool,
    ask: bool,
    policy: RefreshPolicy,
) -> Result<Outcome> {
    let mut to_apply: Vec<&ConfigEntry> = Vec::new();
    let mut changed: Vec<(Utf8PathBuf, &'static str)> = Vec::new();
    for e in entries {
        let verb = match e.change(policy) {
            Change::Create => "create",
            Change::Update => "update",
            Change::Unchanged => continue,
        };
        changed.push((e.path().to_owned(), verb));
        to_apply.push(e);
    }

    if changed.is_empty() {
        return Ok(Outcome::NothingToApply);
    }

    if pretend || ask {
        println!(">>> config changes:");
        for (path, verb) in &changed {
            println!("  {verb} {path}");
        }
    }
    if pretend {
        // Fully pretend: no disk writes. Crossdev staged `-p` injects planned
        // aliases in-memory via `EmergeOpts::extra_aliases` / `load_repos`
        // (aliases are virtual — no on-disk tree required).
        return Ok(Outcome::Previewed);
    }
    if ask && !confirm_config_write(changed.len())? {
        println!(">>> Quitting.");
        return Ok(Outcome::Declined);
    }

    for e in to_apply {
        e.apply()?;
    }
    Ok(Outcome::Applied)
}

/// `pub(crate)`, not just `apply`'s own private helper: reused by the
/// `--ask`-triggered `package.use` write prompt in `query::depgraph`, which
/// wants the exact same "write these N config file(s)?" phrasing/TTY-gate
/// rather than a second near-identical prompt.
pub(crate) fn confirm_config_write(count: usize) -> Result<bool> {
    crate::merge::require_ask_tty()?;
    print!("\n>>> Would you like to write these {count} config file(s)? [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Ok(false);
    }
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf8_join(dir: &tempfile::TempDir, rel: &str) -> Result<Utf8PathBuf> {
        Ok(Utf8Path::from_path(dir.path())
            .context("non-UTF-8 tempdir")?
            .join(rel))
    }

    /// `-p`: nothing is written, even though there's a real change to make.
    #[test]
    fn pretend_writes_nothing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = utf8_join(&dir, "make.conf")?;
        let entries = vec![ConfigEntry::File {
            path: path.clone(),
            desired: "CHOST=riscv64-unknown-linux-gnu\n".to_owned(),
        }];
        let outcome = apply(&entries, true, false, RefreshPolicy::Sync)?;
        assert!(matches!(outcome, Outcome::Previewed));
        assert!(!path.exists(), "pretend must not write {path}");
        Ok(())
    }

    /// Neither `-p` nor `-a`: applies directly, no prompt needed.
    #[test]
    fn plain_run_applies_directly() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = utf8_join(&dir, "nested/make.conf")?;
        let desired = "CHOST=riscv64-unknown-linux-gnu\n".to_owned();
        let entries = vec![ConfigEntry::File {
            path: path.clone(),
            desired: desired.clone(),
        }];
        let outcome = apply(&entries, false, false, RefreshPolicy::Sync)?;
        assert!(matches!(outcome, Outcome::Applied));
        assert_eq!(std::fs::read_to_string(&path)?, desired);
        Ok(())
    }

    /// Nothing changed (content already matches): reported as
    /// `NothingToApply`, and `Outcome::applied()` treats that as "the
    /// caller's own 'ready' summary may print" (it's already in the desired
    /// state either way).
    #[test]
    fn no_change_is_reported_as_nothing_to_apply() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = utf8_join(&dir, "make.conf")?;
        let desired = "CHOST=riscv64-unknown-linux-gnu\n".to_owned();
        std::fs::write(&path, &desired)?;
        let entries = vec![ConfigEntry::File { path, desired }];
        let outcome = apply(&entries, false, false, RefreshPolicy::Sync)?;
        assert!(matches!(outcome, Outcome::NothingToApply));
        assert!(outcome.applied());
        Ok(())
    }

    /// `CreateOnly` never overwrites an existing file's content, no matter
    /// how it differs from `desired`.
    #[test]
    fn create_only_never_overwrites_existing_content() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = utf8_join(&dir, "gentoo.conf")?;
        std::fs::write(&path, "[gentoo]\nlocation = /somewhere/else\n")?;
        let entries = vec![ConfigEntry::CreateOnly {
            path: path.clone(),
            desired: "[gentoo]\nlocation = /var/db/repos/gentoo\n".to_owned(),
        }];
        apply(&entries, false, false, RefreshPolicy::Sync)?;
        assert_eq!(
            std::fs::read_to_string(&path)?,
            "[gentoo]\nlocation = /somewhere/else\n"
        );
        Ok(())
    }

    /// `Dir` creates a missing directory and is a no-op once it exists.
    #[test]
    fn dir_entry_creates_a_missing_directory() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = utf8_join(&dir, "var/db/pkg")?;
        assert!(!path.is_dir());
        apply(
            &[ConfigEntry::Dir { path: path.clone() }],
            false,
            false,
            RefreshPolicy::Sync,
        )?;
        assert!(path.is_dir());
        Ok(())
    }

    /// A foreign `Alias` entry (no `alias-target =` key) is reported
    /// unchanged and never overwritten.
    #[test]
    fn alias_entry_never_touches_a_foreign_file() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = utf8_join(&dir, "crossdev.conf")?;
        let foreign = "[crossdev]\nlocation = /var/db/repos/crossdev\n".to_owned();
        std::fs::write(&path, &foreign)?;
        let entries = vec![ConfigEntry::Alias {
            path: path.clone(),
            name: "crossdev.riscv64-unknown-linux-gnu".to_owned(),
            category: "cross-riscv64-unknown-linux-gnu".to_owned(),
            packages_line: "sys-devel/binutils".to_owned(),
        }];
        let outcome = apply(&entries, false, false, RefreshPolicy::Sync)?;
        assert!(matches!(outcome, Outcome::NothingToApply));
        assert_eq!(std::fs::read_to_string(&path)?, foreign);
        Ok(())
    }

    /// A hand-edited `alias-packages` line that happens to *contain* the
    /// computed line as a substring (e.g. someone appended a package by hand
    /// instead of using `--ex-pkg`) must still count as drift and be
    /// refreshed — a loose `.contains()` check previously let this slip
    /// through as "already up to date", silently discarding the hand edit
    /// on any later re-run instead of visibly overwriting it (or, depending
    /// on where in the line the edit landed, inconsistently doing the
    /// opposite). Exact-body comparison fixes both.
    #[test]
    fn alias_entry_treats_a_hand_extended_line_as_drift() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = utf8_join(&dir, "crossdev.conf")?;
        std::fs::write(
            &path,
            "[crossdev]\nalias-source = gentoo\nalias-target = cross-riscv64-unknown-linux-gnu\n\
             alias-packages = sys-devel/binutils dev-vcs/git\n",
        )?;
        let entries = vec![ConfigEntry::Alias {
            path: path.clone(),
            name: "crossdev.riscv64-unknown-linux-gnu".to_owned(),
            category: "cross-riscv64-unknown-linux-gnu".to_owned(),
            packages_line: "sys-devel/binutils".to_owned(),
        }];
        let outcome = apply(&entries, false, false, RefreshPolicy::Sync)?;
        assert!(matches!(outcome, Outcome::Applied));
        assert!(!std::fs::read_to_string(&path)?.contains("dev-vcs/git"));
        Ok(())
    }

    /// `FillGapsOnly` (`--setup`'s own implied config-laydown step): an
    /// existing file is left completely alone, no matter how far its
    /// content has drifted from `desired` — a hand edit made between an
    /// earlier `--init-target` and this `--setup` must survive.
    #[test]
    fn fill_gaps_only_never_touches_an_existing_file() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = utf8_join(&dir, "make.conf")?;
        std::fs::write(&path, "CHOST=hand-edited\n")?;
        let entries = vec![ConfigEntry::File {
            path: path.clone(),
            desired: "CHOST=riscv64-unknown-linux-gnu\n".to_owned(),
        }];
        let outcome = apply(&entries, false, false, RefreshPolicy::FillGapsOnly)?;
        assert!(matches!(outcome, Outcome::NothingToApply));
        assert_eq!(std::fs::read_to_string(&path)?, "CHOST=hand-edited\n");
        Ok(())
    }

    /// `FillGapsOnly` still creates a file that's genuinely missing — a
    /// fresh target being `--setup` directly (no prior `--init-target`)
    /// still gets fully written.
    #[test]
    fn fill_gaps_only_still_creates_missing_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = utf8_join(&dir, "make.conf")?;
        let desired = "CHOST=riscv64-unknown-linux-gnu\n".to_owned();
        let entries = vec![ConfigEntry::File {
            path: path.clone(),
            desired: desired.clone(),
        }];
        let outcome = apply(&entries, false, false, RefreshPolicy::FillGapsOnly)?;
        assert!(matches!(outcome, Outcome::Applied));
        assert_eq!(std::fs::read_to_string(&path)?, desired);
        Ok(())
    }

    /// `FillGapsOnly` also leaves an existing `Alias` entry alone even when
    /// its `packages_line` no longer matches what this run would compute
    /// (e.g. a different `--ex-pkg` selection than an earlier explicit
    /// `--init-target` used) — the accepted trade-off for hand edits
    /// surviving `--setup`.
    #[test]
    fn fill_gaps_only_never_touches_an_existing_alias_even_with_a_different_packages_line()
    -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = utf8_join(&dir, "crossdev.conf")?;
        let existing = alias_body(
            "crossdev.riscv64-unknown-linux-gnu",
            "cross-riscv64-unknown-linux-gnu",
            "sys-devel/binutils",
        );
        std::fs::write(&path, &existing)?;
        let entries = vec![ConfigEntry::Alias {
            path: path.clone(),
            name: "crossdev.riscv64-unknown-linux-gnu".to_owned(),
            category: "cross-riscv64-unknown-linux-gnu".to_owned(),
            packages_line: "sys-devel/binutils dev-vcs/git".to_owned(),
        }];
        let outcome = apply(&entries, false, false, RefreshPolicy::FillGapsOnly)?;
        assert!(matches!(outcome, Outcome::NothingToApply));
        assert_eq!(std::fs::read_to_string(&path)?, existing);
        Ok(())
    }
}
