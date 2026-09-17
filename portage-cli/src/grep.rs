//! `em grep` — search inside ebuilds and eclasses (qgrep workalike)
//!
//! Two discovery modes feed one shared matcher/emitter pipeline: the repo
//! tree (ebuilds or, with `-E`, eclasses) or, with `-J`, the installed
//! packages' own ebuild copies under the resolved VDB. See
//! `todo/grep-qgrep-workalike-plan.md` for the design this follows.

use std::io::Write as _;

use anyhow::{Context, Result};
use portage_atom::{Cpv, Dep};

use crate::cli::Cli;

/// One line-matching strategy. Case-insensitive literal matching also
/// routes through `Regex` (an escaped, case-insensitive pattern) so there is
/// exactly one case-folding code path.
#[derive(Clone)]
enum Matcher {
    Literal(memchr::memmem::Finder<'static>),
    Regex(regex::Regex),
}

impl Matcher {
    fn new(pattern: &str, regexp: bool, ignore_case: bool) -> Result<Self> {
        if regexp || ignore_case {
            let pat = if regexp {
                pattern.to_string()
            } else {
                regex::escape(pattern)
            };
            let re = regex::RegexBuilder::new(&pat)
                .case_insensitive(ignore_case)
                .build()
                .with_context(|| format!("invalid pattern: {pattern}"))?;
            Ok(Matcher::Regex(re))
        } else {
            Ok(Matcher::Literal(
                memchr::memmem::Finder::new(pattern.as_bytes()).into_owned(),
            ))
        }
    }

    fn is_match(&self, line: &str) -> bool {
        match self {
            Matcher::Literal(f) => f.find(line.as_bytes()).is_some(),
            Matcher::Regex(re) => re.is_match(line),
        }
    }
}

/// Which tree `em grep` walks.
enum Mode {
    Ebuilds,
    Eclasses,
    Installed,
}

/// How a matched file's label is rendered.
#[derive(Clone, Copy)]
enum Label {
    /// Repo-relative path to the file (the default).
    Path,
    /// `cat/pkg-ver`, from the ebuild's own CPV (`-N`).
    Atom,
    /// `cat/pkg-ver::repo` (`-R`, implies `Atom`).
    AtomRepo,
}

/// What to print once a file has been scanned.
#[derive(Clone, Copy)]
enum Emit {
    /// The matching (and, with `-B`/`-A`, context) lines.
    Lines,
    /// `label:count` for every file with at least one match.
    Count,
    /// The label of every file that matched.
    List,
    /// The label of every file that did *not* match.
    ListInvert,
}

/// One atom-shaped restriction on which packages to search (`em grep`'s
/// `TARGETS`): a bare category (`cat/`), a bare package name, or a full
/// dependency atom.
enum Target {
    Category(String),
    Name(String),
    Dep(Box<Dep>),
}

impl Target {
    fn parse(s: &str) -> Self {
        if let Some(cat) = s.strip_suffix('/') {
            return Target::Category(cat.to_string());
        }
        if !s.contains('/') {
            return Target::Name(s.to_string());
        }
        match Dep::parse(s) {
            Ok(dep) => Target::Dep(Box::new(dep)),
            Err(_) => Target::Name(s.to_string()),
        }
    }

