//! `em news` — GLEP 42 news items.
//!
//! Mirrors real portage's `portage/news.py` (`NewsManager`/`NewsItem`) and
//! `eselect news`'s state-file dance rather than reinventing the format: news
//! items live at `<repo>/metadata/news/<item>/<item>.en.txt`, tracked per repo
//! under `${EROOT}/var/lib/gentoo/news/news-<repo>.{unread,read,skip}` — one
//! item name per line. `skip` records every item ever promoted into `unread`
//! so that a since-read-and-purged item is never reconsidered, even after a
//! rescan.
//!
//! An item's relevance is AND'd across whichever `Display-If-*` restriction
//! kinds it declares (a kind that's absent doesn't constrain), OR'd within a
//! kind — matching `NewsItem.isRelevant`'s `all_match`/`any_match`.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::{Cpv, Dep};
use portage_repo::ReposConf;

use crate::advisory::{
    self, effective_arch, installed_packages, print_item_banner, print_list_header, print_sub_line,
    print_summary, read_line_set, state_dir, status_marker, truncate_to_budget, write_line_set,
};
use crate::cli::{Cli, NewsCommand};
use crate::style::{einfo_line, warn_line};

const LANGUAGE_ID: &str = "en";

struct RelevanceCtx {
    profile: Option<String>,
    arch: String,
}

impl RelevanceCtx {
    fn load(globals: &Cli) -> Self {
        let profile = main_repo_and_profile(globals).and_then(|(_, p)| p);
        let arch = effective_arch(globals);
        Self { profile, arch }
    }
}

/// The main repo (for `current_profile`'s `profiles/`-relative resolution)
/// plus that profile, if `make.profile` resolves.
fn main_repo_and_profile(globals: &Cli) -> Option<(portage_repo::Repository, Option<String>)> {
    let conf = ReposConf::load().ok()?;
    let entry = conf.main_repo().or_else(|| conf.find("gentoo"))?;
    let repo = crate::repo_open::open(entry.location.as_path()?).ok()?;
    let profile = crate::select::profile::current_profile(globals, &repo);
    Some((repo, profile))
}

#[derive(Default)]
struct Restrictions {
    installed: Vec<Dep>,
    profile: Vec<String>,
    keyword: Vec<String>,
}

impl Restrictions {
    fn is_empty(&self) -> bool {
        self.installed.is_empty() && self.profile.is_empty() && self.keyword.is_empty()
    }
}

struct ParsedItem {
    valid: bool,
    restrictions: Restrictions,
    title: Option<String>,
    author: Option<String>,
    posted: Option<String>,
}

