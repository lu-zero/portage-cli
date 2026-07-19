//! Structured snapshot of an ebuild's shell environment after `source_ebuild`.
//!
//! [`EbuildEnv`] collects the metadata variables that are stable across all
//! build phases (SLOT, IUSE, DEPEND, …).  It is populated by
//! [`EbuildShell::collect_env`] and can be used independently of the shell
//! once sourcing is complete.

/// Metadata exported by an ebuild's shell environment.
///
/// All fields are the values of the corresponding Portage variables as they
/// exist after the ebuild (and all inherited eclasses) have been sourced.
/// Space-separated list variables are pre-split into `Vec<String>`.
#[derive(Debug, Clone, Default)]
pub struct EbuildEnv {
    /// EAPI version string (e.g. `"8"`).
    pub eapi: String,
    /// Full SLOT value (e.g. `"0"` or `"0/5.1"`).
    pub slot: String,
    /// IUSE as declared by the ebuild (may include `+`/`-` defaults).
    pub iuse: Vec<String>,
    /// USE flags that were active when the ebuild was sourced.
    pub use_flags: Vec<String>,
    /// KEYWORDS (e.g. `["amd64", "~arm64"]`).
    pub keywords: Vec<String>,
    /// Single-line package description.
    pub description: String,
    /// Homepage URL(s), or `None` if unset.
    pub homepage: Option<String>,
    /// License expression, or `None` if unset.
    pub license: Option<String>,
    /// RESTRICT value, or `None` if unset.
    pub restrict: Option<String>,
    /// PROPERTIES value, or `None` if unset.
    pub properties: Option<String>,
    /// DEPEND atom string, or `None` if unset.
    pub depend: Option<String>,
    /// RDEPEND atom string, or `None` if unset.
    pub rdepend: Option<String>,
    /// BDEPEND atom string, or `None` if unset.
    pub bdepend: Option<String>,
    /// PDEPEND atom string, or `None` if unset.
    pub pdepend: Option<String>,
    /// IDEPEND atom string, or `None` if unset.
    pub idepend: Option<String>,
    /// Phase functions defined by the ebuild (e.g. `["configure", "install"]`).
    pub defined_phases: Vec<String>,
    /// Repository name the ebuild was sourced from, or `None`.
    pub repository: Option<String>,
    /// Eclasses inherited by the ebuild (INHERITED), in inherit order.
    pub inherited: Vec<String>,
    /// FEATURES active for the build, or `None`.
    pub features: Option<String>,
    /// Host triple the package targets (CHOST), or `None`.
    pub chost: Option<String>,
    /// Build triple (CBUILD), or `None`.
    pub cbuild: Option<String>,
    /// C compiler flags (CFLAGS), or `None`.
    pub cflags: Option<String>,
    /// C++ compiler flags (CXXFLAGS), or `None`.
    pub cxxflags: Option<String>,
    /// Linker flags (LDFLAGS), or `None`.
    pub ldflags: Option<String>,
    /// Rust compiler flags (RUSTFLAGS), or `None`.
    pub rustflags: Option<String>,
}

impl EbuildEnv {
    /// The main slot (the part before `/`, e.g. `"0"` from `"0/5.1"`).
    pub fn slot_main(&self) -> &str {
        self.slot
            .split_once('/')
            .map(|(s, _)| s)
            .unwrap_or(&self.slot)
    }
}