    fn matches(&self, cpv: &Cpv) -> bool {
        match self {
            Target::Category(cat) => cpv.cpn.category.as_str() == cat,
            Target::Name(name) => cpv.cpn.package.as_str() == name,
            Target::Dep(dep) => dep.matches_cpv(cpv, None),
        }
    }
}

fn parse_targets(raw: &[String]) -> Vec<Target> {
    raw.iter().map(|s| Target::parse(s)).collect()
}

fn targets_match(targets: &[Target], cpv: &Cpv) -> bool {
    targets.is_empty() || targets.iter().any(|t| t.matches(cpv))
}

/// One file queued for the match phase: its pre-rendered label and path.
#[derive(Clone)]
struct Job {
    label: String,
    path: camino::Utf8PathBuf,
}

pub(crate) struct Options {
    pub pattern: String,
    pub targets: Vec<String>,
    pub invert_match: bool,
    pub ignore_case: bool,
    pub atom_name: bool,
    pub count: bool,
    pub list: bool,
    pub list_invert: bool,
    pub regexp: bool,
    pub installed: bool,
    pub eclass: bool,
    pub skip_comments: bool,
    pub show_repo: bool,
    pub skip: Option<String>,
    pub before: usize,
    pub after: usize,
    pub no_filename: bool,
    pub line_numbers: bool,
}

/// Open every repo `em grep` should search, skipping (with a warning) any
/// that fail to open — same policy as `em search`'s `open_repos`. Returns
/// each repo paired with the short name used for `Label::AtomRepo`.
fn open_repos(
    repo_paths: &[std::path::PathBuf],
) -> Result<Vec<(portage_repo::Repository, String)>> {
    if repo_paths.is_empty() {
        anyhow::bail!("no repositories configured");
    }
    let mut repos = Vec::with_capacity(repo_paths.len());
    for p in repo_paths {
        match crate::repo_open::open(p) {
            Ok(r) => {
                let name = r
                    .path()
                    .file_name()
                    .unwrap_or(r.path().as_str())
                    .to_string();
                repos.push((r, name));
            }
            Err(e) => crate::style::warn_line!("skipping {}: {e}", p.display()),
        }
    }
    if repos.is_empty() {
        anyhow::bail!("no usable repositories");
    }
    Ok(repos)
}

fn render_ebuild_label(
    label_kind: Label,
    repo_path: &camino::Utf8Path,
    repo_name: &str,
    ebuild: &portage_repo::Ebuild,
) -> String {
    match label_kind {
        Label::Path => ebuild
            .path()
            .strip_prefix(repo_path)
            .map(|p| p.to_string())
            .unwrap_or_else(|_| ebuild.path().to_string()),
        Label::Atom => ebuild.cpv().to_string(),
        Label::AtomRepo => format!("{}::{repo_name}", ebuild.cpv()),
    }
}

/// Discover the jobs to search: every ebuild/eclass/installed-copy that
/// passes the target filter, in a deterministic order (per repo, sorted by
/// CPV; eclasses sorted by name).
fn discover(cli: &Cli, opts: &Options, mode: &Mode, label_kind: Label) -> Result<Vec<Job>> {
    match mode {
        Mode::Eclasses => {
            if !opts.targets.is_empty() {
                crate::style::warn_line!("-E/--eclass ignores TARGETS");
            }
            let repos = open_repos(&cli.search_repos())?;
            let mut jobs = Vec::new();
            for (repo, name) in &repos {
                let mut names = repo.eclasses()?;
                names.sort();
                for eclass in names {
                    let path = repo.path().join("eclass").join(format!("{eclass}.eclass"));
                    let label = match label_kind {
                        Label::Path => format!("eclass/{eclass}.eclass"),
                        Label::Atom | Label::AtomRepo => format!("eclass/{eclass}::{name}"),
                    };
                    jobs.push(Job { label, path });
                }
            }
            Ok(jobs)
        }
        Mode::Installed => {
            let targets = parse_targets(&opts.targets);
            let vdb = crate::vdb::open_cli_vdb(cli)?;
            let mut pkgs: Vec<_> = vdb
                .packages()
                .into_iter()
                .filter(|p| targets_match(&targets, p.cpv()))
                .collect();
            pkgs.sort_by(|a, b| a.cpv().cmp(b.cpv()));
            Ok(pkgs
                .into_iter()
                .map(|p| {
                    let path = p.path().join(format!("{}.ebuild", p.pf()));
                    let label = match label_kind {
                        Label::Path => path.to_string(),
                        Label::Atom => p.cpv().to_string(),
                        Label::AtomRepo => format!("{}::installed", p.cpv()),
                    };
                    Job { label, path }
                })
                .collect())
        }
        Mode::Ebuilds => {
            let targets = parse_targets(&opts.targets);
            let repos = open_repos(&cli.search_repos())?;
            let mut jobs = Vec::new();
            for (repo, name) in &repos {
                let ebuilds = repo.ebuilds()?.collect_vec();
                for eb in ebuilds {
                    if !targets_match(&targets, eb.cpv()) {
                        continue;
                    }
                    let label = render_ebuild_label(label_kind, repo.path(), name, &eb);
                    jobs.push(Job {
                        label,
                        path: eb.path().to_owned(),
                    });
                }
            }
            Ok(jobs)
        }
    }
}

/// A leading-context ring buffer: keeps the last `n` lines seen so a match
/// can print them as `-B` context.
struct Before {
    buf: std::collections::VecDeque<(usize, String)>,
    cap: usize,
}

impl Before {
    fn new(cap: usize) -> Self {
        Self {
            buf: std::collections::VecDeque::with_capacity(cap),
            cap,
        }
    }

