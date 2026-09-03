//! Shared depgraph-related flags for use in both the main CLI and query depgraph subcommand

/// Flags related to dependency graph resolution that can be used by multiple commands
#[derive(usage::Args, Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DepgraphFlags {
    /// Re-examine transitive dependencies
    ///
    /// With `--update` (`-uD`), upgrades installed packages in the depgraph to the newest
    /// accepted in-slot version (emerge `-uD`). Alone, still bumps `:*` any-slot deps to the
    /// newest slot rather than keeping a satisfying installed slot.
    #[usage(short = 'D', long)]
    pub deep: bool,

    /// Reinstall installed packages when their planned USE or IUSE differs from the VDB (emerge
    /// `--newuse`)
    ///
    /// Applies to packages that appear in the depgraph; pairs with `--deep` for a full-tree USE
    /// recheck.
    #[usage(short = 'N', long)]
    pub newuse: bool,

    /// Like `--newuse`, but only rebuild when an *enabled* USE flag changed
    /// among flags present in both installed and current IUSE (ignore pure
    /// IUSE add/drop). Emerge's `--changed-use` / `-U`.
    #[usage(short = 'U', long = "changed-use")]
    pub changed_use: bool,
}

impl DepgraphFlags {
    /// Overlay a dual-mounted copy: bools OR.
    pub(crate) fn overlay(&self, applet: &Self) -> Self {
        Self {
            deep: self.deep || applet.deep,
            newuse: self.newuse || applet.newuse,
            changed_use: self.changed_use || applet.changed_use,
        }
    }
}
