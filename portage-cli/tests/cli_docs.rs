//! Generate `docs/user/cli/` from `Cli::to_kdl()` in-process.
//!
//! Hidden applets (`__helper`, `__worker`) must not appear. Native `hide` is
//! the first cut; names are stripped too if a renderer still lists them.
//!
//! Refresh committed pages with:
//! `UPDATE_CLI_DOCS=1 cargo test -p portage-cli --test cli_docs`

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use portage_cli::cli::Cli;
use usage::spec::CommandMeta;
use usage_lib::docs::markdown::MarkdownRenderer;
use usage_lib::{Spec, SpecCommand};

const GENERATED_HEADER: &str = "<!-- @generated from em's usage spec; do not edit -->\n";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("portage-cli is a workspace member")
        .to_path_buf()
}

fn cli_docs_dir() -> PathBuf {
    workspace_root().join("docs/user/cli")
}

fn is_hidden_applet(cmd: &SpecCommand) -> bool {
    cmd.hide || cmd.name == "__helper" || cmd.name == "__worker"
}

fn omit_hidden(spec: &mut Spec) {
    fn walk(cmd: &mut SpecCommand) {
        cmd.subcommands.retain(|_, child| !is_hidden_applet(child));
        for child in cmd.subcommands.values_mut() {
            walk(child);
        }
    }
    walk(&mut spec.cmd);
}

fn parse_spec() -> Spec {
    let kdl = Cli::to_kdl();
    kdl.parse()
        .unwrap_or_else(|err| panic!("Cli::to_kdl() must parse as a usage spec:\n{err}\n{kdl}"))
}

/// Site-root `/foo.md` → a path relative to `from_page`.
fn relative_href(from_page: &str, target: &str) -> String {
    let (path, frag) = target
        .split_once('#')
        .map(|(p, f)| (p, Some(f)))
        .unwrap_or((target, None));
    let from_dir: Vec<&str> = match from_page.rsplit_once('/') {
        Some((dir, _)) => dir.split('/').filter(|s| !s.is_empty()).collect(),
        None => Vec::new(),
    };
    let to_parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut i = 0;
    while i < from_dir.len() && i < to_parts.len() && from_dir[i] == to_parts[i] {
        i += 1;
    }
    let mut rel = vec![".."; from_dir.len() - i];
    rel.extend(to_parts[i..].iter().copied());
    let href = if rel.is_empty() {
        from_page
            .rsplit_once('/')
            .map(|(_, name)| name)
            .unwrap_or(from_page)
            .to_string()
    } else {
        rel.join("/")
    };
    match frag {
        Some(f) => format!("{href}#{f}"),
        None => href,
    }
}

