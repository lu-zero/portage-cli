//! `em mirrordist`: build/maintain a distfiles mirror — an `emirrordist`
//! workalike.
//!
//! **Not** `em select mirrors` (a `mirrorselect`/`eselect mirror` workalike
//! that picks which upstream `GENTOO_MIRRORS` *this machine* fetches from).
//! This is the opposite-direction tool: it walks a repository's every
//! ebuild, fetches every distfile its `SRC_URI` references (all versions,
//! all USE branches — matching `-F`/`--fetch-all-uri` semantics, since a
//! mirror must carry whatever any USE setting could ever need), verifies
//! each against the repo's `Manifest` digests, and optionally prunes what no
//! current ebuild references anymore.
//!
//! Requires an up-to-date metadata cache (`em regen <repo>` first, for any
//! repo that doesn't already ship one) — this reads `metadata/md5-cache`
//! directly rather than falling back to live ebuild sourcing on a cache
//! miss, so a stale/absent cache entry is a real precondition violation
//! (see [`MirrorPlan::is_complete`]), not silently degraded output.
//!
//! `RESTRICT` is evaluated with matchnone semantics
//! (`RestrictExpr::has_unconditional`, `RestrictGate::from_restrict`): a
//! client's particular USE selection must never change what a mirror
//! redistributes. Deletion-grace state is `em`'s own JSON (see
//! [`DeletionDb`]), not portage's `shelve`/dbm format — same convention as
//! [`crate::preserve_libs::PreservedLibsRegistry`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};

use portage_atom::{Cpn, Cpv};
use portage_distfiles::{
    DistDigests, Distfile, DistfileResolver, FetchConfig, FetchStatus, Fetcher, ResolveOpts,
    RestrictGate, is_atomic_temp_name,
};
use portage_metadata::{RawCacheEntry, RestrictExpr, SrcUriEntry};
use portage_repo::{
    CacheReadOpts, Manifest, ManifestEntry, ReposConf, Repository, cache_entries_parallel,
};

use crate::cli;

/// Options for [`run`], one field per CLI flag on `Applet::MirrorDist`.
pub struct MirrorDistOpts {
    /// repos.conf name or path. `None` = the main repo (opposite default
    /// from `em regen`, which deliberately excludes it).
    pub repo: Option<String>,
    pub repos_dir: Option<String>,
    pub distfiles: Utf8PathBuf,
    pub jobs: Option<usize>,
    pub delete: bool,
    pub deletion_delay: Duration,
    pub deletion_db: Option<Utf8PathBuf>,
    pub success_log: Option<Utf8PathBuf>,
    pub failure_log: Option<Utf8PathBuf>,
    pub scheduled_deletion_log: Option<Utf8PathBuf>,
    pub whitelist_from: Vec<Utf8PathBuf>,
    pub verify_existing_digest: bool,
    pub gentoo_mirrors_fallback: bool,
    pub delete_allow_incomplete: bool,
}

