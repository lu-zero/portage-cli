//! Parallel ebuild sourcing
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

/// Result of sourcing an ebuild
///
/// Bundles the extracted metadata with the resolved file paths of every
/// eclass that was sourced (in inheritance order). The paths come from the
/// `inherit` builtin's own resolution against the live eclass search dirs,
/// so they correctly reflect eclasses pulled from master repositories rather
/// than the local repo — which a name-based lookup at write time cannot.
#[derive(Debug, Clone)]
pub struct SourcedEbuild {
    /// Parsed metadata variables (DEPEND, IUSE, EAPI, …)
    pub metadata: EbuildMetadata,
    /// Eclasses sourced for this ebuild, paired `(name, file path)`
    pub eclasses: Vec<(String, Utf8PathBuf)>,
    /// MD5 digest of the ebuild content that was sourced
    pub ebuild_md5: String,
}

/// One finished source attempt, in completion order
pub type SourceItem = (Ebuild, Result<SourcedEbuild>);

type AstCache =
    Arc<papaya::HashMap<String, std::result::Result<brush_parser::ast::Program, String>>>;

/// Shared eclass AST cache
///
/// Create once per run and pass the same instance to every sourcing call.
/// The internal brush AST types are not part of the public API.
#[derive(Clone, Default)]
pub struct SourceContext(pub(crate) AstCache);

impl SourceContext {
    /// Create a fresh (empty) sourcing context
    pub fn new() -> Self {
        Self(Arc::new(papaya::HashMap::new()))
    }
}

/// Options for sourcing operations
#[derive(Debug, Clone, Default)]
pub struct SourceOpts {
    /// Number of parallel workers. `None` uses a conservative default capped at 16.
    /// Values below one are treated as one worker.
    pub jobs: Option<usize>,
    /// Deduplicate top-level dep tokens before returning metadata
    pub dedup: bool,
}

/// Source `ebuilds` in parallel; return a stream of outcomes in completion order
///
/// Must be called from a Tokio runtime. Drain the receiver until it
/// disconnects. Per-ebuild failures are `Err` items; the pool keeps going.
/// A worker-pool failure is logged before the receiver disconnects.
///
/// Internally this is the classic feed-then-join worker pool (same shape that
/// used an `on_result` callback): the result stream is just the hand-off to
/// the caller, not a second pipeline stage that re-parks workers.
pub fn source_parallel(
    repo: &Repository,
    ebuilds: Vec<Ebuild>,
    opts: &SourceOpts,
    ctx: &SourceContext,
) -> flume::Receiver<SourceItem> {
    let (out_tx, out_rx) = flume::unbounded::<SourceItem>();
    let repo = repo.clone();
    let opts = opts.clone();
    let ctx = ctx.clone();

    tokio::spawn(async move {
        if let Err(e) = source_parallel_join(&repo, ebuilds, &opts, &ctx, move |ebuild, result| {
            // Unbounded + sync send: never park a worker on the hand-off.
            let _ = out_tx.send((ebuild, result));
        })
        .await
        {
            tracing::error!("source worker pool failed: {e}");
        }
    });

    out_rx
}

/// Classic worker pool: feed work, run `on_result` on the worker task, join
///
/// Kept as the shared engine for [`source_parallel`] and [`crate::cache::regen_cache`]
/// so the stream API does not change the scheduling shape.
pub(crate) async fn source_parallel_join<F>(
    repo: &Repository,
    ebuilds: Vec<Ebuild>,
    opts: &SourceOpts,
    ctx: &SourceContext,
    on_result: F,
) -> Result<()>
where
    F: Fn(Ebuild, Result<SourcedEbuild>) + Send + Sync + 'static,
{
    let jobs = crate::resolve_worker_count(opts.jobs);
    let dedup = opts.dedup;
    let repo = Arc::new(repo.clone());
    let ctx = ctx.clone();
    let on_result = Arc::new(on_result);

    let (work_tx, work_rx) = flume::bounded::<Ebuild>(jobs * 2);

    let mut handles = Vec::with_capacity(jobs);
    for _ in 0..jobs {
        let work_rx = work_rx.clone();
        let repo = Arc::clone(&repo);
        let ctx = ctx.clone();
        let on_result = Arc::clone(&on_result);

        handles.push(tokio::spawn(async move {
            let master_refs: Vec<&Repository> = repo.masters().iter().collect();
            let mut shell = match repo.shell_with_masters_and_cache(&master_refs, &ctx).await {
                Ok(shell) => shell,
                Err(_) => {
                    while let Ok(ebuild) = work_rx.recv_async().await {
                        let result = source_one(&repo, &master_refs, &ebuild, &ctx, dedup).await;
                        on_result(ebuild, result);
                    }
                    return;
                }
            };
            while let Ok(ebuild) = work_rx.recv_async().await {
                let result = source_with_shell(&mut shell, &ebuild, dedup).await;
                on_result(ebuild, result);
            }
        }));
    }
    drop(work_rx);

    // Feed on this task (interleaved with workers via send_async backpressure),
    // then join — same structure as the pre-stream implementation.
    for ebuild in ebuilds {
        let _ = work_tx.send_async(ebuild).await;
    }
    drop(work_tx);

    let mut worker_error = None;
    for h in handles {
        if let Err(e) = h.await {
            worker_error.get_or_insert_with(|| crate::Error::SourceWorker(e.to_string()));
        }
    }
    worker_error.map_or(Ok(()), Err)
}

