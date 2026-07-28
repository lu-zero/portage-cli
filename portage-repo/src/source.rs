//! Parallel ebuild sourcing.
//!
//! - [`source_parallel`] — worker pool; results as a stream (completion order)
//! - [`source_single`] — source one ebuild (emerge cache-miss path)
//!
//! Both share a [`SourceContext`] that holds the eclass AST cache. Pass the
//! same instance across calls within a single run to maximise cache hits.
//! No disk I/O is performed here; writing cache files is `crate::cache`.

use std::sync::Arc;

use camino::Utf8PathBuf;
use portage_metadata::EbuildMetadata;

use crate::{Ebuild, Repository, Result};

/// Result of sourcing an ebuild.
///
/// Bundles the extracted metadata with the resolved file paths of every
/// eclass that was sourced (in inheritance order). The paths come from the
/// `inherit` builtin's own resolution against the live eclass search dirs,
/// so they correctly reflect eclasses pulled from master repositories rather
/// than the local repo — which a name-based lookup at write time cannot.
#[derive(Debug, Clone)]
pub struct SourcedEbuild {
    /// Parsed metadata variables (DEPEND, IUSE, EAPI, …).
    pub metadata: EbuildMetadata,
    /// Eclasses sourced for this ebuild, paired `(name, file path)`.
    pub eclasses: Vec<(String, Utf8PathBuf)>,
}

/// One finished source attempt, in completion order.
pub type SourceItem = (Ebuild, Result<SourcedEbuild>);

type AstCache = Arc<papaya::HashMap<String, brush_parser::ast::Program>>;

/// Shared eclass AST cache.
///
/// Create once per run and pass the same instance to every sourcing call.
/// The internal brush AST types are not part of the public API.
#[derive(Clone, Default)]
pub struct SourceContext(pub(crate) AstCache);

impl SourceContext {
    /// Create a fresh (empty) sourcing context.
    pub fn new() -> Self {
        Self(Arc::new(papaya::HashMap::new()))
    }
}

/// Options for sourcing operations.
#[derive(Debug, Clone, Default)]
pub struct SourceOpts {
    /// Number of parallel workers. `None` uses [`std::thread::available_parallelism`].
    pub jobs: Option<usize>,
    /// Deduplicate top-level dep tokens before returning metadata.
    pub dedup: bool,
}

/// Source `ebuilds` in parallel; return a stream of outcomes in completion order.
///
/// Must be called from a Tokio runtime: workers and the work feeder are
/// spawned on it. Drain the receiver until it disconnects (all items done,
/// or the consumer dropped early — workers then stop sending).
///
/// Per-ebuild failures are `Err` items on the stream; the pool keeps going.
pub fn source_parallel(
    repo: &Repository,
    masters: &[Repository],
    ebuilds: Vec<Ebuild>,
    opts: &SourceOpts,
    ctx: &SourceContext,
) -> flume::Receiver<SourceItem> {
    let jobs = opts.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });
    let dedup = opts.dedup;
    let repo = Arc::new(repo.clone());
    let masters: Arc<Vec<Repository>> = Arc::new(masters.to_vec());
    let ctx = ctx.clone();

    let (work_tx, work_rx) = flume::bounded::<Ebuild>(jobs * 2);
    let (out_tx, out_rx) = flume::bounded::<SourceItem>(jobs * 2);

    for _ in 0..jobs {
        let work_rx = work_rx.clone();
        let out_tx = out_tx.clone();
        let repo = Arc::clone(&repo);
        let masters = Arc::clone(&masters);
        let ctx = ctx.clone();

        tokio::spawn(async move {
            let master_refs: Vec<&Repository> = masters.iter().collect();
            while let Ok(ebuild) = work_rx.recv_async().await {
                let result = source_one(&repo, &master_refs, &ebuild, &ctx, dedup).await;
                if out_tx.send_async((ebuild, result)).await.is_err() {
                    break; // consumer gone
                }
            }
        });
    }
    drop(work_rx);
    drop(out_tx); // workers hold the remaining senders

    tokio::spawn(async move {
        for ebuild in ebuilds {
            if work_tx.send_async(ebuild).await.is_err() {
                break;
            }
        }
        // work_tx drop → workers exit → out senders drop → out_rx ends
    });

    out_rx
}

/// Source a single ebuild and return its metadata.
///
/// Use this for the emerge cache-miss path where spawning a full worker pool
/// would be wasteful. Reuse the same `ctx` across multiple calls in one run.
pub async fn source_single(
    repo: &Repository,
    masters: &[Repository],
    ebuild: &Ebuild,
    ctx: &SourceContext,
) -> Result<SourcedEbuild> {
    let master_refs: Vec<&Repository> = masters.iter().collect();
    source_one(repo, &master_refs, ebuild, ctx, false).await
}

pub(crate) async fn source_one(
    repo: &Repository,
    masters: &[&Repository],
    ebuild: &Ebuild,
    ctx: &SourceContext,
    dedup: bool,
) -> Result<SourcedEbuild> {
    let mut shell = repo.shell_with_masters_and_cache(masters, ctx).await?;
    let mut sourced = shell.source_ebuild(ebuild).await?;
    if dedup {
        sourced.metadata = sourced.metadata.dedup();
    }
    Ok(sourced)
}