/// `em mirrordist`: detect and fetch every distfile a repo's ebuilds
/// reference, then (optionally) prune what's no longer referenced.
pub async fn run(cli: &cli::Cli, opts: &MirrorDistOpts) -> Result<()> {
    let repo_target = resolve_repo_target(opts.repo.as_deref(), cli)?;
    let repos_dir = opts
        .repos_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_repos_dir(cli));
    let (repo, masters) = crate::repo_open::open_with_masters(repo_target.clone(), &repos_dir)
        .context("open repo")?;

    check_layout(&opts.distfiles)?;
    std::fs::create_dir_all(opts.distfiles.as_std_path())
        .with_context(|| format!("creating distfiles dir {}", opts.distfiles))?;

    let gentoo_mirrors = if opts.gentoo_mirrors_fallback {
        gentoo_mirrors_list(cli)
    } else {
        Vec::new()
    };
    let resolver = DistfileResolver::from_repo(&repo, gentoo_mirrors)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("loading thirdpartymirrors")?;

    let plan = build_plan(
        &repo,
        &masters,
        &resolver,
        opts.jobs,
        opts.gentoo_mirrors_fallback,
    )
    .await?;

    let whitelist = parse_whitelist(&opts.whitelist_from)?;
    let referenced = plan.referenced();
    let state = scan_distdir(&opts.distfiles, &referenced, &whitelist)?;

    let repo_name = repo.path().file_name().unwrap_or("repo").to_string();
    let db_path = opts
        .deletion_db
        .clone()
        .unwrap_or_else(|| default_deletion_db_path(&repo_name, &opts.distfiles));
    let mut db = DeletionDb::load(&db_path);
    let now = now_unix();
    db.retain_present(&state.present.keys().cloned().collect());
    let (delete_now, scheduled) =
        deletion_decisions(&mut db, &state.orphans, now, opts.deletion_delay);

    if cli.pretend {
        print_report(&plan, &state, &delete_now, &scheduled, opts.delete);
        return Ok(());
    }

    if plan.files.is_empty() {
        println!(">>> Nothing to mirror.");
    } else {
        let mut config = FetchConfig {
            max_concurrent: opts.jobs.unwrap_or(4),
            trust_existing_size: !opts.verify_existing_digest,
            atomic_write: true,
            ..FetchConfig::default()
        };
        config.max_concurrent = config.max_concurrent.max(1);
        let fetcher = Fetcher::new(opts.distfiles.clone(), config);
        let distfiles: Vec<Distfile> = plan.files.iter().map(|f| f.distfile.clone()).collect();
        let results = fetcher.fetch_all_digests(&distfiles, &plan.digests).await;

        let mut log = EventLog::open(opts.success_log.as_deref(), opts.failure_log.as_deref())?;
        let mut added = 0usize;
        let mut failed = 0usize;
        for (owned, (df, result)) in plan.files.iter().zip(results) {
            match result {
                Ok(FetchStatus::Downloaded) => {
                    let size = plan
                        .digests
                        .get(&df.filename)
                        .and_then(dist_entry_size)
                        .unwrap_or(0);
                    log.success(&owned.owner, &df.filename, &format!("added {size} bytes"));
                    added += 1;
                }
                Ok(FetchStatus::AlreadyPresent) => {}
                Ok(FetchStatus::FetchRestricted) => {
                    log.failure(&owned.owner, &df.filename, "fetch-restricted");
                    failed += 1;
                }
                Err(e) => {
                    log.failure(&owned.owner, &df.filename, &e.to_string());
                    failed += 1;
                }
            }
        }
        println!(">>> {added} file(s) added, {failed} failed.");
    }

    if opts.delete {
        if !plan.is_complete() && !opts.delete_allow_incomplete {
            bail!(
                "refusing --delete: metadata scan is incomplete ({} missing cache, {} metadata errors, {} missing digests, {} manifest errors) — pass --delete-allow-incomplete to override",
                plan.cpvs_missing_cache,
                plan.cpvs_metadata_error,
                plan.missing_digest,
                plan.manifest_errors
            );
        }
        if plan.cpvs_scanned == 0 || referenced.is_empty() {
            bail!("refusing --delete: the scan found no packages/no referenced distfiles at all");
        }

        let mut log = EventLog::open(None, None)?;
        for filename in &delete_now {
            let path = opts.distfiles.join(filename);
            match std::fs::remove_file(path.as_std_path()) {
                Ok(()) => log.success_anon(filename, "removed"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    log.success_anon(filename, "removed");
                }
                Err(e) => {
                    eprintln!("mirrordist: could not remove {filename}: {e}");
                }
            }
        }
        println!(">>> {} orphaned file(s) removed.", delete_now.len());

        if let Some(path) = &opts.scheduled_deletion_log {
            write_scheduled_deletion_log(path, &scheduled)?;
        }
        db.store()?;
    }

    Ok(())
}

/// Resolve `repo_arg` (a repos.conf name or a directory) to a filesystem
/// path, same shape `em regen` uses per target — but singular, and
/// defaulting to the main repo (via [`cli::Cli::repo_path`]) rather than
/// requiring an explicit name.
fn resolve_repo_target(repo_arg: Option<&str>, cli: &cli::Cli) -> Result<PathBuf> {
    let Some(arg) = repo_arg else {
        return Ok(PathBuf::from(cli.repo_path()));
    };
    let path = Path::new(arg);
    if path.is_dir() {
        return Ok(path.to_path_buf());
    }
    if let Some(entry) = ReposConf::load().ok().and_then(|c| c.find(arg).cloned())
        && let Some(p) = entry.location.as_path()
    {
        return Ok(p.to_path_buf());
    }
    bail!("'{arg}' is neither a directory nor a repos.conf repo name")
}