/// Source a single ebuild and return its metadata
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
    source_with_shell(&mut shell, ebuild, dedup).await
}

async fn source_with_shell(
    shell: &mut crate::EbuildShell,
    ebuild: &Ebuild,
    dedup: bool,
) -> Result<SourcedEbuild> {
    let mut sourced = shell.source_ebuild(ebuild).await?;
    if dedup {
        sourced.metadata = sourced.metadata.dedup();
    }
    Ok(sourced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{RegenOpts, RegenWriteTarget, regen_cache};
    use camino::{Utf8Path, Utf8PathBuf};

    fn repo_with_eclass(eclass: &str) -> (tempfile::TempDir, Repository, Vec<Ebuild>) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::write(root.join("profiles/repo_name"), "test\n").unwrap();
        std::fs::write(root.join("profiles/categories"), "cat\n").unwrap();
        std::fs::create_dir_all(root.join("eclass")).unwrap();
        std::fs::write(root.join("eclass/broken.eclass"), eclass).unwrap();

        let pkg = root.join("cat/pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        let ebuild_path = pkg.join("pkg-1.0.ebuild");
        std::fs::write(
            &ebuild_path,
            "EAPI=8\ninherit broken\nDESCRIPTION=\"t\"\nSLOT=\"0\"\n",
        )
        .unwrap();
        let repo = Repository::builder().in_memory_cache().open(root).unwrap();
        let ebuild = Ebuild::from_path(Utf8Path::from_path(&ebuild_path).unwrap()).unwrap();
        (tmp, repo, vec![ebuild])
    }

    #[tokio::test]
    async fn malformed_eclass_is_reported_without_publishing_missing_output() {
        let (_tmp, repo, ebuilds) = repo_with_eclass("if true; then\n");
        let output_root = tempfile::tempdir().unwrap();
        let output = output_root.path().join("out");
        let (tx, _rx) = flume::unbounded();
        let opts = RegenOpts {
            source: SourceOpts {
                jobs: Some(1),
                dedup: false,
            },
            write: RegenWriteTarget::Dir(output.clone()),
        };

        let stats = regen_cache(&repo, ebuilds, &opts, tx).await.unwrap();

        assert_eq!(stats.errors, 1);
        assert!(!output.exists());
        assert!(!output.join("cat/pkg-1.0").exists());
    }

    #[tokio::test]
    async fn zero_jobs_still_writes_a_sourced_entry() {
        let (_tmp, repo, ebuilds) = repo_with_eclass("");
        let output = tempfile::tempdir().unwrap();
        let (tx, _rx) = flume::unbounded();
        let opts = RegenOpts {
            source: SourceOpts {
                jobs: Some(0),
                dedup: false,
            },
            write: RegenWriteTarget::Dir(output.path().to_owned()),
        };

        let stats = regen_cache(&repo, ebuilds, &opts, tx).await.unwrap();

        assert_eq!(stats.errors, 0);
        assert!(output.path().join("cat/pkg-1.0").is_file());
    }

    #[tokio::test]
    async fn source_worker_reuse_keeps_each_ebuild_hermetic() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::write(root.join("profiles/repo_name"), "test\n").unwrap();
        std::fs::write(root.join("profiles/categories"), "cat\n").unwrap();
        let mut ebuilds = Vec::new();
        for (name, description) in [("first", "one"), ("second", "two")] {
            let dir = root.join("cat").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{name}-1.0.ebuild"));
            std::fs::write(
                &path,
                format!("EAPI=8\nDESCRIPTION=\"{description}\"\nSLOT=0\n"),
            )
            .unwrap();
            ebuilds.push(Ebuild::from_path(Utf8Path::from_path(&path).unwrap()).unwrap());
        }
        let repo = Repository::builder().in_memory_cache().open(root).unwrap();
        let receiver = source_parallel(
            &repo,
            ebuilds,
            &SourceOpts {
                jobs: Some(1),
                dedup: false,
            },
            &SourceContext::new(),
        );
        let mut descriptions = Vec::new();
        while let Ok((_, result)) = receiver.recv_async().await {
            descriptions.push(result.unwrap().metadata.description);
        }
        descriptions.sort();
        assert_eq!(descriptions, ["one", "two"]);
    }

    #[tokio::test]
    async fn sourced_ebuild_keeps_script_path_and_content_digest() {
        let (_tmp, repo, ebuilds) = repo_with_eclass("");
        let ebuild = &ebuilds[0];
        let path = ebuild.path().to_owned();
        let content = "EAPI=8\nDESCRIPTION=\"${BASH_SOURCE[0]}\"\nSLOT=0\n";
        std::fs::write(&path, content).unwrap();
        let expected_md5 = format!("{:x}", md5::compute(content.as_bytes()));
        let masters = repo.masters();

        let sourced = source_single(&repo, masters, ebuild, &SourceContext::new())
            .await
            .unwrap();

        assert_eq!(sourced.metadata.description, path.as_str());
        assert_eq!(sourced.ebuild_md5, expected_md5);
    }

    #[tokio::test]
    async fn successful_repository_regen_reconciles_gap_index() {
        let root = tempfile::tempdir().unwrap();
        let cache_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("metadata")).unwrap();
        std::fs::write(root.path().join("metadata/layout.conf"), "masters =\n").unwrap();
        std::fs::create_dir_all(root.path().join("profiles")).unwrap();
        std::fs::write(root.path().join("profiles/repo_name"), "test\n").unwrap();
        std::fs::write(root.path().join("profiles/categories"), "cat\n").unwrap();
        std::fs::write(root.path().join("metadata/timestamp.chk"), "1\n").unwrap();
        let pkg_dir = root.path().join("cat/pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let ebuild_path = pkg_dir.join("pkg-1.0.ebuild");
        std::fs::write(&ebuild_path, "EAPI=8\nDESCRIPTION=\"base\"\nSLOT=0\n").unwrap();
        let ebuild = Ebuild::from_path(Utf8Path::from_path(&ebuild_path).unwrap()).unwrap();
        let cpv = ebuild.cpv().clone();
        let repo = Repository::builder()
            .user_cache_root(Utf8PathBuf::from_path_buf(cache_root.path().to_owned()).unwrap())
            .open(root.path())
            .unwrap();
        let stamp = repo.sync_stamp().unwrap();
        let sidecar = repo.sidecar_path("gap-index").unwrap();
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(&sidecar, format!("{stamp}\n{cpv}\n")).unwrap();
        let (tx, _rx) = flume::unbounded();
        let opts = RegenOpts {
            source: SourceOpts {
                jobs: Some(1),
                dedup: false,
            },
            write: RegenWriteTarget::Repository,
        };

        let stats = regen_cache(&repo, vec![ebuild], &opts, tx).await.unwrap();

        assert_eq!(stats.errors, 0);
        assert!(repo.cache_entry(&cpv).unwrap().is_some());
        assert!(
            !std::fs::read_to_string(sidecar)
                .unwrap()
                .contains(&cpv.to_string())
        );
    }

    #[tokio::test]
    async fn source_worker_panics_are_reported() {
        let (_tmp, repo, ebuilds) = repo_with_eclass("");
        let result = source_parallel_join(
            &repo,
            ebuilds,
            &SourceOpts {
                jobs: Some(1),
                dedup: false,
            },
            &SourceContext::new(),
            |_, _| panic!("test worker panic"),
        )
        .await;

        assert!(matches!(result, Err(crate::Error::SourceWorker(_))));
    }
}