fn relativize_links(from_page: &str, md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut rest = md;
    while let Some(idx) = rest.find("](/") {
        out.push_str(&rest[..idx]);
        out.push_str("](");
        rest = &rest[idx + 3..];
        let end = rest
            .find(')')
            .unwrap_or_else(|| panic!("unclosed markdown link in {from_page}"));
        out.push_str(&relative_href(from_page, &rest[..end]));
        out.push(')');
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn finish_page(page: &str, md: &str) -> String {
    format!("{GENERATED_HEADER}{}\n", relativize_links(page, md.trim()))
}

/// One page per visible command, matching `usage generate markdown --multi`.
fn generated_pages() -> BTreeMap<String, String> {
    let mut spec = parse_spec();
    omit_hidden(&mut spec);
    let renderer = MarkdownRenderer::new(spec.clone())
        .with_multi(true)
        .with_html_encode(false);

    let mut pages = BTreeMap::new();
    for cmd in spec.cmd.all_subcommands() {
        if is_hidden_applet(cmd) {
            continue;
        }
        let md = renderer
            .render_cmd(cmd)
            .unwrap_or_else(|err| panic!("render `{}`: {err}", cmd.name));
        let parent = cmd.full_cmd[..cmd.full_cmd.len().saturating_sub(1)].join("/");
        let rel = if parent.is_empty() {
            format!("{}.md", cmd.name)
        } else {
            format!("{parent}/{}.md", cmd.name)
        };
        pages.insert(rel.clone(), finish_page(&rel, &md));
    }

    let index = renderer
        .render_index()
        .unwrap_or_else(|err| panic!("render index: {err}"));
    pages.insert("index.md".into(), finish_page("index.md", &index));

    let config = renderer
        .render_config()
        .unwrap_or_else(|err| panic!("render config: {err}"));
    if !config.trim().is_empty() {
        let name = renderer.config_page();
        pages.insert(name.into(), finish_page(name, &config));
    }
    pages
}

fn collect_md(dir: &Path) -> BTreeMap<String, String> {
    let mut pages = BTreeMap::new();
    collect_md_into(dir, Path::new(""), &mut pages);
    pages
}

fn collect_md_into(dir: &Path, rel: &Path, pages: &mut BTreeMap<String, String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        let rel_path = rel.join(entry.file_name());
        if path.is_dir() {
            collect_md_into(&path, &rel_path, pages);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            let key = rel_path.to_str().expect("utf-8 path").replace('\\', "/");
            pages.insert(key, fs::read_to_string(&path).expect("read markdown"));
        }
    }
}

fn write_pages(dir: &Path, pages: &BTreeMap<String, String>) {
    fs::create_dir_all(dir).expect("create docs/user/cli");
    for (rel, body) in pages {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create command dir");
        }
        fs::write(&path, body).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
    }
    let committed = collect_md(dir);
    for rel in committed.keys() {
        if !pages.contains_key(rel) {
            fs::remove_file(dir.join(rel))
                .unwrap_or_else(|err| panic!("remove stale {rel}: {err}"));
        }
    }
    remove_empty_dirs(dir);
}

fn remove_empty_dirs(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let children: Vec<_> = entries.map(|e| e.expect("read dir entry").path()).collect();
    for child in &children {
        if child.is_dir() {
            remove_empty_dirs(child);
        }
    }
    if dir != cli_docs_dir()
        && fs::read_dir(dir)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false)
    {
        let _ = fs::remove_dir(dir);
    }
}

fn assert_omits_hidden(pages: &BTreeMap<String, String>) {
    assert!(
        pages.contains_key("index.md"),
        "expected generated index.md"
    );
    assert!(
        pages.contains_key("emerge.md"),
        "expected generated emerge.md"
    );
    assert!(
        pages.contains_key("query.md"),
        "expected generated query.md"
    );
    for (path, body) in pages {
        assert!(
            !path.contains("__helper") && !path.contains("__worker"),
            "hidden applet page leaked: {path}"
        );
        assert!(
            !body.contains("`em __helper") && !body.contains("`em __worker"),
            "hidden applet leaked into {path}"
        );
        assert!(
            !body.contains("](/"),
            "site-root markdown link left in {path}"
        );
    }
}

fn flag_longs_argv(cmd: &CommandMeta<'_>) -> BTreeSet<String> {
    cmd.cmd
        .flags
        .iter()
        .flat_map(|flag| flag.longs.iter().map(|s| (*s).to_string()))
        .collect()
}

fn flag_longs_kdl(cmd: &SpecCommand) -> BTreeSet<String> {
    cmd.flags
        .iter()
        .flat_map(|flag| flag.long.iter().cloned())
        .collect()
}

fn declared_flag_longs(mut flags: BTreeSet<String>) -> BTreeSet<String> {
    // usage-lib injects --help/--version when reading KDL; Cli::spec() does not list them.
    flags.remove("help");
    flags.remove("version");
    flags
}

