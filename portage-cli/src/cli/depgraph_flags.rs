//! Shared depgraph-related flags for use in both the main CLI and query depgraph subcommand.

use clap::Args;

/// Flags related to dependency graph resolution that can be used by multiple commands.
#[derive(Args, Debug, Clone, Default)]
pub struct DepgraphFlags {
    /// Re-examine transitive dependencies. With `--update` (`-uD`), upgrades
    /// installed packages in the depgraph to the newest accepted in-slot
    /// version (emerge `-uD`). Alone, still bumps `:*` any-slot deps to the
    /// newest slot rather than keeping a satisfying installed slot.
    #[arg(short = 'D', long)]
    pub deep: bool,

    /// Re-evaluate USE flags for all packages. Forces re-examination of USE
    /// state for installed packages.
    #[arg(short = 'N', long)]
    pub newuse: bool,
}
