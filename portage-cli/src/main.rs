#[cfg(all(feature = "mimalloc", not(feature = "dhat-heap")))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use clap::Parser;
use portage_cli::cli;

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

    let cli = cli::Cli::parse();
    cli.color.write_global();
    // Tracing subscriber: libraries emit, this decides where (stderr now, the
    // activity bus once the bus layer is stacked on). `parallel` drops info
    // noise so `-j>1` doesn't interleave per-package status.
    portage_cli::diag::init(
        cli.quiet,
        cli.verbose,
        cli.merge_flags.jobs.unwrap_or(1) > 1,
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
        // A "changes needed" resolve exits 1 quietly — the change block is already
        // printed (and the staged driver prints its step header), so an `error:`
        // line would be noise. Everything else gets the message.
        if e.downcast_ref::<portage_cli::ConfigChangesNeeded>()
            .is_none()
        {
            portage_cli::print_fatal_error(&e);
        }
        std::process::exit(1);
    }
}