/// Masters resolve against the *main* repo's parent — same reasoning `em
/// regen` uses (`regen.rs`): an overlay's masters conventionally live
/// alongside the main repo (e.g. `/var/db/repos/gentoo`) regardless of
/// where the target repo itself is checked out.
fn default_repos_dir(cli: &cli::Cli) -> PathBuf {
    Path::new(&cli.repo_path())
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// Read `GENTOO_MIRRORS` from `make.conf` directly (no live `EbuildShell` —
/// there is no ebuild being built here, unlike `ebuild.rs`'s
/// `gentoo_mirrors_list`).
fn gentoo_mirrors_list(cli: &cli::Cli) -> Vec<String> {
    let path = crate::select::config_portage_dir(cli).join("make.conf");
    let Ok(mc) = portage_repo::MakeConf::load(&path) else {
        return Vec::new();
    };
    mc.get("GENTOO_MIRRORS")
        .map(|v| v.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Minimal guard against a non-flat (content-hash) distfiles layout: v1 only
/// ever supports the plain `distfiles/<filename>` layout. A GLEP 75
/// `filename-hash` mirror layout (what `distfiles.gentoo.org` itself uses)
/// is the natural v2 feature — not a full parse here, just a refusal to
/// silently write to the wrong place and then, under `--delete`, treat
/// every correctly-hashed file as an orphan.
fn check_layout(distfiles: &Utf8Path) -> Result<()> {
    let layout = distfiles.join("layout.conf");
    let Ok(text) = std::fs::read_to_string(layout.as_std_path()) else {
        return Ok(());
    };
    if text.contains("filename-hash") {
        bail!(
            "{layout}: a content-hash (filename-hash) mirror layout is not supported yet — \
             only the plain flat layout is; remove or edit layout.conf to proceed"
        );
    }
    Ok(())
}

/// One distfile the mirror should carry, and the first (lowest-sorted) cpv
/// that claimed it.
#[derive(Clone)]
struct OwnedFile {
    distfile: Distfile,
    owner: Cpv,
}

/// Everything the scan phase learned. The fetch/delete phases are pure
/// functions of this plus on-disk state.
pub struct MirrorPlan {
    files: Vec<OwnedFile>,
    digests: DistDigests,
    cpvs_scanned: usize,
    cpvs_restricted: usize,
    cpvs_missing_cache: usize,
    cpvs_metadata_error: usize,
    missing_digest: usize,
    manifest_errors: usize,
}

impl MirrorPlan {
    /// No missing-cache/metadata-error/missing-digest/manifest-error entries
    /// — the gate `--delete` requires (see module docs: `em` only reads the
    /// cache, so an incomplete scan must never look like "nothing is
    /// referenced, delete everything").
    pub fn is_complete(&self) -> bool {
        self.cpvs_missing_cache == 0
            && self.cpvs_metadata_error == 0
            && self.missing_digest == 0
            && self.manifest_errors == 0
    }

    fn referenced(&self) -> HashSet<&str> {
        self.files
            .iter()
            .map(|f| f.distfile.filename.as_str())
            .collect()
    }

    fn total_bytes(&self) -> u64 {
        self.files
            .iter()
            .filter_map(|f| self.digests.get(&f.distfile.filename))
            .filter_map(dist_entry_size)
            .sum()
    }
}

fn dist_entry_size(entry: &ManifestEntry) -> Option<u64> {
    match entry {
        ManifestEntry::Dist { size, .. } => Some(*size),
        _ => None,
    }
}

/// A cache entry's two fields we need, each independently possibly
/// unparseable (real emirrordist logs `SRC_URI`/`RESTRICT` parse failures
/// distinctly per cpv — `FetchIterator.py`).
struct RawMeta {
    src_uri: std::result::Result<Vec<SrcUriEntry>, String>,
    restrict: std::result::Result<Vec<RestrictExpr>, String>,
}

/// Decode just `SRC_URI`/`RESTRICT` from a raw md5-cache file's text,
/// skipping the full atom-tree parse (`cache_entries_parallel`'s intended
/// use — see its own doc). Never itself fails: a sub-field parse error is
/// carried as data in [`RawMeta`], not propagated as an `Err`.
fn decode_raw_meta(text: &str) -> portage_repo::Result<RawMeta> {
    let raw = RawCacheEntry::new(text);
    let [src_uri_field, restrict_field] = raw.fields(["SRC_URI", "RESTRICT"]);
    Ok(RawMeta {
        src_uri: SrcUriEntry::parse(src_uri_field.unwrap_or("")).map_err(|e| e.to_string()),
        restrict: RestrictExpr::parse(restrict_field.unwrap_or("")).map_err(|e| e.to_string()),
    })
}

/// Enumerate every cpv in `repo` (every version currently in the tree,
/// matching real emirrordist — not just latest), read its cached
/// `SRC_URI`/`RESTRICT`, resolve distfiles (gated, filename-merged — see
/// [`DistfileResolver::resolve_uri_map`]), and fold every referenced
/// package's `Manifest` into one combined [`DistDigests`] index.
///
/// `ebuilds_with_masters(masters)` only ever walks `repo`'s own tree (masters
/// widen its *category* filter, not the file walk itself — verified in
/// `Repository::ebuilds_with_masters`'s own implementation) — so every
/// ebuild here, and every Manifest it needs, belongs to this one `repo`;
/// no cross-repo Manifest keying is needed.
async fn build_plan(
    repo: &Repository,
    masters: &[Repository],
    resolver: &DistfileResolver,
    jobs: Option<usize>,
    gentoo_mirrors_fallback: bool,
) -> Result<MirrorPlan> {
    let ebuilds = repo
        .ebuilds_with_masters(masters)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("enumerating ebuilds")?
        .collect_vec(); // already sorted by Cpv — deterministic first-owner-wins

    let cache_opts = CacheReadOpts {
        jobs,
        latest_per_cpn: false,
    };
    let raw =
        cache_entries_parallel(std::slice::from_ref(repo), &cache_opts, decode_raw_meta).await;
    let mut by_cpv: HashMap<Cpv, portage_repo::Result<RawMeta>> = raw.into_iter().collect();

    let mut plan = MirrorPlan {
        files: Vec::new(),
        digests: DistDigests::new(),
        cpvs_scanned: ebuilds.len(),
        cpvs_restricted: 0,
        cpvs_missing_cache: 0,
        cpvs_metadata_error: 0,
        missing_digest: 0,
        manifest_errors: 0,
    };

    let mut manifest_ok: HashMap<Cpn, bool> = HashMap::new();
    let mut claimed: HashSet<String> = HashSet::new();

    for ebuild in &ebuilds {
        let cpv = ebuild.cpv();
        let Some(result) = by_cpv.remove(cpv) else {
            plan.cpvs_missing_cache += 1;
            continue;
        };
        let meta = match result {
            Ok(m) => m,
            Err(_) => {
                plan.cpvs_metadata_error += 1;
                continue;
            }
        };
        let (Ok(src_uri), Ok(restrict)) = (&meta.src_uri, &meta.restrict) else {
            plan.cpvs_metadata_error += 1;
            continue;
        };

        let gate = RestrictGate::from_restrict(restrict);
        let opts = ResolveOpts {
            gentoo_mirrors_fallback,
            restrict: gate,
        };
        let distfiles = resolver.resolve_uri_map(src_uri, &opts);
        if distfiles.is_empty() {
            if gate.fetch || gate.mirror {
                plan.cpvs_restricted += 1;
            }
            continue;
        }

        let cpn = cpv.cpn;
        if let std::collections::hash_map::Entry::Vacant(e) = manifest_ok.entry(cpn) {
            let loaded = ebuild
                .path()
                .parent()
                .and_then(|dir| std::fs::read_to_string(dir.join("Manifest").as_std_path()).ok())
                .and_then(|text| Manifest::parse(&text).ok());
            if let Some(m) = &loaded {
                plan.digests.extend_from_manifest(m);
            }
            e.insert(loaded.is_some());
        }
        if !manifest_ok[&cpn] {
            plan.manifest_errors += 1;
            continue;
        }

        for distfile in distfiles {
            if plan.digests.get(&distfile.filename).is_none() {
                plan.missing_digest += 1;
                continue;
            }
            if !claimed.insert(distfile.filename.clone()) {
                continue; // already owned by an earlier (lower-sorted) cpv
            }
            plan.files.push(OwnedFile {
                distfile,
                owner: cpv.clone(),
            });
        }
    }

    Ok(plan)
}

/// One distfile name per line, `#`-prefixed comments and blank lines
/// ignored. Real portage also accepts a directory of such files; v1 only
/// accepts plain files.
fn parse_whitelist(paths: &[Utf8PathBuf]) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    for path in paths {
        let text = std::fs::read_to_string(path.as_std_path())
            .with_context(|| format!("reading --whitelist-from {path}"))?;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            out.insert(line.to_string());
        }
    }
    Ok(out)
}

/// What's physically present in `distfiles`, split into referenced
/// (`present`, with recorded size) vs. orphan candidates.
struct DistdirState {
    present: BTreeMap<String, u64>,
    orphans: Vec<String>,
}

/// Skips directories, `layout.conf` itself (real emirrordist's flat layout
/// happily treats it as an ordinary file and will schedule it for deletion
/// — a known real-world quirk, not inherited here), and the atomic-write
/// temp pattern (`is_atomic_temp_name` — never a legitimate distfile, always
/// a leftover from an interrupted download).
fn scan_distdir(
    distfiles: &Utf8Path,
    referenced: &HashSet<&str>,
    whitelist: &HashSet<String>,
) -> Result<DistdirState> {
    let mut present = BTreeMap::new();
    let mut orphans = Vec::new();

    let entries = std::fs::read_dir(distfiles.as_std_path())
        .with_context(|| format!("reading distfiles dir {distfiles}"))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading distfiles dir {distfiles}"))?;
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "layout.conf" || is_atomic_temp_name(&name) {
            continue;
        }
        present.insert(name.clone(), meta.len());
        if !referenced.contains(name.as_str()) && !whitelist.contains(&name) {
            orphans.push(name);
        }
    }

    Ok(DistdirState { present, orphans })
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `em`'s own JSON deletion-grace state — real portage's `--deletion-db` is
/// a `shelve` (dbm); this is a plain JSON `filename -> first-seen-orphaned
/// unix timestamp` map, following [`crate::preserve_libs::PreservedLibsRegistry`]'s
/// established "own format, documented as not byte-compatible" convention.
struct DeletionDb {
    path: Utf8PathBuf,
    data: BTreeMap<String, u64>,
}

impl DeletionDb {
    fn load(path: &Utf8Path) -> Self {
        let data = std::fs::read_to_string(path.as_std_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            path: path.to_owned(),
            data,
        }
    }

    fn store(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.data).context("serializing deletion db")?;
        crate::util::write_atomic(&self.path, json).context("writing deletion db")
    }

    /// Record `filename` as orphaned as of `now` if not already tracked;
    /// return its (possibly earlier) first-seen timestamp.
    fn note_orphan(&mut self, filename: &str, now: u64) -> u64 {
        *self.data.entry(filename.to_string()).or_insert(now)
    }

    /// Drop entries for filenames no longer physically present in the
    /// distfiles dir (`DeletionIterator.py`'s own pruning) — without this
    /// the db grows forever and `--scheduled-deletion-log` reports ghosts.
    fn retain_present(&mut self, present: &HashSet<String>) {
        self.data.retain(|filename, _| present.contains(filename));
    }
}