/// Parse a news item's header. Only the fields needed for relevance and
/// listing are extracted; the body is re-read verbatim when displaying.
fn parse_item(text: &str) -> ParsedItem {
    let mut format_ok = false;
    let mut restrictions = Restrictions::default();
    let mut title = None;
    let mut author = None;
    let mut posted = None;

    for line in text.lines() {
        // The header ends at the first blank line.
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("News-Item-Format:") {
            let v = v.trim();
            format_ok = v.starts_with("1.") || v.starts_with("2.");
        } else if let Some(v) = line.strip_prefix("Display-If-Installed:") {
            match Dep::parse(v.trim()) {
                Ok(d) => restrictions.installed.push(d),
                Err(_) => return invalid(),
            }
        } else if let Some(v) = line.strip_prefix("Display-If-Profile:") {
            restrictions.profile.push(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Display-If-Keyword:") {
            restrictions.keyword.push(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Title:") {
            title = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Author:") {
            author = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Posted:") {
            posted = Some(v.trim().to_string());
        }
    }

    ParsedItem {
        valid: format_ok,
        restrictions,
        title,
        author,
        posted,
    }
}

fn invalid() -> ParsedItem {
    ParsedItem {
        valid: false,
        restrictions: Restrictions::default(),
        title: None,
        author: None,
        posted: None,
    }
}

fn is_relevant(r: &Restrictions, ctx: &RelevanceCtx, installed: &[(Cpv, String)]) -> bool {
    if r.is_empty() {
        return true;
    }
    if !r.installed.is_empty() {
        let any = r.installed.iter().any(|d| {
            installed
                .iter()
                .any(|(cpv, slot)| d.matches_cpv(cpv, Some(slot)))
        });
        if !any {
            return false;
        }
    }
    if !r.profile.is_empty() {
        let any = r.profile.iter().any(|p| match p.strip_suffix("/*") {
            Some(prefix) => ctx
                .profile
                .as_deref()
                .is_some_and(|cur| cur.starts_with(prefix)),
            None => ctx.profile.as_deref() == Some(p.as_str()),
        });
        if !any {
            return false;
        }
    }
    if !r.keyword.is_empty() {
        let any = r.keyword.iter().any(|k| k == &ctx.arch);
        if !any {
            return false;
        }
    }
    true
}

/// Where unread/read/skip state lives for `eroot` — see
/// [`advisory::state_dir`] for the bare-host/managed-root split.
fn news_lib_dir(eroot: &Utf8Path) -> Utf8PathBuf {
    state_dir(eroot, "var/lib/gentoo/news", crate::xdg::news_state_dir)
}

fn state_path(eroot: &Utf8Path, repo: &str, kind: &str) -> Utf8PathBuf {
    news_lib_dir(eroot).join(format!("news-{repo}.{kind}"))
}

/// A repo's news directory, if it has one on disk.
fn repo_news_dir(repo_path: &Utf8Path) -> Utf8PathBuf {
    repo_path.join("metadata/news")
}

/// Rescan a repo's news dir and promote any newly-relevant item into
/// `unread` (and `skip`, so it's never reconsidered again).
fn update_repo(
    eroot: &Utf8Path,
    repo: &str,
    repo_path: &Utf8Path,
    ctx: &RelevanceCtx,
    installed: &[(Cpv, String)],
) -> Result<()> {
    let news_dir = repo_news_dir(repo_path);
    let Ok(entries) = std::fs::read_dir(news_dir.as_std_path()) else {
        return Ok(());
    };

    let unread_path = state_path(eroot, repo, "unread");
    let skip_path = state_path(eroot, repo, "skip");
    let mut unread = read_line_set(&unread_path);
    let unread_orig = unread.clone();
    let mut skip = read_line_set(&skip_path);
    let skip_orig = skip.clone();

    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if skip.contains(&name) {
            continue;
        }
        let item_file = news_dir
            .join(&name)
            .join(format!("{name}.{LANGUAGE_ID}.txt"));
        let Ok(text) = std::fs::read_to_string(item_file.as_std_path()) else {
            continue;
        };
        let parsed = parse_item(&text);
        if !parsed.valid {
            warn_line!("invalid news item: {name}");
            continue;
        }
        if is_relevant(&parsed.restrictions, ctx, installed) {
            unread.insert(name.clone());
            skip.insert(name);
        }
    }

    std::fs::create_dir_all(news_lib_dir(eroot).as_std_path())
        .with_context(|| format!("creating {}", news_lib_dir(eroot)))?;
    if unread != unread_orig {
        write_line_set(&unread_path, &unread)?;
    }
    if skip != skip_orig {
        write_line_set(&skip_path, &skip)?;
    }
    Ok(())
}

struct Item {
    repo: String,
    name: String,
    unread: bool,
    title: Option<String>,
    posted: Option<String>,
}

/// Every repo's news items, `unread` first status per item, sorted by item
/// name (which is date-prefixed, so this is also chronological) — the same
/// sort `eselect news`'s `find_items` uses.
fn collect_items(globals: &Cli, eroot: &Utf8Path, update: bool) -> Result<Vec<Item>> {
    let repos_conf = globals
        .outer_roots()
        .repos_conf()
        .context("reading repos.conf")?;
    let ctx = RelevanceCtx::load(globals);
    let installed = installed_packages(globals);

    let mut items = Vec::new();
    for entry in repos_conf.repos() {
        let Some(path) = entry.location.as_path() else {
            continue;
        };
        let repo_path = Utf8Path::from_path(path)
            .map(Utf8Path::to_path_buf)
            .unwrap_or_default();
        if update {
            update_repo(eroot, &entry.name, &repo_path, &ctx, &installed)?;
        }
        let unread = read_line_set(&state_path(eroot, &entry.name, "unread"));
        let read = read_line_set(&state_path(eroot, &entry.name, "read"));
        for name in &unread {
            items.push(load_item(&entry.name, name, &repo_path, true));
        }
        for name in &read {
            items.push(load_item(&entry.name, name, &repo_path, false));
        }
    }
    items.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(items)
}

fn load_item(repo: &str, name: &str, repo_path: &Utf8Path, unread: bool) -> Item {
    let item_file = repo_news_dir(repo_path)
        .join(name)
        .join(format!("{name}.{LANGUAGE_ID}.txt"));
    let (title, posted) = match std::fs::read_to_string(item_file.as_std_path()) {
        Ok(text) => {
            let parsed = parse_item(&text);
            (parsed.title, parsed.posted)
        }
        Err(_) => (None, None),
    };
    Item {
        repo: repo.to_string(),
        name: name.to_string(),
        unread,
        title,
        posted,
    }
}

fn run_count(globals: &Cli) -> Result<()> {
    let eroot = globals.outer_roots().merge_root().to_owned();
    let items = collect_items(globals, &eroot, true)?;
    println!("{}", items.iter().filter(|i| i.unread).count());
    Ok(())
}

fn run_list(globals: &Cli) -> Result<()> {
    let eroot = globals.outer_roots().merge_root().to_owned();
    let items = collect_items(globals, &eroot, true)?;
    if items.is_empty() {
        println!("(no news items)");
        return Ok(());
    }

    print_list_header("News items:");

    let num_width = advisory::num_width(items.len());
    // Room for "<num>  <mark>  " before the line itself, mirroring eselect's
    // own `11 + ${#line} >= cols` budget.
    let budget = crate::style::term_width().saturating_sub(num_width + 5);
    for (i, item) in items.iter().enumerate() {
        let title = item.title.as_deref().unwrap_or("(no title)");
        let posted = item.posted.as_deref().unwrap_or("(no date)");
        let repo_tag = if item.repo == "gentoo" {
            String::new()
        } else {
            format!("[{}] ", item.repo)
        };
        let line = truncate_to_budget(format!("{posted:<12} {repo_tag}{title}"), budget);
        let mark = status_marker(item.unread, 'N');
        println!("{:>num_width$}  {mark}  {line}", i + 1);
    }

    let unread = items.iter().filter(|i| i.unread).count();
    print_summary(unread, "unread", items.len(), "news item", "news items");
    Ok(())
}

fn find_item_index(items: &[Item], id: &str) -> Option<usize> {
    if let Ok(n) = id.parse::<usize>()
        && n >= 1
        && n <= items.len()
    {
        return Some(n - 1);
    }
    items.iter().position(|i| i.name == id)
}

fn run_read(globals: &Cli, ids: &[String]) -> Result<()> {
    let eroot = globals.outer_roots().merge_root().to_owned();
    let items = collect_items(globals, &eroot, true)?;

    // No ids, or the single keyword "new"/"all" — real eselect's own
    // shorthand: "new" (also the no-args default) means every unread item,
    // "all" means every item regardless of read status.
    let keyword = match ids {
        [] => Some("new"),
        [only] if only == "new" || only == "all" => Some(only.as_str()),
        _ => None,
    };
    let targets: Vec<usize> = match keyword {
        Some("all") => (0..items.len()).collect(),
        Some(_) => items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.unread)
            .map(|(idx, _)| idx)
            .collect(),
        None => {
            let mut targets = Vec::with_capacity(ids.len());
            for id in ids {
                match find_item_index(&items, id) {
                    Some(idx) => targets.push(idx),
                    None => warn_line!("news item not found: {id}"),
                }
            }
            targets
        }
    };

    if targets.is_empty() {
        println!("No news is good news.");
        return Ok(());
    }

    let mut newly_read: Vec<(&str, &str)> = Vec::new();
    for (n, &idx) in targets.iter().enumerate() {
        let item = &items[idx];
        let item_file = repo_news_dir(&repo_path_for(globals, &item.repo)?)
            .join(&item.name)
            .join(format!("{}.{LANGUAGE_ID}.txt", item.name));
        match std::fs::read_to_string(item_file.as_std_path()) {
            Ok(text) => {
                if n > 0 {
                    println!();
                }
                let parsed = parse_item(&text);
                let title = parsed.title.as_deref().unwrap_or(&item.name);
                print_item_banner(title, &item.name);
                if let Some(author) = &parsed.author {
                    print_sub_line("Author", author);
                }
                if let Some(posted) = &parsed.posted {
                    print_sub_line("Posted", posted);
                }
                println!();
                let body_start = text.find("\n\n").map(|p| p + 2).unwrap_or(0);
                print!("{}", &text[body_start..]);
                println!();
            }
            Err(_) => warn_line!("news item {} no longer exists", item.name),
        }
        if item.unread {
            newly_read.push((&item.repo, &item.name));
        }
    }

    for (repo, name) in newly_read {
        let mut unread = read_line_set(&state_path(&eroot, repo, "unread"));
        let mut read = read_line_set(&state_path(&eroot, repo, "read"));
        unread.remove(name);
        read.insert(name.to_string());
        write_line_set(&state_path(&eroot, repo, "unread"), &unread)?;
        write_line_set(&state_path(&eroot, repo, "read"), &read)?;
    }
    Ok(())
}