    fn push(&mut self, idx: usize, line: &str) {
        if self.cap == 0 {
            return;
        }
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back((idx, line.to_string()));
    }

    fn drain(&mut self) -> Vec<(usize, String)> {
        self.buf.drain(..).collect()
    }
}

/// Formatting knobs `scan_file` needs, bundled so the call site doesn't pass
/// eight loose booleans/numbers.
struct ScanOpts<'a> {
    matcher: &'a Matcher,
    skip: Option<&'a Matcher>,
    invert: bool,
    skip_comments: bool,
    before: usize,
    after: usize,
    line_numbers: bool,
    no_filename: bool,
}

fn fmt_line(label: &str, idx: usize, sep: char, line: &str, opts: &ScanOpts) -> String {
    if opts.no_filename {
        format!("{line}\n")
    } else if opts.line_numbers {
        format!("{label}{sep}{}{sep}{line}\n", idx + 1)
    } else {
        format!("{label}{sep}{line}\n")
    }
}

/// Scan one file's already-read text against `opts.matcher`. Appends
/// formatted matching (and context) lines to `out` and returns the number
/// of matching lines — the caller decides what `out`/the count mean for the
/// active `Emit` mode.
fn scan_file(out: &mut String, label: &str, text: &str, opts: &ScanOpts) -> usize {
    let mut count = 0usize;
    let mut before_buf = Before::new(opts.before);
    let mut after_remaining = 0usize;

    for (idx, line) in text.lines().enumerate() {
        if opts.skip_comments && line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(skip) = opts.skip
            && skip.is_match(line)
        {
            continue;
        }
        let hit = opts.matcher.is_match(line) ^ opts.invert;
        if hit {
            count += 1;
            for (bidx, bline) in before_buf.drain() {
                out.push_str(&fmt_line(label, bidx, '-', &bline, opts));
            }
            out.push_str(&fmt_line(label, idx, ':', line, opts));
            after_remaining = opts.after;
        } else if after_remaining > 0 {
            out.push_str(&fmt_line(label, idx, '-', line, opts));
            after_remaining -= 1;
        } else {
            before_buf.push(idx, line);
        }
    }
    count
}

/// Run the match phase over `jobs`, fanning reads out across
/// `available_parallelism()` blocking tasks (same shape as
/// `portage-repo`'s `cache.rs::read_and_decode`), then print results in job
/// order. Returns the total number of files that matched (for the
/// no-matches exit-1 decision), regardless of `emit`.
async fn run_jobs(jobs: Vec<Job>, opts: &Options, emit: Emit) -> Result<usize> {
    if jobs.is_empty() {
        return Ok(0);
    }
    let matcher = Matcher::new(&opts.pattern, opts.regexp, opts.ignore_case)?;
    let skip = opts
        .skip
        .as_deref()
        .map(|s| Matcher::new(s, opts.regexp, opts.ignore_case))
        .transpose()?;

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let chunk_size = jobs.len().div_ceil(workers).max(1);

    let invert = opts.invert_match;
    let skip_comments = opts.skip_comments;
    let before = opts.before;
    let after = opts.after;
    let line_numbers = opts.line_numbers;
    let no_filename = opts.no_filename;

    let mut handles = Vec::new();
    for chunk in jobs.chunks(chunk_size) {
        let chunk: Vec<Job> = chunk.to_vec();
        let matcher = matcher.clone();
        let skip = skip.clone();
        handles.push(tokio::task::spawn_blocking(move || {
            let scan = ScanOpts {
                matcher: &matcher,
                skip: skip.as_ref(),
                invert,
                skip_comments,
                before,
                after,
                line_numbers,
                no_filename,
            };
            let mut buf = String::new();
            let mut matched_files = 0usize;
            for job in &chunk {
                let Ok(text) = std::fs::read_to_string(&job.path) else {
                    continue;
                };
                let mut lines = String::new();
                let count = scan_file(&mut lines, &job.label, &text, &scan);
                if count == 0 {
                    if matches!(emit, Emit::ListInvert) {
                        buf.push_str(&job.label);
                        buf.push('\n');
                    }
                    continue;
                }
                matched_files += 1;
                match emit {
                    Emit::Lines => buf.push_str(&lines),
                    Emit::Count => buf.push_str(&format!("{}:{count}\n", job.label)),
                    Emit::List => {
                        buf.push_str(&job.label);
                        buf.push('\n');
                    }
                    Emit::ListInvert => {}
                }
            }
            (buf, matched_files)
        }));
    }

    let mut stdout = anstream::stdout();
    let mut total_matches = 0usize;
    for h in handles {
        if let Ok((buf, matched_files)) = h.await {
            total_matches += matched_files;
            stdout.write_all(buf.as_bytes()).ok();
        }
    }
    Ok(total_matches)
}