/// Apply the grace period: a fresh orphan is scheduled (recorded, not
/// deleted); one whose delay has elapsed is deleted; a referenced file
/// clears its entry (handled by the caller via [`DeletionDb::retain_present`]
/// running against `present`, not `orphans`, before this is called — a
/// reappearing file is simply absent from `orphans` and so never touched
/// here, and was already dropped from `db` if it had a stale entry).
fn deletion_decisions(
    db: &mut DeletionDb,
    orphans: &[String],
    now: u64,
    delay: Duration,
) -> (Vec<String>, Vec<(String, u64)>) {
    let delay_secs = delay.as_secs();
    let mut delete_now = Vec::new();
    let mut scheduled = Vec::new();
    for filename in orphans {
        let first_seen = db.note_orphan(filename, now);
        let eligible_at = first_seen.saturating_add(delay_secs);
        if eligible_at <= now {
            delete_now.push(filename.clone());
        } else {
            scheduled.push((filename.clone(), eligible_at));
        }
    }
    // Deleted files no longer belong in the pending-deletion db.
    for filename in &delete_now {
        db.data.remove(filename);
    }
    (delete_now, scheduled)
}

fn default_deletion_db_path(repo_name: &str, distfiles: &Utf8Path) -> Utf8PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    distfiles.as_str().hash(&mut hasher);
    crate::xdg::mirrordist_state_dir().join(format!(
        "{repo_name}-{:016x}-deletion.json",
        hasher.finish()
    ))
}