fn assert_cmd_matches(path: &str, argv: &CommandMeta<'_>, kdl: &SpecCommand) {
    assert_eq!(argv.hide, kdl.hide, "hide mismatch at {path:?}");
    if !path.is_empty() {
        assert_eq!(argv.cmd.name, kdl.name, "name mismatch at {path:?}");
    }
    assert_eq!(
        declared_flag_longs(flag_longs_argv(argv)),
        declared_flag_longs(flag_longs_kdl(kdl)),
        "flag longs mismatch at {path:?}"
    );
    let argv_subs: BTreeSet<&str> = argv.subcommands.iter().map(|sub| sub.cmd.name).collect();
    let kdl_subs: BTreeSet<&str> = kdl.subcommands.keys().map(String::as_str).collect();
    assert_eq!(argv_subs, kdl_subs, "subcommands mismatch at {path:?}");
    for sub in argv.subcommands {
        let child = kdl
            .subcommands
            .get(sub.cmd.name)
            .unwrap_or_else(|| panic!("KDL missing subcommand {} under {path:?}", sub.cmd.name));
        let child_path = if path.is_empty() {
            sub.cmd.name.to_string()
        } else {
            format!("{path}/{}", sub.cmd.name)
        };
        assert_cmd_matches(&child_path, sub, child);
    }
}

#[test]
fn relative_href_keeps_nested_pages_in_their_directory() {
    assert_eq!(relative_href("index.md", "emerge.md"), "emerge.md");
    assert_eq!(
        relative_href("index.md", "query/belongs.md"),
        "query/belongs.md"
    );
    assert_eq!(
        relative_href("query.md", "query/belongs.md"),
        "query/belongs.md"
    );
    assert_eq!(
        relative_href("select/profile.md", "select/profile/list.md"),
        "profile/list.md"
    );
    assert_eq!(
        relative_href("select/profile/list.md", "select.md"),
        "../../select.md"
    );
    assert_ne!(
        relative_href("select/profile.md", "select/profile/list.md"),
        "./select/profile/list.md"
    );
}

#[test]
fn spec_round_trips_through_usage_lib() {
    let spec = parse_spec();
    assert_eq!(spec.bin, "em");
    let helper = spec
        .cmd
        .subcommands
        .get("__helper")
        .expect("__helper must remain in the spec (hide, not delete)");
    assert!(
        helper.hide,
        "__helper must be hide=true in the portable spec"
    );
    let worker = spec
        .cmd
        .subcommands
        .get("__worker")
        .expect("__worker must remain in the spec (hide, not delete)");
    assert!(
        worker.hide,
        "__worker must be hide=true in the portable spec"
    );
    assert!(spec.cmd.subcommands.contains_key("emerge"));
    assert!(spec.cmd.subcommands.contains_key("query"));

    let argv = Cli::spec();
    assert_eq!(argv.bin.unwrap_or(argv.name), spec.bin);
    assert_cmd_matches("", argv.root, &spec.cmd);
}

#[test]
fn generated_markdown_omits_hidden_applets() {
    let pages = generated_pages();
    assert_omits_hidden(&pages);
    let profile = pages.get("select/profile.md").expect("select/profile.md");
    assert!(
        profile.contains("](profile/list.md)"),
        "nested page must link a child relative to itself, not the tree root:\n{profile}"
    );
    let index = pages.get("index.md").expect("index.md");
    assert!(
        index.contains("](emerge.md)") && index.contains("](query/belongs.md)"),
        "index links should be repo-relative from docs/user/cli:\n{index}"
    );
}

#[test]
fn committed_cli_docs_match_spec() {
    let pages = generated_pages();
    assert_omits_hidden(&pages);
    let dir = cli_docs_dir();
    if std::env::var_os("UPDATE_CLI_DOCS").is_some() {
        write_pages(&dir, &pages);
        return;
    }
    let committed = collect_md(&dir);
    assert_eq!(
        committed, pages,
        "docs/user/cli is stale. \
         UPDATE_CLI_DOCS=1 cargo test -p portage-cli --test cli_docs"
    );
}
