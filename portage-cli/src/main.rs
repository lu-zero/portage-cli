#[cfg(all(feature = "mimalloc", not(feature = "dhat-heap")))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use clap::{CommandFactory, Parser};
use portage_cli::cli;

/// Parse argv, making the word `emerge` optional (`em --root R cat/pkg` ==
/// `em emerge --root R cat/pkg`) since `Topology`/`MergeFlags`/etc. now live
/// solely on `EmergeArgs`. clap has no native "default subcommand": try the
/// real argv first; if no real subcommand matched, retry with `emerge`
/// spliced in after the program name.
///
/// The `ignore_errors` probe (not the real parse) decides whether to retry:
/// this keeps `em crossdev --bogus-flag` reporting an error about
/// `crossdev`, not a confusing one about `emerge`.
fn parse_cli() -> cli::Cli {
    let raw: Vec<std::ffi::OsString> = std::env::args_os().collect();
    match cli::Cli::try_parse_from(&raw) {
        Ok(cli) => cli,
        Err(err) => {
            let lenient = cli::Cli::command().ignore_errors(true);
            let subcommand_seen = lenient
                .try_get_matches_from(&raw)
                .ok()
                .and_then(|m| m.subcommand_name().map(str::to_owned));
            if subcommand_seen.is_some() {
                err.exit();
            }
            let mut injected = Vec::with_capacity(raw.len() + 1);
            injected.push(raw[0].clone());
            injected.push("emerge".into());
            injected.extend(raw.into_iter().skip(1));
            cli::Cli::parse_from(injected)
        }
    }
}

fn main() {
    // Investigation-only: `cargo build --release --features dhat-heap` writes
    // dhat-heap.json on exit (see the Cargo.toml feature doc comment).
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();

    // Must be the first thing in main: on a fakeroost/pseudoroot supervisor
    // re-exec these run the session and exit; on a normal launch they are
    // no-ops. Kept ahead of the tokio runtime so the supervisor never spins
    // one up.
    #[cfg(all(feature = "fakeroost", target_os = "linux"))]
    fakeroost::init();
    #[cfg(all(feature = "pseudoroot", any(target_os = "linux", target_os = "macos")))]
    pseudoroot::init();

    // Portage's ebuild.sh sets `umask 022` before running any phase; mirror it
    // so file and directory modes under ${D} and the build tree match a real
    // merge regardless of the invoking shell's umask. The install helpers
    // additionally chmod each created image dir to 0755 (see mkdir_p_mode), so
    // they stay correct even under a tighter ebuild-local umask; this call
    // covers everything else (ebuild-written files, distfiles, the prefix
    // layout, cache regen).
    rustix::process::umask(rustix::fs::Mode::from_bits_truncate(0o022));

    let cli = parse_cli();
    cli.color.write_global();
    // Tracing subscriber: libraries emit, this decides where (stderr now, the
    // activity bus once the bus layer is stacked on). `parallel` drops info
    // noise so `-j>1` doesn't interleave per-package status.
    portage_cli::diag::init(
        cli.quiet,
        cli.verbose,
        cli.merge_flags().jobs.unwrap_or(1) > 1,
    );

    // An unprivileged build re-execs once under a fake root so chown/setuid
    // succeed; the wrapped child returns here with `EM_PRIVILEGE_ACTIVE` set and
    // proceeds normally. Nothing to wrap ⇒ proceed in-process.
    if let Some(code) = portage_cli::privilege::maybe_supervise(&cli) {
        std::process::exit(code);
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            portage_cli::print_fatal_error_display(format!(
                "failed to build the tokio runtime: {e}"
            ));
            std::process::exit(1);
        }
    };
    let result = runtime.block_on(portage_cli::run(&cli));

    if let Err(e) = result {
        // `process::exit` does not flush buffered stdout (the resolver's plan /
        // change block); do it explicitly so nothing printed is lost.
        use std::io::Write;
        std::io::stdout().flush().ok();
        // A "changes needed" resolve, or an all-atoms-failed run, exits 1
        // quietly — the real explanation (change block / per-atom warnings)
        // is already printed, so a final generic `!!!` line would be noise,
        // not new information.
        if e.downcast_ref::<portage_cli::ConfigChangesNeeded>()
            .is_none()
            && e.downcast_ref::<portage_cli::NoValidAtoms>().is_none()
        {
            portage_cli::print_fatal_error(&e);
        }
        std::process::exit(1);
    }
}