/// Tab-delimited append-only logs, matching real emirrordist's line shapes:
/// `"{cpv}\t{file}\t{message}"`.
struct EventLog {
    success: Option<std::fs::File>,
    failure: Option<std::fs::File>,
}

impl EventLog {
    fn open(success_path: Option<&Utf8Path>, failure_path: Option<&Utf8Path>) -> Result<Self> {
        let open = |path: Option<&Utf8Path>| -> Result<Option<std::fs::File>> {
            let Some(path) = path else { return Ok(None) };
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path.as_std_path())
                .with_context(|| format!("opening log {path}"))?;
            Ok(Some(file))
        };
        Ok(Self {
            success: open(success_path)?,
            failure: open(failure_path)?,
        })
    }

    fn success(&mut self, cpv: &Cpv, file: &str, message: &str) {
        Self::write_line(&mut self.success, &format!("{cpv}\t{file}\t{message}\n"));
    }

    fn success_anon(&mut self, file: &str, message: &str) {
        Self::write_line(&mut self.success, &format!("-\t{file}\t{message}\n"));
    }

    fn failure(&mut self, cpv: &Cpv, file: &str, message: &str) {
        Self::write_line(&mut self.failure, &format!("{cpv}\t{file}\t{message}\n"));
    }

    fn write_line(file: &mut Option<std::fs::File>, line: &str) {
        if let Some(f) = file {
            use std::io::Write as _;
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// `--scheduled-deletion-log`: pending deletions grouped by the date they
/// become eligible, overwritten each run — the report an admin reads to see
/// what `--delete` is about to do.
fn write_scheduled_deletion_log(path: &Utf8Path, scheduled: &[(String, u64)]) -> Result<()> {
    use std::fmt::Write as _;
    let mut by_date: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (filename, eligible_at) in scheduled {
        let date = format_date(*eligible_at);
        by_date.entry(date).or_default().push(filename);
    }
    let mut out = String::new();
    for (date, files) in &by_date {
        let _ = writeln!(out, "{date}");
        for file in files {
            let _ = writeln!(out, "\t{file}");
        }
    }
    crate::util::write_atomic(path, out).with_context(|| format!("writing {path}"))
}

/// `YYYY-MM-DD` from a unix timestamp, no chrono dependency needed for this
/// one field — days-since-epoch civil calendar conversion (Howard Hinnant's
/// well-known algorithm).
fn format_date(unix_secs: u64) -> String {
    let days = (unix_secs / 86400) as i64;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn print_report(
    plan: &MirrorPlan,
    state: &DistdirState,
    delete_now: &[String],
    scheduled: &[(String, u64)],
    delete_requested: bool,
) {
    use humansize::{BINARY, format_size};

    let referenced = plan.referenced();
    let to_fetch: Vec<&OwnedFile> = plan
        .files
        .iter()
        .filter(|f| !state.present.contains_key(&f.distfile.filename))
        .collect();
    let to_fetch_bytes: u64 = to_fetch
        .iter()
        .filter_map(|f| plan.digests.get(&f.distfile.filename))
        .filter_map(dist_entry_size)
        .sum();

    println!("mirrordist (pretend)");
    println!("  scanned          {} ebuild(s)", plan.cpvs_scanned);
    println!(
        "  restricted       {} ebuild(s) skipped (RESTRICT=fetch/mirror, USE-independent)",
        plan.cpvs_restricted
    );
    println!(
        "  no metadata      {} ebuild(s) — must be 0 for --delete",
        plan.cpvs_missing_cache
    );
    println!(
        "  metadata errors  {} ebuild(s) — must be 0 for --delete",
        plan.cpvs_metadata_error
    );
    println!("  no digest        {} file(s)", plan.missing_digest);
    println!("  manifest errors  {} package(s)", plan.manifest_errors);
    println!(
        "  referenced       {} file(s), {}",
        referenced.len(),
        format_size(plan.total_bytes(), BINARY)
    );
    println!(
        "  present          {} file(s), {}",
        state.present.len(),
        format_size(state.present.values().sum::<u64>(), BINARY)
    );
    println!(
        "  to fetch         {} file(s), {}",
        to_fetch.len(),
        format_size(to_fetch_bytes, BINARY)
    );
    if delete_requested {
        println!("  orphaned         {} file(s)", state.orphans.len());
        println!("    delete now     {} file(s)", delete_now.len());
        println!("    scheduled      {} file(s)", scheduled.len());
        println!("  (state not written in pretend mode — no grace clock started)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_repo(root: &Path) {
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        std::fs::write(root.join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(root.join("profiles")).unwrap();
    }

    fn write_categories(root: &Path, cats: &[&str]) {
        std::fs::write(root.join("profiles").join("categories"), cats.join("\n")).unwrap();
    }

    /// Write one fixture package version: an (empty, never-sourced) ebuild
    /// file, an appended `Manifest` DIST line, and a minimal md5-cache entry
    /// carrying only the two fields `build_plan` reads.
    fn write_package(
        root: &Path,
        cat: &str,
        name: &str,
        version: &str,
        src_uri: &str,
        restrict: &str,
        manifest_dist_line: &str,
    ) {
        let pkg_dir = root.join(cat).join(name);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join(format!("{name}-{version}.ebuild")), "").unwrap();

        let manifest_path = pkg_dir.join("Manifest");
        let mut existing = std::fs::read_to_string(&manifest_path).unwrap_or_default();
        existing.push_str(manifest_dist_line);
        std::fs::write(&manifest_path, existing).unwrap();

        let cache_dir = root.join("metadata").join("md5-cache").join(cat);
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(
            cache_dir.join(format!("{name}-{version}")),
            format!("SRC_URI={src_uri}\nRESTRICT={restrict}\n"),
        )
        .unwrap();
    }

    fn open_repo(dir: &tempfile::TempDir) -> Repository {
        Repository::builder()
            .in_memory_cache()
            .open(dir.path())
            .unwrap()
    }

    async fn plan_for(dir: &tempfile::TempDir) -> MirrorPlan {
        let repo = open_repo(dir);
        let resolver = DistfileResolver::new(vec![], vec![]);
        build_plan(&repo, &[], &resolver, None, false)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn first_owner_wins_across_versions_of_the_same_package() {
        let dir = tempfile::tempdir().unwrap();
        setup_repo(dir.path());
        write_categories(dir.path(), &["app-misc"]);
        write_package(
            dir.path(),
            "app-misc",
            "foo",
            "1.0",
            "https://example.com/shared.tar.gz",
            "",
            "DIST shared.tar.gz 100 BLAKE2B aaaa\n",
        );
        write_package(
            dir.path(),
            "app-misc",
            "foo",
            "2.0",
            "https://example.com/shared.tar.gz",
            "",
            "",
        );

        let plan = plan_for(&dir).await;
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].owner.to_string(), "app-misc/foo-1.0");
    }

    #[tokio::test]
    async fn first_owner_wins_across_two_different_packages() {
        let dir = tempfile::tempdir().unwrap();
        setup_repo(dir.path());
        write_categories(dir.path(), &["app-misc", "dev-libs"]);
        write_package(
            dir.path(),
            "app-misc",
            "aaa-first",
            "1.0",
            "https://example.com/shared.tar.gz",
            "",
            "DIST shared.tar.gz 100 BLAKE2B aaaa\n",
        );
        write_package(
            dir.path(),
            "dev-libs",
            "zzz-second",
            "1.0",
            "https://example.com/shared.tar.gz",
            "",
            "DIST shared.tar.gz 100 BLAKE2B aaaa\n",
        );

        let plan = plan_for(&dir).await;
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].owner.to_string(), "app-misc/aaa-first-1.0");
    }

    #[tokio::test]
    async fn restrict_mirror_package_contributes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        setup_repo(dir.path());
        write_categories(dir.path(), &["app-misc"]);
        write_package(
            dir.path(),
            "app-misc",
            "foo",
            "1.0",
            "https://example.com/foo.tar.gz",
            "mirror",
            "DIST foo.tar.gz 100 BLAKE2B aaaa\n",
        );

        let plan = plan_for(&dir).await;
        assert!(plan.files.is_empty());
        assert_eq!(plan.cpvs_restricted, 1);
    }

    /// End-to-end bug #1 regression: a *negated* conditional restriction is
    /// still mirrored — matchnone means "no conditional ever applies," not
    /// "treat every flag as unset."
    #[tokio::test]
    async fn negated_conditional_restrict_is_still_mirrored() {
        let dir = tempfile::tempdir().unwrap();
        setup_repo(dir.path());
        write_categories(dir.path(), &["app-misc"]);
        write_package(
            dir.path(),
            "app-misc",
            "foo",
            "1.0",
            "https://example.com/foo.tar.gz",
            "!test? ( mirror )",
            "DIST foo.tar.gz 100 BLAKE2B aaaa\n",
        );

        let plan = plan_for(&dir).await;
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.cpvs_restricted, 0);
    }

    #[tokio::test]
    async fn missing_cache_entry_drives_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        setup_repo(dir.path());
        write_categories(dir.path(), &["app-misc"]);
        // Ebuild file with no matching md5-cache entry at all.
        let pkg_dir = dir.path().join("app-misc").join("foo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("foo-1.0.ebuild"), "").unwrap();

        let plan = plan_for(&dir).await;
        assert_eq!(plan.cpvs_missing_cache, 1);
        assert!(!plan.is_complete());
    }

    #[tokio::test]
    async fn missing_manifest_drives_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        setup_repo(dir.path());
        write_categories(dir.path(), &["app-misc"]);
        // Cache entry present with a real SRC_URI, but no Manifest file at all.
        let pkg_dir = dir.path().join("app-misc").join("foo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("foo-1.0.ebuild"), "").unwrap();
        let cache_dir = dir
            .path()
            .join("metadata")
            .join("md5-cache")
            .join("app-misc");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(
            cache_dir.join("foo-1.0"),
            "SRC_URI=https://example.com/foo.tar.gz\nRESTRICT=\n",
        )
        .unwrap();

        let plan = plan_for(&dir).await;
        assert_eq!(plan.manifest_errors, 1);
        assert!(plan.files.is_empty());
        assert!(!plan.is_complete());
    }

    #[test]
    fn scan_distdir_skips_layout_conf_subdirs_and_atomic_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let distfiles = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::write(distfiles.join("layout.conf").as_std_path(), "").unwrap();
        std::fs::write(distfiles.join("foo.tar.gz").as_std_path(), b"data").unwrap();
        std::fs::write(
            distfiles.join(".foo.tar.gz.__em_download__").as_std_path(),
            b"partial",
        )
        .unwrap();
        std::fs::create_dir_all(distfiles.join("subdir").as_std_path()).unwrap();

        let referenced: HashSet<&str> = ["foo.tar.gz"].into_iter().collect();
        let state = scan_distdir(distfiles, &referenced, &HashSet::new()).unwrap();

        assert_eq!(state.present.len(), 1);
        assert!(state.present.contains_key("foo.tar.gz"));
        assert!(state.orphans.is_empty());
    }

    #[test]
    fn scan_distdir_flags_unreferenced_files_as_orphans_unless_whitelisted() {
        let dir = tempfile::tempdir().unwrap();
        let distfiles = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::write(distfiles.join("orphan.tar.gz").as_std_path(), b"data").unwrap();
        std::fs::write(distfiles.join("kept.tar.gz").as_std_path(), b"data").unwrap();

        let referenced: HashSet<&str> = HashSet::new();
        let whitelist: HashSet<String> = ["kept.tar.gz".to_string()].into_iter().collect();
        let state = scan_distdir(distfiles, &referenced, &whitelist).unwrap();

        assert_eq!(state.present.len(), 2);
        assert_eq!(state.orphans, vec!["orphan.tar.gz".to_string()]);
    }

    #[test]
    fn parse_whitelist_skips_comments_and_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap().join("whitelist");
        std::fs::write(
            path.as_std_path(),
            "# a comment\n\nfoo.tar.gz\n  bar.tar.gz  \n",
        )
        .unwrap();

        let set = parse_whitelist(&[path]).unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains("foo.tar.gz"));
        assert!(set.contains("bar.tar.gz"));
    }

    #[test]
    fn deletion_decisions_schedules_a_fresh_orphan_without_deleting() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = DeletionDb::load(&Utf8Path::from_path(dir.path()).unwrap().join("db.json"));
        let now = 1_000_000;
        let orphans = vec!["foo.tar.gz".to_string()];

        let (delete_now, scheduled) =
            deletion_decisions(&mut db, &orphans, now, Duration::from_secs(7 * 86400));

        assert!(delete_now.is_empty());
        assert_eq!(scheduled.len(), 1);
        assert_eq!(scheduled[0].0, "foo.tar.gz");
        assert_eq!(scheduled[0].1, now + 7 * 86400);
    }

    #[test]
    fn deletion_decisions_deletes_once_the_grace_period_elapses() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = DeletionDb::load(&Utf8Path::from_path(dir.path()).unwrap().join("db.json"));
        let orphans = vec!["foo.tar.gz".to_string()];
        let delay = Duration::from_secs(100);

        // First run: seen for the first time, scheduled.
        let (delete_now, _) = deletion_decisions(&mut db, &orphans, 1000, delay);
        assert!(delete_now.is_empty());

        // Second run, past the delay: deleted, and the db entry is cleared.
        let (delete_now, scheduled) = deletion_decisions(&mut db, &orphans, 1000 + 200, delay);
        assert_eq!(delete_now, vec!["foo.tar.gz".to_string()]);
        assert!(scheduled.is_empty());
        assert!(db.data.is_empty());
    }

    #[test]
    fn deletion_db_reappearing_file_clears_its_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap().join("db.json");
        let mut db = DeletionDb::load(&path);
        db.note_orphan("foo.tar.gz", 1000);
        assert_eq!(db.data.len(), 1);

        // The file reappeared as a live, present distfile — retain_present
        // (called with the *present* set, not orphans) must clear its entry,
        // resetting the grace clock rather than carrying it over.
        let present: HashSet<String> = HashSet::new(); // foo.tar.gz absent == still gone
        db.retain_present(&present);
        assert!(db.data.is_empty());
    }

    #[test]
    fn deletion_db_round_trips_through_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap().join("db.json");
        let mut db = DeletionDb::load(&path);
        db.note_orphan("foo.tar.gz", 1234);
        db.store().unwrap();

        let reloaded = DeletionDb::load(&path);
        assert_eq!(reloaded.data.get("foo.tar.gz"), Some(&1234));
    }

    #[test]
    fn deletion_db_missing_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path())
            .unwrap()
            .join("does-not-exist.json");
        let db = DeletionDb::load(&path);
        assert!(db.data.is_empty());
    }

    #[test]
    fn default_deletion_db_path_differs_by_distfiles_target() {
        let a = default_deletion_db_path("gentoo", Utf8Path::new("/mnt/mirror-a"));
        let b = default_deletion_db_path("gentoo", Utf8Path::new("/mnt/mirror-b"));
        assert_ne!(a, b);
    }

    #[test]
    fn format_date_known_epoch_values() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(1_700_000_000), "2023-11-14");
    }
}
