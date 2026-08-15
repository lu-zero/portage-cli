//! Shared helpers and formatters for `em select news` and `em select glsa`.
//!
//! Both are small "here's a list of named items, some need your attention"
//! tools — unread news items, unresolved security advisories — reading a
//! repo-shipped catalog and tracking per-item state under `${EROOT}` (falling
//! back to XDG state on the bare host; see [`state_dir`]). They should look
//! and feel like the same tool family, not two independently-invented UIs:
//! a numbered list with a single-char status marker and a terminal-width-
//! aware truncation, a trailing "N `<verb>` of M `<noun>`" summary, and a
//! per-item detail banner (bold title, dim parenthetical tag, dim
//! `Label: value` sub-lines) for anything worth showing in full.

use std::collections::BTreeSet;

use anstyle::{Effects, Style};
use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Cpv;

use crate::cli::Cli;
use crate::style::{C_BOLD, C_MARKER_INFO, C_WARN};

/// Secondary/annotation text (a dim `(tag)`, a dim `Label:` prefix) — matches
/// `info.rs`'s local `C_DIM` convention; not (yet) promoted to `style.rs`'s
/// shared palette.
pub(crate) const C_DIM: Style = Style::new().effects(Effects::DIMMED);

/// The architecture filter both `Display-If-Keyword` (news) and a GLSA
/// `<package arch="...">` match against: `ARCH` from the effective
/// `make.conf`, else the global `--arch` — the same resolution
/// `select/profile.rs`'s own `effective_arch` uses for `eselect profile`'s
/// arch filter.
pub(crate) fn effective_arch(globals: &Cli) -> String {
    let make_conf = crate::select::config_portage_dir(globals).join("make.conf");
    if let Ok(conf) = portage_repo::MakeConf::load(&make_conf)
        && let Some(arch) = conf.get("ARCH").filter(|a| !a.is_empty())
    {
        return arch.to_string();
    }
    globals.arch.as_str().to_string()
}

/// Every installed package as `(cpv, main_slot)`, or empty if the VDB can't
/// be opened — news' `Display-If-Installed` and GLSA's affected-version
/// matching both just need "what's on the system", not full VDB metadata.
pub(crate) fn installed_packages(globals: &Cli) -> Vec<(Cpv, String)> {
    crate::vdb::open_cli_vdb(globals)
        .map(|vdb| {
            vdb.packages()
                .into_iter()
                .map(|p| (p.cpv().clone(), p.slot_main().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default()
}

/// Where a command's own state lives for `eroot`: the real, root-owned
/// system path (`eroot.join(real_subpath)`) under a managed
/// `--root`/`--prefix`/`--local` target (the invoking user's own tree, same
/// as `var/lib/portage/world` under it) — but `xdg()`'s unprivileged XDG
/// path on the bare host (`eroot == "/"`), since both `em news` and `em
/// glsa` are read-mostly/unprivileged commands that shouldn't need root
/// just to remember what they've already shown or fixed. Same reasoning as
/// `xdg::regen_activity_root`.
pub(crate) fn state_dir(
    eroot: &Utf8Path,
    real_subpath: &str,
    xdg: impl FnOnce() -> Utf8PathBuf,
) -> Utf8PathBuf {
    if eroot.as_str() == "/" {
        xdg()
    } else {
        eroot.join(real_subpath)
    }
}

/// Read a flat, one-item-per-line state file (missing/unreadable → empty).
pub(crate) fn read_line_set(path: &Utf8Path) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Write `items` back out, one per line, atomically.
pub(crate) fn write_line_set(path: &Utf8Path, items: &BTreeSet<String>) -> Result<()> {
    let body: String = items.iter().map(|i| format!("{i}\n")).collect();
    crate::util::write_atomic(path, body.as_bytes()).with_context(|| format!("writing {path}"))
}

/// A list row's single-char status column: `letter` in [`C_WARN`] when
/// `active` (news' unread `N`, GLSA's affected `A`), a blank column
/// otherwise — never omitted, so every row's title starts in the same
/// place regardless of status.
pub(crate) fn status_marker(active: bool, letter: char) -> String {
    if active {
        format!("{C_WARN}{letter}{C_WARN:#}")
    } else {
        " ".to_string()
    }
}

/// A list's header banner ("News items:", "GLSAs:").
pub(crate) fn print_list_header(title: &str) {
    println!("{C_BOLD}{title}{C_BOLD:#}\n");
}

/// The right-aligned row-number column width for `count` rows.
pub(crate) fn num_width(count: usize) -> usize {
    count.to_string().len()
}

/// Truncate `line` to `budget` display columns, appending `...` — the same
/// budget shape real `eselect`'s own list truncation uses
/// (`11 + ${#line} >= cols`), just computed from the caller's actual prefix
/// width instead of eselect's hardcoded `11`.
pub(crate) fn truncate_to_budget(line: String, budget: usize) -> String {
    if budget > 3 && line.chars().count() > budget {
        format!("{}...", line.chars().take(budget - 3).collect::<String>())
    } else {
        line
    }
}

/// A list's trailing "N `<verb>` of M `<noun>`(s)" summary line, e.g. "3 unread
/// of 10 news items" / "2 affected of 3817 GLSAs".
pub(crate) fn print_summary(active: usize, verb: &str, total: usize, singular: &str, plural: &str) {
    println!(
        "\n{active} {verb} of {total} {}",
        if total == 1 { singular } else { plural }
    );
}

/// A per-item detail banner: [`C_MARKER_INFO`]'s `*` marker, the title in
/// [`C_BOLD`], and a dim `(tag)` — news' item slug, a GLSA id. Shared by
/// `news read` and `glsa check`'s affected-item report so a single named
/// thing looks the same everywhere it's shown in full.
pub(crate) fn print_item_banner(title: &str, tag: &str) {
    println!("{C_MARKER_INFO}*{C_MARKER_INFO:#} {C_BOLD}{title}{C_BOLD:#} {C_DIM}({tag}){C_DIM:#}");
}

/// A dim `Label: value` sub-line under an item banner (`Author:`/`Posted:`/
/// `Synopsis:`).
pub(crate) fn print_sub_line(label: &str, value: &str) {
    println!("  {C_DIM}{label}:{C_DIM:#} {value}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_uses_xdg_for_the_bare_host() {
        let dir = state_dir(Utf8Path::new("/"), "var/lib/gentoo/news", || {
            Utf8PathBuf::from("/xdg/news")
        });
        assert_eq!(dir.as_str(), "/xdg/news");
    }

    #[test]
    fn state_dir_uses_the_real_path_under_a_managed_root() {
        let dir = state_dir(
            Utf8Path::new("/tmp/some-root"),
            "var/lib/gentoo/news",
            || Utf8PathBuf::from("/xdg/news"),
        );
        assert_eq!(dir.as_str(), "/tmp/some-root/var/lib/gentoo/news");
    }

    #[test]
    fn line_set_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap().join("state");
        let mut items = BTreeSet::new();
        items.insert("b".to_string());
        items.insert("a".to_string());
        write_line_set(&path, &items).unwrap();
        assert_eq!(read_line_set(&path), items);
    }

    #[test]
    fn missing_line_set_is_empty() {
        assert!(read_line_set(Utf8Path::new("/no/such/file")).is_empty());
    }

    #[test]
    fn truncate_leaves_short_lines_alone() {
        assert_eq!(truncate_to_budget("short".to_string(), 80), "short");
    }

    #[test]
    fn truncate_cuts_long_lines_with_ellipsis() {
        let long = "x".repeat(50);
        let out = truncate_to_budget(long, 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with("..."));
    }
}