pub(crate) async fn run(cli: &Cli, opts: Options) -> Result<()> {
    let label_kind = if opts.show_repo {
        Label::AtomRepo
    } else if opts.atom_name {
        Label::Atom
    } else {
        Label::Path
    };
    let mode = if opts.eclass {
        Mode::Eclasses
    } else if opts.installed {
        Mode::Installed
    } else {
        Mode::Ebuilds
    };

    let jobs = discover(cli, &opts, &mode, label_kind)?;

    let emit = if opts.count {
        Emit::Count
    } else if opts.list {
        Emit::List
    } else if opts.list_invert {
        Emit::ListInvert
    } else {
        Emit::Lines
    };

    let matches = run_jobs(jobs, &opts, emit).await?;
    if matches == 0 {
        anyhow::bail!(crate::NoMatches);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_matcher_finds_substring() {
        let m = Matcher::new("PYTHON_COMPAT", false, false).unwrap();
        assert!(m.is_match("PYTHON_COMPAT=( python3_12 )"));
    }

    #[test]
    fn literal_matcher_is_case_sensitive_by_default() {
        let m = Matcher::new("depend", false, false).unwrap();
        assert!(!m.is_match("DEPEND=\"foo\""));
        assert!(m.is_match("depend=\"foo\""));
    }

    #[test]
    fn ignore_case_matches_either_case() {
        let m = Matcher::new("depend", false, true).unwrap();
        assert!(m.is_match("DEPEND=\"foo\""));
        assert!(m.is_match("depend=\"foo\""));
    }

    #[test]
    fn regex_matcher_compiles_and_matches() {
        let m = Matcher::new("PYTHON_COM.AT", true, false).unwrap();
        assert!(m.is_match("PYTHON_COMPAT=( python3_12 )"));
    }

    #[test]
    fn literal_pattern_is_not_treated_as_regex() {
        // A `.` in a literal pattern must not act as a wildcard.
        let m = Matcher::new("a.b", false, false).unwrap();
        assert!(m.is_match("a.b"));
        assert!(!m.is_match("aXb"));
    }

    #[test]
    fn target_category_matches_only_that_category() {
        let t = Target::parse("dev-lang/");
        let hit: Cpv = Cpv::parse("dev-lang/python-3.12").unwrap();
        let miss: Cpv = Cpv::parse("dev-python/setuptools-79.0.1").unwrap();
        assert!(t.matches(&hit));
        assert!(!t.matches(&miss));
    }

    #[test]
    fn target_bare_name_matches_package_name() {
        let t = Target::parse("setuptools");
        let hit: Cpv = Cpv::parse("dev-python/setuptools-79.0.1").unwrap();
        let miss: Cpv = Cpv::parse("dev-python/pip-24.0").unwrap();
        assert!(t.matches(&hit));
        assert!(!t.matches(&miss));
    }

    #[test]
    fn target_dep_atom_matches_version_range() {
        let t = Target::parse(">=dev-python/setuptools-80");
        let hit: Cpv = Cpv::parse("dev-python/setuptools-83.0.0").unwrap();
        let miss: Cpv = Cpv::parse("dev-python/setuptools-79.0.1").unwrap();
        assert!(t.matches(&hit));
        assert!(!t.matches(&miss));
    }

    #[test]
    fn empty_targets_matches_everything() {
        let targets: Vec<Target> = Vec::new();
        let cpv: Cpv = Cpv::parse("dev-python/setuptools-79.0.1").unwrap();
        assert!(targets_match(&targets, &cpv));
    }

    fn scan_opts<'a>(matcher: &'a Matcher, skip: Option<&'a Matcher>) -> ScanOpts<'a> {
        ScanOpts {
            matcher,
            skip,
            invert: false,
            skip_comments: false,
            before: 0,
            after: 0,
            line_numbers: false,
            no_filename: false,
        }
    }

    #[test]
    fn scan_file_default_label_uses_colon_separator() {
        let matcher = Matcher::new("hit", false, false).unwrap();
        let mut out = String::new();
        let count = scan_file(
            &mut out,
            "pkg/pkg-1.ebuild",
            "no\nhit here\nno",
            &scan_opts(&matcher, None),
        );
        assert_eq!(count, 1);
        assert_eq!(out, "pkg/pkg-1.ebuild:hit here\n");
    }

    #[test]
    fn scan_file_context_uses_dash_separator() {
        let matcher = Matcher::new("hit", false, false).unwrap();
        let mut out = String::new();
        let mut opts = scan_opts(&matcher, None);
        opts.before = 1;
        opts.after = 1;
        scan_file(&mut out, "pkg/pkg-1.ebuild", "before\nhit here\nafter", &opts);
        assert_eq!(
            out,
            "pkg/pkg-1.ebuild-before\npkg/pkg-1.ebuild:hit here\npkg/pkg-1.ebuild-after\n"
        );
    }

    #[test]
    fn scan_file_skip_comments_ignores_comment_lines() {
        let matcher = Matcher::new("hit", false, false).unwrap();
        let mut out = String::new();
        let mut opts = scan_opts(&matcher, None);
        opts.skip_comments = true;
        let count = scan_file(
            &mut out,
            "pkg/pkg-1.ebuild",
            "# hit in a comment\nhit for real",
            &opts,
        );
        assert_eq!(count, 1);
        assert_eq!(out, "pkg/pkg-1.ebuild:hit for real\n");
    }

    #[test]
    fn scan_file_invert_prints_non_matching_lines() {
        let matcher = Matcher::new("hit", false, false).unwrap();
        let mut out = String::new();
        let mut opts = scan_opts(&matcher, None);
        opts.invert = true;
        let count = scan_file(&mut out, "pkg/pkg-1.ebuild", "hit\nmiss", &opts);
        assert_eq!(count, 1);
        assert_eq!(out, "pkg/pkg-1.ebuild:miss\n");
    }

    #[test]
    fn scan_file_no_filename_omits_label() {
        let matcher = Matcher::new("hit", false, false).unwrap();
        let mut out = String::new();
        let mut opts = scan_opts(&matcher, None);
        opts.no_filename = true;
        scan_file(&mut out, "pkg/pkg-1.ebuild", "hit here", &opts);
        assert_eq!(out, "hit here\n");
    }

    #[test]
    fn scan_file_line_numbers_are_one_indexed() {
        let matcher = Matcher::new("hit", false, false).unwrap();
        let mut out = String::new();
        let mut opts = scan_opts(&matcher, None);
        opts.line_numbers = true;
        scan_file(&mut out, "pkg/pkg-1.ebuild", "no\nhit", &opts);
        assert_eq!(out, "pkg/pkg-1.ebuild:2:hit\n");
    }

    #[test]
    fn scan_file_skip_pattern_excludes_matching_lines() {
        let matcher = Matcher::new("hit", false, false).unwrap();
        let skip = Matcher::new("skip-me", false, false).unwrap();
        let mut out = String::new();
        let count = scan_file(
            &mut out,
            "pkg/pkg-1.ebuild",
            "hit skip-me\nhit for real",
            &scan_opts(&matcher, Some(&skip)),
        );
        assert_eq!(count, 1);
        assert_eq!(out, "pkg/pkg-1.ebuild:hit for real\n");
    }
}