fn repo_path_for(globals: &Cli, repo: &str) -> Result<Utf8PathBuf> {
    let conf = globals
        .outer_roots()
        .repos_conf()
        .context("reading repos.conf")?;
    let entry = conf.find(repo).context("repo no longer configured")?;
    let path = entry
        .location
        .as_path()
        .context("virtual repo has no path")?;
    Ok(Utf8Path::from_path(path)
        .map(Utf8Path::to_path_buf)
        .unwrap_or_default())
}

fn run_purge(globals: &Cli) -> Result<()> {
    let eroot = globals.outer_roots().merge_root().to_owned();
    let conf = globals
        .outer_roots()
        .repos_conf()
        .context("reading repos.conf")?;
    let mut purged = 0usize;
    for entry in conf.repos() {
        let path = state_path(&eroot, &entry.name, "read");
        let existing = read_line_set(&path);
        if !existing.is_empty() {
            purged += existing.len();
            write_line_set(&path, &BTreeSet::new())?;
        }
    }
    einfo_line!("purged {purged} read news item(s)");
    Ok(())
}

pub(crate) fn run(command: &Option<NewsCommand>, globals: &Cli) -> Result<()> {
    match command {
        None | Some(NewsCommand::Count) => run_count(globals),
        Some(NewsCommand::List) => run_list(globals),
        Some(NewsCommand::Read { ids }) => run_read(globals, ids),
        Some(NewsCommand::Purge) => run_purge(globals),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn news_lib_dir_uses_xdg_state_for_the_bare_host() {
        let dir = news_lib_dir(Utf8Path::new("/"));
        assert!(
            dir.ends_with("em/news"),
            "expected an XDG state path, got {dir}"
        );
        assert_ne!(dir.as_str(), "/var/lib/gentoo/news");
    }

    #[test]
    fn news_lib_dir_uses_the_real_path_under_a_managed_root() {
        let dir = news_lib_dir(Utf8Path::new("/tmp/some-root"));
        assert_eq!(dir.as_str(), "/tmp/some-root/var/lib/gentoo/news");
    }

    /// Header block only (no trailing blank line / body) — restriction lines
    /// can be appended directly, since `parse_item` stops scanning at the
    /// first blank line.
    const HEADER: &str = "\
Title: Change of ACCEPT_LICENSE default
Author: Thomas Deutschmann <whissi@gentoo.org>
Posted: 2019-05-23
Revision: 1
News-Item-Format: 2.0
";

    const ITEM: &str = "\
Title: Change of ACCEPT_LICENSE default
Author: Thomas Deutschmann <whissi@gentoo.org>
Posted: 2019-05-23
Revision: 1
News-Item-Format: 2.0

The default set of accepted licenses has been changed.
";

    fn ctx(profile: Option<&str>, arch: &str) -> RelevanceCtx {
        RelevanceCtx {
            profile: profile.map(str::to_string),
            arch: arch.to_string(),
        }
    }

    #[test]
    fn parses_header_fields() {
        let parsed = parse_item(ITEM);
        assert!(parsed.valid);
        assert_eq!(
            parsed.title.as_deref(),
            Some("Change of ACCEPT_LICENSE default")
        );
        assert_eq!(
            parsed.author.as_deref(),
            Some("Thomas Deutschmann <whissi@gentoo.org>")
        );
        assert_eq!(parsed.posted.as_deref(), Some("2019-05-23"));
        assert!(parsed.restrictions.is_empty());
    }

    #[test]
    fn rejects_missing_format() {
        let text = "Title: no format line\n\nbody\n";
        assert!(!parse_item(text).valid);
    }

    #[test]
    fn unrestricted_item_is_always_relevant() {
        let parsed = parse_item(ITEM);
        assert!(is_relevant(&parsed.restrictions, &ctx(None, "amd64"), &[]));
    }

    #[test]
    fn keyword_restriction_matches_current_arch_only() {
        let text = format!("{HEADER}Display-If-Keyword: amd64\n");
        let parsed = parse_item(&text);
        assert!(is_relevant(&parsed.restrictions, &ctx(None, "amd64"), &[]));
        assert!(!is_relevant(&parsed.restrictions, &ctx(None, "x86"), &[]));
    }

    #[test]
    fn profile_wildcard_matches_by_prefix() {
        let text = format!("{HEADER}Display-If-Profile: default/linux/amd64/*\n");
        let parsed = parse_item(&text);
        assert!(is_relevant(
            &parsed.restrictions,
            &ctx(Some("default/linux/amd64/23.0"), "amd64"),
            &[]
        ));
        assert!(!is_relevant(
            &parsed.restrictions,
            &ctx(Some("default/linux/x86/23.0"), "amd64"),
            &[]
        ));
    }

    #[test]
    fn installed_restriction_checks_the_precomputed_set() {
        let text = format!("{HEADER}Display-If-Installed: dev-lang/rust\n");
        let parsed = parse_item(&text);
        let rust = (Cpv::parse("dev-lang/rust-1.75.0").unwrap(), "0".to_string());
        assert!(is_relevant(
            &parsed.restrictions,
            &ctx(None, "amd64"),
            &[rust]
        ));
        assert!(!is_relevant(&parsed.restrictions, &ctx(None, "amd64"), &[]));
    }

    #[test]
    fn restriction_kinds_and_together_but_or_within_a_kind() {
        // Two kinds present: needs a matching keyword AND a matching profile.
        let text = format!(
            "{HEADER}Display-If-Keyword: amd64\nDisplay-If-Keyword: x86\nDisplay-If-Profile: default/linux/amd64/23.0\n"
        );
        let parsed = parse_item(&text);
        assert!(is_relevant(
            &parsed.restrictions,
            &ctx(Some("default/linux/amd64/23.0"), "amd64"),
            &[]
        ));
        // Keyword matches (OR'd) but profile doesn't (AND'd across kinds).
        assert!(!is_relevant(
            &parsed.restrictions,
            &ctx(Some("default/linux/x86/23.0"), "amd64"),
            &[]
        ));
    }
}
