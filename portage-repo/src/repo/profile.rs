use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::{Path, PathBuf};

use gentoo_core::Arch;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpv, Dep};
use portage_metadata::Eapi;

use super::layout::LayoutConf;
use super::util;
use crate::error::{Error, Result};

/// Stability status of a profile
///
/// PMS allows repositories to define arbitrary status values beyond the
/// well-known `stable`, `dev`, and `exp`.
///
/// See [PMS 5](https://projects.gentoo.org/pms/9/pms.html#profiles).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProfileStatus {
    /// Stable profile
    Stable,
    /// Development profile
    Dev,
    /// Experimental profile
    Exp,
    /// A repository-defined status value not covered by the well-known variants
    Other(String),
}

impl ProfileStatus {
    fn parse(s: &str) -> Self {
        match s {
            "stable" => ProfileStatus::Stable,
            "dev" => ProfileStatus::Dev,
            "exp" => ProfileStatus::Exp,
            other => ProfileStatus::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for ProfileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileStatus::Stable => f.write_str("stable"),
            ProfileStatus::Dev => f.write_str("dev"),
            ProfileStatus::Exp => f.write_str("exp"),
            ProfileStatus::Other(s) => f.write_str(s),
        }
    }
}

/// A profile entry from `profiles/profiles.desc`
///
/// See [PMS 5](https://projects.gentoo.org/pms/9/pms.html#profiles).
#[derive(Debug, Clone)]
pub struct ProfileDesc {
    /// Typed architecture keyword
    arch: Arch,
    /// Path relative to `profiles/` (e.g. `default/linux/amd64/23.0`)
    path: String,
    /// Stability status
    status: ProfileStatus,
}

impl ProfileDesc {
    /// Parse a single line from `profiles.desc`
    ///
    /// Format: `arch path status`
    pub fn parse(line: &str) -> Result<Self> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            return Err(Error::InvalidProfile(format!(
                "expected 'arch path status', got: {line}"
            )));
        }
        Ok(ProfileDesc {
            arch: Arch::intern(parts[0]),
            path: parts[1].to_string(),
            status: ProfileStatus::parse(parts[2]),
        })
    }

    /// Typed architecture keyword
    pub fn arch(&self) -> &Arch {
        &self.arch
    }

    /// Path relative to `profiles/` (e.g. `default/linux/amd64/23.0`)
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Stability status
    pub fn status(&self) -> &ProfileStatus {
        &self.status
    }
}

/// A profile directory
///
/// Profiles contain stacked configuration files that control default
/// USE flags, package masking, keywords, and more.
///
/// See [PMS 5 — Profiles](https://projects.gentoo.org/pms/9/pms.html#profiles).
#[derive(Debug, Clone)]
pub struct Profile {
    path: PathBuf,
    eapi: Eapi,
    /// Effective `profile-formats` for the repo this profile belongs to (from
    /// the enclosing repo's `metadata/layout.conf`), used to gate `@profile`-
    /// set contributions — only profiles whose `profile_formats` contains
    /// `profile-set` contribute their plain `packages` lines to `@profile`.
    ///
    /// Resolved at open time by [`resolve_profile_formats`] (walking up to
    /// the nearest enclosing repo, matching portage's longest-path-wins
    /// rule). Defaults to empty when no enclosing repo is found —
    /// equivalent to portage's `portage-1`/`portage-1-compat` default.
    ///
    /// The site-local user profile (`/etc/portage/profile`) is overridden
    /// to `["profile-bashrcs", "profile-set"]` by
    /// [`ProfileStack::with_user_profile`], mirroring portage's own
    /// `LocationsManager.load_profiles` hardcoded value.
    profile_formats: Vec<String>,
}

/// One entry from a profile `packages` file (PMS 5.2.6)
///
/// `*cat/pkg` is a system-package add; `cat/pkg` an advisory (`@profile`) add;
/// `-cat/pkg` removes a prior add across the stack; `-*` clears all accumulated.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PackageEntry {
    System(Dep),
    Plain(Dep),
    Remove(Dep),
    Clear,
}

impl Profile {
    /// Open a profile at the given directory path
    pub fn open(path: PathBuf) -> Result<Self> {
        let eapi_str = util::read_single_line(path.join("eapi"))?;
        let eapi = match eapi_str {
            Some(s) => s
                .parse::<Eapi>()
                .map_err(|e| Error::InvalidProfile(format!("bad EAPI: {e}")))?,
            None => Eapi::Zero,
        };
        let profile_formats = resolve_profile_formats(&path);
        Ok(Profile {
            path,
            eapi,
            profile_formats,
        })
    }

    /// Absolute path to the profile directory
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The EAPI declared by this profile (from the `eapi` file)
    pub fn eapi(&self) -> Eapi {
        self.eapi
    }

    /// The effective `profile-formats` of the repo this profile belongs to
    ///
    /// Used to gate `@profile`-set membership (see
    /// [`ProfileStack::profile_set`]). Empty for a profile with no enclosing
    /// repo (no `profile-set`); `["profile-bashrcs", "profile-set"]` for the
    /// site-local user profile.
    pub fn profile_formats(&self) -> &[String] {
        &self.profile_formats
    }

    /// Parse the `parent` file to get parent profile paths
    ///
    /// Paths are relative to this profile directory and resolved to absolute paths.
    pub fn parents(&self) -> Result<Vec<PathBuf>> {
        let lines = util::read_lines(self.path.join("parent"))?;
        Ok(lines.iter().map(|l| self.path.join(l)).collect())
    }

    /// Parse the `packages` file into raw [`PackageEntry`]s (no cross-profile
    /// removal applied; that happens in [`ProfileStack::packages`]).
    fn packages_raw(&self) -> Result<Vec<PackageEntry>> {
        let lines = util::read_lines(self.path.join("packages"))?;
        let mut result = Vec::new();
        for line in lines {
            let entry = if line == "-*" {
                PackageEntry::Clear
            } else if let Some(rest) = line.strip_prefix('-') {
                // A removal line echoes the *original* text of the addition
                // it cancels, `*` marker and all (e.g. `arch/riscv/packages`
                // has `-*sys-apps/busybox`, removing `default/linux`'s
                // `*sys-apps/busybox` system add) — the marker doesn't change
                // what gets removed (`Remove` matches by dep identity
                // regardless of whether the retained entry was System or
                // Plain), so strip it too before parsing.
                // building the riscv profile's stage1 packages.build list for
                // the first time ever — no prior profile stack this codebase
                // exercised happened to use this removal form. See
                let rest = rest.strip_prefix('*').unwrap_or(rest);
                PackageEntry::Remove(Dep::parse(rest.trim())?)
            } else if let Some(rest) = line.strip_prefix('*') {
                PackageEntry::System(Dep::parse(rest.trim())?)
            } else {
                PackageEntry::Plain(Dep::parse(line.trim())?)
            };
            result.push(entry);
        }
        Ok(result)
    }

    /// Parse the `packages.build` file into raw entries (PMS doesn't cover this
    /// file; it's a catalyst/crossdev-stages convention: the stage1 bootstrap
    /// package list, in build order). Unlike `packages`, entries are never
    /// `*`-prefixed (there is no advisory-vs-system distinction here) but the
    /// file still uses the same incremental `-atom`/`-*` removal as other
    /// profile files (`portage.util.stack_lists(..., incremental=1)`).
    fn packages_build_raw(&self) -> Result<Vec<PackageEntry>> {
        let lines = util::read_lines(self.path.join("packages.build"))?;
        let mut result = Vec::new();
        for line in lines {
            let entry = if line == "-*" {
                PackageEntry::Clear
            } else if let Some(rest) = line.strip_prefix('-') {
                PackageEntry::Remove(Dep::parse(rest.trim())?)
            } else {
                PackageEntry::Plain(Dep::parse(line.trim())?)
            };
            result.push(entry);
        }
        Ok(result)
    }

    /// Parse `package.mask`
    ///
    /// Lines prefixed with `-` remove a previously masked atom (PMS 5.2.8
    /// incremental semantics). Since this is a single-profile view, removals
    /// simply aren't included in the output.
    ///
    /// See [PMS 5.2.8](https://projects.gentoo.org/pms/9/pms.html#packagemask).
    pub fn package_mask(&self) -> Result<Vec<Dep>> {
        let lines = util::read_lines(self.path.join("package.mask"))?;
        let mut result = Vec::new();
        for line in lines {
            if let Some(stripped) = line.strip_prefix('-') {
                let dep = Dep::parse(stripped.trim())?;
                result.retain(|d| d != &dep);
            } else {
                result.push(Dep::parse(line.trim())?);
            }
        }
        Ok(result)
    }

    /// Parse `package.use`
    ///
    /// Returns `(dep, [flags...])` pairs.
    pub fn package_use(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use"))
    }

    /// Parse `package.accept_keywords` (and legacy `package.keywords`)
    ///
    /// Returns `(dep, [keyword tokens...])` pairs; a bare atom (no tokens) is
    /// kept with an empty token list, meaning "accept this package's testing
    /// keyword for the configured arch".
    pub fn package_accept_keywords(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        let mut out = parse_atom_flags_list(&self.path.join("package.accept_keywords"))?;
        out.extend(parse_atom_flags_list(&self.path.join("package.keywords"))?);
        Ok(out)
    }

    /// Parse `package.license`
    ///
    /// Returns `(dep, [license tokens...])` pairs — the tokens extend
    /// `ACCEPT_LICENSE` for matching packages (`@GROUP`, `-deny`, `*`).
    pub fn package_license(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.license"))
    }

    /// Parse `use.force`
    pub fn use_force(&self) -> Result<Vec<String>> {
        util::read_lines(self.path.join("use.force"))
    }

    /// Parse `use.mask`
    pub fn use_mask(&self) -> Result<Vec<String>> {
        util::read_lines(self.path.join("use.mask"))
    }

    /// Parse `use.stable.force`
    pub fn use_stable_force(&self) -> Result<Vec<String>> {
        util::read_lines(self.path.join("use.stable.force"))
    }

    /// Parse `use.stable.mask`
    pub fn use_stable_mask(&self) -> Result<Vec<String>> {
        util::read_lines(self.path.join("use.stable.mask"))
    }

    /// Parse `package.use.force`
    pub fn package_use_force(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use.force"))
    }

    /// Parse `package.use.mask`
    pub fn package_use_mask(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use.mask"))
    }

    /// Parse `package.use.stable.force`
    pub fn package_use_stable_force(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use.stable.force"))
    }

    /// Parse `package.use.stable.mask`
    pub fn package_use_stable_mask(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use.stable.mask"))
    }
}

// ---------------------------------------------------------------------------
// ProfileStack
// ---------------------------------------------------------------------------

/// A fully-resolved profile stack produced by following all `parent` files
/// recursively.
///
/// Profiles are stored in resolution order: root ancestors first, the active
/// (leaf) profile last.  The `use_*` and `package_mask` methods merge entries
/// across the full stack and apply incremental removal (lines prefixed with
/// `-`) per PMS 5.2.5.
///
/// See [PMS 5.1](https://projects.gentoo.org/pms/9/pms.html#profiles) and
/// [PMS 5.2.5](https://projects.gentoo.org/pms/9/pms.html#profile-inheritance).
#[derive(Debug, Clone)]
pub struct ProfileStack {
    /// Profiles in resolution order: root ancestors first, leaf last
    profiles: Vec<Profile>,
}

impl ProfileStack {
    /// Build the full profile stack for the directory at `path`
    ///
    /// Follows `parent` files recursively (depth-first).  Each unique profile
    /// directory is included at most once even in diamond-shaped inheritance.
    /// Cycle detection uses canonicalized paths.
    pub fn build(path: PathBuf) -> Result<Self> {
        let mut visited = HashSet::new();
        let profiles = collect_stack(&path, &mut visited)?;
        if profiles.is_empty() {
            return Err(Error::InvalidProfile("empty profile stack".into()));
        }
        Ok(ProfileStack { profiles })
    }

    /// Append a site-local user-configuration profile (Portage's
    /// `/etc/portage/profile`) as the final, highest-priority layer.
    ///
    /// Portage treats this directory as a profile appended *after* the resolved
    /// `make.profile` chain, so its `use.force`/`use.mask`/`package.use*`/
    /// `package.mask`/`packages`/`make.defaults` override everything below it
    /// (see portage(5), `LocationsManager`'s `CUSTOM_PROFILE_PATH`). Unlike a
    /// normal profile it is a **flat** node: its own `parent` file is *not*
    /// followed. A no-op when `dir` does not exist or is not a directory.
    pub fn with_user_profile(mut self, dir: PathBuf) -> Result<Self> {
        if dir.is_dir() {
            let mut p = Profile::open(dir)?;
            // Portage hardcodes the site-local user profile's profile_formats
            // to `("profile-bashrcs", "profile-set")` regardless of any
            // enclosing repo (LocationsManager.py:182), so its plain `packages`
            // lines always contribute to `@profile` — override whatever the
            // walk-up in `Profile::open` produced.
            p.profile_formats = vec!["profile-bashrcs".to_string(), "profile-set".to_string()];
            self.profiles.push(p);
        }
        Ok(self)
    }

    /// All profiles in resolution order: root ancestors first, leaf last
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// The active (leaf) profile — last in the stack
    ///
    /// # Panics
    ///
    /// Panics if the profile stack is empty. In practice this should never occur
    /// because [`ProfileStack::build`] validates non-emptiness and `profiles` is a
    /// private field with no other public constructors.
    pub fn leaf(&self) -> &Profile {
        // SAFETY: ProfileStack::build() rejects empty stacks (line 326-328) and
        // profiles is private with no other constructors, so it is never empty.
        self.profiles.last().expect("stack is never empty")
    }

    /// Whether the leaf profile has a `deprecated` file
    ///
    /// See [PMS 5.2.3](https://projects.gentoo.org/pms/9/pms.html#deprecated).
    pub fn is_deprecated(&self) -> bool {
        self.leaf().path().join("deprecated").exists()
    }

    /// Merged `use.force` across the full stack (incremental, `-` removes)
    ///
    /// See [PMS 5.2.9](https://projects.gentoo.org/pms/9/pms.html#use-flags).
    pub fn use_force(&self) -> Result<Vec<String>> {
        merge_use_flags(
            self.profiles
                .iter()
                .map(|p| read_profile_file(&p.path().join("use.force"))),
        )
    }

    /// Merged `use.mask` across the full stack (incremental, `-` removes)
    pub fn use_mask(&self) -> Result<Vec<String>> {
        merge_use_flags(
            self.profiles
                .iter()
                .map(|p| read_profile_file(&p.path().join("use.mask"))),
        )
    }

    /// Merged `use.stable.force` across the full stack (incremental, `-` removes)
    pub fn use_stable_force(&self) -> Result<Vec<String>> {
        merge_use_flags(
            self.profiles
                .iter()
                .map(|p| read_profile_file(&p.path().join("use.stable.force"))),
        )
    }

    /// Merged `use.stable.mask` across the full stack (incremental, `-` removes)
    pub fn use_stable_mask(&self) -> Result<Vec<String>> {
        merge_use_flags(
            self.profiles
                .iter()
                .map(|p| read_profile_file(&p.path().join("use.stable.mask"))),
        )
    }

    /// Merged `package.mask` across the full stack (incremental, `-atom` unmasks)
    ///
    /// See [PMS 5.2.8](https://projects.gentoo.org/pms/9/pms.html#package-mask).
    pub fn package_mask(&self) -> Result<Vec<Dep>> {
        merge_atom_list(
            self.profiles
                .iter()
                .map(|p| read_profile_file(&p.path().join("package.mask"))),
        )
    }

    /// Accumulated `package.provided` across the full stack (incremental
    /// `-cat/pkg-version` removal).
    ///
    /// Each line is an exact `cat/pkg-version` the system provides externally
    /// (e.g. a host-supplied interpreter in a Gentoo Prefix): it satisfies
    /// matching dependencies and is never built or listed for merge. A leading
    /// `=` is tolerated for symmetry with dependency-atom syntax.
    ///
    /// See portage(5) `package.provided`.
    pub fn package_provided(&self) -> Result<Vec<Cpv>> {
        let mut acc: Vec<Cpv> = Vec::new();
        for p in &self.profiles {
            for line in read_profile_file(&p.path().join("package.provided"))? {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix('-') {
                    let cpv = parse_provided_cpv(rest.trim())?;
                    acc.retain(|c| c != &cpv);
                } else {
                    let cpv = parse_provided_cpv(line)?;
                    if !acc.contains(&cpv) {
                        acc.push(cpv);
                    }
                }
            }
        }
        Ok(acc)
    }

    /// Accumulated `packages` entries across the full stack, with incremental
    /// `-atom` / `-*` removal applied (portage `stack_lists(..., incremental=1)`).
    ///
    /// Returns `(is_system, dep)` pairs. A `*cat/pkg` line is a system package
    /// (PMS 5.2.6); a `-cat/pkg` line removes a prior `cat/pkg` across the
    /// stack; `-*` clears everything accumulated so far. Ancestors are processed
    /// first (root → leaf), so a leaf `-` correctly removes a parent's add.
    /// `@set` references are not valid in profile `packages` files (portage
    /// rejects them at parse time) and are not handled here.
    ///
    /// See [PMS 5.2.6](https://projects.gentoo.org/pms/9/pms.html#packages).
    pub fn packages(&self) -> Result<Vec<(bool, Dep)>> {
        // Preserve insertion order while allowing removal. A `-cat/pkg` removes
        // the matching prior atom (plain or system) — portage strips `*` before
        // recording, so the `-` form matches the resulting atom regardless of
        // whether it was a system add.
        let mut acc: Vec<(bool, Dep)> = Vec::new();
        for p in &self.profiles {
            for entry in p.packages_raw()? {
                match entry {
                    PackageEntry::System(d) => acc.push((true, d)),
                    PackageEntry::Plain(d) => acc.push((false, d)),
                    PackageEntry::Remove(d) => {
                        acc.retain(|(_, existing)| existing != &d);
                    }
                    PackageEntry::Clear => acc.clear(),
                }
            }
        }
        Ok(acc)
    }

    /// The profile-derived `@system` set: the `*cat/pkg` entries from
    /// [`packages`](Self::packages), with incremental `-` removal applied across
    /// the stack. Matches portage's `PackagesSystemSet`, which keeps only the
    /// `*`-marked atoms (the plain ones are advisory `@profile`, not `@system` —
    /// see [`profile_set`](Self::profile_set)).
    pub fn system_set(&self) -> Result<Vec<Dep>> {
        Ok(self
            .packages()?
            .into_iter()
            .filter_map(|(is_sys, d)| is_sys.then_some(d))
            .collect())
    }

    /// The profile-derived `@profile` set: the non-`*` "advisory" `packages`
    /// lines, accumulated with incremental `-`/`-*` removal — but **only from
    /// profiles whose enclosing repo declares `profile-set` in
    /// `profile-formats`** (a portage extension, not PMS).
    ///
    /// PMS 5.2.6 ignores plain (non-`*`) `packages` lines for the *system*
    /// set. Portage additionally routes them into `@profile`, gated on the
    /// repo-level `profile-set` flag — stock Gentoo doesn't set it, so
    /// `@profile` is empty *unless* the user profile contributes plain
    /// lines (hardcoded to `profile-set`, see [`Profile::profile_formats`]).
    ///
    /// Matches `portage.sets.ProfilePackageSet.ProfilePackageSet.load`
    /// (`if "profile-set" in y.profile_formats` … `if x[:1] != "*"`).
    pub fn profile_set(&self) -> Result<Vec<Dep>> {
        // Same incremental accumulation as `packages()`, but restricted to the
        // profile-set-contributing subset (portage's `stack_lists` runs over
        // that filtered list, then drops `*`-marked entries).
        let mut acc: Vec<(bool, Dep)> = Vec::new();
        for p in &self.profiles {
            if !p.profile_formats.iter().any(|f| f == "profile-set") {
                continue;
            }
            for entry in p.packages_raw()? {
                match entry {
                    PackageEntry::System(d) => acc.push((true, d)),
                    PackageEntry::Plain(d) => acc.push((false, d)),
                    PackageEntry::Remove(d) => acc.retain(|(_, existing)| existing != &d),
                    PackageEntry::Clear => acc.clear(),
                }
            }
        }
        Ok(acc
            .into_iter()
            .filter_map(|(is_sys, d)| (!is_sys).then_some(d))
            .collect())
    }

    /// Accumulated `packages.build` entries across the stack, in build
    /// order, with incremental `-atom`/`-*` removal. The **stage1 bootstrap
    /// list** — the minimal set a stage1 emerges into an empty root, much
    /// smaller than [`system_set`](Self::system_set)'s `@system`. Bare
    /// `cat/pkg` atoms; see [`stage1_packages`](Self::stage1_packages) for
    /// the version-qualified merge.
    pub fn packages_build(&self) -> Result<Vec<Dep>> {
        let mut acc: Vec<Dep> = Vec::new();
        for p in &self.profiles {
            for entry in p.packages_build_raw()? {
                match entry {
                    PackageEntry::Plain(d) => acc.push(d),
                    PackageEntry::Remove(d) => acc.retain(|existing| existing != &d),
                    PackageEntry::Clear => acc.clear(),
                    PackageEntry::System(_) => {
                        unreachable!("packages_build_raw never produces System entries")
                    }
                }
            }
        }
        Ok(acc)
    }

    /// The stage1 package list, version-qualified: mirrors catalyst's
    /// `targets/stage1/build.py`.
    ///
    /// [`packages_build`](Self::packages_build) gives build *order* and
    /// *membership* as bare `cat/pkg` atoms; [`packages`](Self::packages)
    /// (`@system`) may carry version/operator constraints for the same key.
    /// Each `packages` entry whose key matches a `packages.build` slot
    /// replaces that slot with the (possibly versioned) `packages` atom —
    /// same key, so build order is preserved.
    ///
    /// `packages.build` entries with no `packages` match are kept bare;
    /// `packages` entries with no `packages.build` match are dropped (the
    /// build list drives order and membership, not the system list).
    pub fn stage1_packages(&self) -> Result<Vec<Dep>> {
        let mut result = self.packages_build()?;
        for (_, dep) in self.packages()? {
            if let Some(slot) = result.iter_mut().find(|d| d.cpn == dep.cpn) {
                *slot = dep;
            }
        }
        Ok(result)
    }

    /// Accumulated `package.use` entries from the full stack, ancestors first
    ///
    /// Entries should be applied in order; a later entry for the same atom
    /// takes precedence.
    pub fn package_use(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        collect_atom_flags(self.profiles.iter().map(|p| p.package_use()))
    }

    /// Accumulated `package.accept_keywords` entries from the full stack,
    /// ancestors first. Bare atoms are preserved (empty token list).
    pub fn package_accept_keywords(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        collect_atom_flags(self.profiles.iter().map(|p| p.package_accept_keywords()))
    }

    /// Accumulated `package.license` entries from the full stack, ancestors first
    pub fn package_license(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        collect_atom_flags(self.profiles.iter().map(|p| p.package_license()))
    }

    /// Accumulated `package.use.force` entries from the full stack
    pub fn package_use_force(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        collect_atom_flags(self.profiles.iter().map(|p| p.package_use_force()))
    }

    /// Accumulated `package.use.mask` entries from the full stack
    pub fn package_use_mask(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        collect_atom_flags(self.profiles.iter().map(|p| p.package_use_mask()))
    }

    /// Accumulated `package.use.stable.force` entries from the full stack
    pub fn package_use_stable_force(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        collect_atom_flags(self.profiles.iter().map(|p| p.package_use_stable_force()))
    }

    /// Accumulated `package.use.stable.mask` entries from the full stack
    pub fn package_use_stable_mask(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        collect_atom_flags(self.profiles.iter().map(|p| p.package_use_stable_mask()))
    }
}

// ---------------------------------------------------------------------------
// UseFlags
// ---------------------------------------------------------------------------

/// The effective resolved USE flags for a package build environment
///
/// Contains only enabled flags — USE_EXPAND expansions applied,
/// `use.force` added, `use.mask` removed, environment layer applied.
///
/// Produced by [`ProfileStack::use_flags`] (defined in
/// `portage_repo::build::profile`, which has access to `EbuildShell`).
#[derive(Debug, Clone, Default)]
pub struct UseFlags(pub(crate) Vec<Interned<DefaultInterner>>);

impl UseFlags {
    /// Iterate over the enabled flag names
    pub fn iter(&self) -> impl Iterator<Item = &Interned<DefaultInterner>> {
        self.0.iter()
    }

    /// Whether the effective USE set is empty
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of effective USE flags in this set
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl IntoIterator for UseFlags {
    type Item = Interned<DefaultInterner>;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

// ---------------------------------------------------------------------------
// ProfileEnvLayer / ProfileEnv
// ---------------------------------------------------------------------------

/// Variables captured from a single `make.defaults` file, evaluated through
/// a real bash shell.
///
/// Each profile level in the stack contributes one layer.  Keeping layers
/// separate allows portage-style incremental accumulation (each level's
/// `USE=` is a delta, not a bash replacement) and makes it easy to trace
/// which profile introduced or removed a particular flag.
///
/// Layers are produced by [`ProfileStack::profile_env`] (defined in
/// `portage_repo::build::profile`, which has access to `EbuildShell`).
#[derive(Debug, Clone)]
pub struct ProfileEnvLayer {
    /// Absolute path to the `make.defaults` file this layer was read from
    pub path: PathBuf,
    /// Variables contributed by this file — always one of the fixed
    /// portage-incremental names (`build::profile::INCREMENTAL_VARS`), hence
    /// `&'static str` keys.
    /// Each value is the raw string as seen by the shell after the file ran;
    /// cross-layer accumulation is handled by [`ProfileEnv::merge`].
    pub(crate) vars: HashMap<&'static str, String>,
    /// This layer's translated USE contribution (USE + USE_EXPAND tokens),
    /// not yet merged with ancestors.
    pub use_tokens: String,
}

impl ProfileEnvLayer {
    /// Get the value of a variable from this layer
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    /// All variable names set in this layer
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.vars.keys().copied()
    }
}

/// The stacked profile environment across all layers of a [`ProfileStack`]
///
/// Each [`ProfileEnvLayer`] corresponds to one `make.defaults` file.
/// Variables are not yet merged; call [`merge`](Self::merge) or the
/// higher-level helpers to collapse them with portage-style incremental
/// semantics.
#[derive(Debug, Clone, Default)]
pub struct ProfileEnv {
    /// Layers in resolution order: root ancestors first, leaf last
    pub layers: Vec<ProfileEnvLayer>,
}

impl ProfileEnv {
    /// Merge a space-separated list variable across all layers using
    /// portage-style incremental semantics.
    ///
    /// Each layer's value is split on whitespace; tokens prefixed with `-`
    /// remove previously accumulated tokens.
    pub fn merge(&self, var: &str) -> Vec<String> {
        merge_flag_lists(self.layers.iter().filter_map(|l| l.get(var)))
    }
}

/// Merge space-separated flag lists with incremental semantics
///
/// Tokens prefixed with `-` remove previously accumulated tokens. The special
/// token `-*` is the incremental *clear-all* (make.conf(5): "Clearing these
/// variables requires a clear-all as in: `export USE=-*`"): it discards every
/// flag accumulated so far, so later tokens rebuild from empty.
pub(crate) fn merge_flag_lists<'a>(iter: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut acc: Vec<String> = Vec::new();
    for val in iter {
        for token in val.split_whitespace() {
            if token == "-*" {
                seen.clear();
                acc.clear();
                continue;
            }
            if let Some(name) = token.strip_prefix('-') {
                if seen.remove(name) {
                    acc.retain(|f| f != name);
                }
            } else if seen.insert(token.to_string()) {
                acc.push(token.to_string());
            }
        }
    }
    acc
}

/// Like [`merge_flag_lists`] but *preserves explicit disables*: a flag whose
/// net final token is negative is emitted as `-flag` rather than dropped,
/// so `USE=-foo` survives even when `foo` was never enabled in a prior
/// layer — visible to the per-package step, which must override a `+foo`
/// IUSE default. Round-trips across profile/`make.conf`/env layers. Used
/// only for the `USE` var.
///
/// `-*` (the incremental clear-all, make.conf(5)) is preserved as a
/// *leading* literal token rather than consumed. Portage's USE accumulator
/// sits the ebuild's own IUSE defaults *below* the profile/`make.conf`/env
/// layers, so `em` (folding IUSE defaults in a later per-package step) must
/// remember a clear-all happened to avoid resurrecting a `+`-defaulted flag
/// (e.g. curl's `+quic` under `USE="-* build"`).
///
/// Keeping the token round-trips correctly: feeding this output back as
/// `prev` re-fires the same clear and keeps propagating the signal forward.
/// The top-level consumer (`build::profile::collect_use_flags`) strips it.
///
/// Twinned with `portage_solver::use_config`'s identically-named function
/// (deliberately duplicated, not shared — see its doc for why); a PMS fix here
/// must land there too.
pub(crate) fn merge_flag_lists_signed<'a>(iter: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut state: HashMap<String, bool> = HashMap::new();
    let mut saw_wildcard = false;
    for val in iter {
        for token in val.split_whitespace() {
            // `-*` clear-all: discard everything accumulated so far.
            if token == "-*" {
                order.clear();
                state.clear();
                saw_wildcard = true;
                continue;
            }
            let (name, enabled) = match token.strip_prefix('-') {
                Some(n) => (n.to_string(), false),
                None => (token.to_string(), true),
            };
            if !state.contains_key(&name) {
                order.push(name.clone());
            }
            state.insert(name, enabled);
        }
    }
    let mut out: Vec<String> = order
        .into_iter()
        .map(|n| if state[&n] { n } else { format!("-{n}") })
        .collect();
    if saw_wildcard {
        out.insert(0, "-*".to_string());
    }
    out
}

/// Recursively collect profiles depth-first, ancestors before self
///
/// `visited` is a set of canonicalized paths already added; a profile seen a
/// second time (diamond inheritance or cycle) is silently skipped.
fn collect_stack(path: &Path, visited: &mut HashSet<PathBuf>) -> Result<Vec<Profile>> {
    let canonical = path.canonicalize().map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    if !visited.insert(canonical.clone()) {
        return Ok(vec![]);
    }
    let profile = Profile::open(canonical)?;
    let mut result = Vec::new();
    for parent in profile.parents()? {
        result.extend(collect_stack(&parent, visited)?);
    }
    result.push(profile);
    Ok(result)
}

/// Resolve the effective `profile-formats` for a profile directory by walking
/// up to the nearest enclosing repository root (the closest ancestor holding
/// `metadata/layout.conf`) and reading its `profile-formats` key.
///
/// Mirrors portage's longest-path-wins `intersecting_repos` rule: of all
/// repos whose root path the profile path starts with, the most specific
/// (nearest) one wins. Returns an empty list when no enclosing repo is
/// found — equivalent to portage's `portage-1`/`portage-1-compat` default,
/// so such a profile does not contribute to `@profile`.
///
/// This walk approximates `intersecting_repos` using only the filesystem:
/// `metadata/layout.conf` is the usual repo-root signal, but a repo may
/// legitimately lack one (defaulting to no `profile-formats`). PMS's own
/// `profiles/repo_name` marker is checked as a hard stop for that case, so
/// a real (layout.conf-less) repo root doesn't fall through to an
/// unrelated ancestor's own `layout.conf` (nested/co-located repos).
fn resolve_profile_formats(profile_path: &Path) -> Vec<String> {
    let mut dir = profile_path.parent();
    while let Some(d) = dir {
        let layout_path = d.join("metadata").join("layout.conf");
        if layout_path.is_file() {
            // Nearest enclosing repo found. A parse failure means the format
            // info is unusable; treat it as "no profile-set" (portage would
            // warn and intersect with the valid-format set, which is empty
            // here) rather than continuing further up the tree.
            if let Ok(text) = std::fs::read_to_string(&layout_path)
                && let Ok(parsed) = LayoutConf::parse(&text)
            {
                return parsed.profile_formats;
            }
            return Vec::new();
        }
        if d.join("profiles").join("repo_name").is_file() {
            return Vec::new();
        }
        dir = d.parent();
    }
    Vec::new()
}

/// Read non-blank, non-comment lines from a profile file or directory
///
/// Directory form is PMS 5.2.4 / [`util::read_lines`] (flat, sorted, skip
/// `.…` / `…~`). Returns an empty `Vec` if `path` does not exist.
fn read_profile_file(path: &Path) -> Result<Vec<String>> {
    util::read_lines(path)
}

/// Merge incremental USE-flag lists from a sequence of profile-file results
///
/// A flag prefixed with `-` removes any previously accumulated occurrence.
fn merge_use_flags<I>(iter: I) -> Result<Vec<String>>
where
    I: Iterator<Item = Result<Vec<String>>>,
{
    let mut seen = std::collections::HashSet::new();
    let mut acc: Vec<String> = Vec::new();
    for chunk in iter {
        for flag in chunk? {
            if let Some(name) = flag.strip_prefix('-') {
                if seen.remove(name) {
                    acc.retain(|f| f != name);
                }
            } else if seen.insert(flag.clone()) {
                acc.push(flag);
            }
        }
    }
    Ok(acc)
}

/// Parse one `package.provided` entry into a [`Cpv`], tolerating a leading `=`
fn parse_provided_cpv(s: &str) -> Result<Cpv> {
    let s = s.strip_prefix('=').unwrap_or(s);
    Ok(Cpv::parse(s)?)
}

/// Merge incremental atom lists (`package.mask` format)
///
/// A line prefixed with `-` causes that atom to be removed from the
/// accumulated set.
fn merge_atom_list<I>(iter: I) -> Result<Vec<Dep>>
where
    I: Iterator<Item = Result<Vec<String>>>,
{
    let mut acc: Vec<Dep> = Vec::new();
    for chunk in iter {
        for line in chunk? {
            if let Some(stripped) = line.strip_prefix('-') {
                let dep = Dep::parse(stripped.trim())?;
                acc.retain(|d| d != &dep);
            } else {
                let dep = Dep::parse(line.trim())?;
                if !acc.contains(&dep) {
                    acc.push(dep);
                }
            }
        }
    }
    Ok(acc)
}

/// Concatenate `(atom, flags)` lists from a sequence of profiles
fn collect_atom_flags<I>(iter: I) -> Result<Vec<(Dep, Vec<String>)>>
where
    I: Iterator<Item = Result<Vec<(Dep, Vec<String>)>>>,
{
    let mut acc = Vec::new();
    for chunk in iter {
        acc.extend(chunk?);
    }
    Ok(acc)
}

// ---------------------------------------------------------------------------
// Existing per-profile helpers
// ---------------------------------------------------------------------------

/// Parse a file containing one dependency atom per line
/// Parse a file containing `atom flag1 flag2 ...` per line.
fn parse_atom_flags_list(path: &Path) -> Result<Vec<(Dep, Vec<String>)>> {
    let lines = util::read_lines(path)?;
    let mut result = Vec::new();
    for line in lines {
        let mut parts = line.split_whitespace();
        if let Some(atom_str) = parts.next() {
            let dep = Dep::parse(atom_str)?;
            let flags: Vec<String> = parts.map(String::from).collect();
            result.push((dep, flags));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_profile_desc_line() {
        let desc = ProfileDesc::parse("amd64 default/linux/amd64/23.0 stable").unwrap();
        assert_eq!(desc.arch(), "amd64");
        assert_eq!(desc.path(), "default/linux/amd64/23.0");
        assert_eq!(desc.status(), &ProfileStatus::Stable);
    }

    #[test]
    fn parse_profile_desc_dev() {
        let desc = ProfileDesc::parse("arm64 default/linux/arm64/23.0 dev").unwrap();
        assert_eq!(desc.status(), &ProfileStatus::Dev);
    }

    #[test]
    fn parse_profile_desc_exp() {
        let desc = ProfileDesc::parse("riscv default/linux/riscv/23.0 exp").unwrap();
        assert_eq!(desc.status(), &ProfileStatus::Exp);
    }

    #[test]
    fn parse_profile_desc_other_status() {
        let desc = ProfileDesc::parse("x86 some/path testing").unwrap();
        assert_eq!(desc.status(), &ProfileStatus::Other("testing".to_string()));
    }

    #[test]
    fn parse_profile_desc_too_few_fields() {
        assert!(ProfileDesc::parse("amd64 some/path").is_err());
    }

    // --- incremental merge (`-*` clear-all) tests ---

    #[test]
    fn merge_flag_lists_dash_star_clears_accumulated() {
        // `-*` discards everything seen so far; later tokens rebuild from empty.
        let out = merge_flag_lists(["foo bar", "-* baz"].into_iter());
        assert_eq!(out, vec!["baz".to_string()]);
    }

    #[test]
    fn merge_flag_lists_dash_star_within_one_layer() {
        let out = merge_flag_lists(["foo -* bar"].into_iter());
        assert_eq!(out, vec!["bar".to_string()]);
    }

    #[test]
    fn merge_flag_lists_dash_star_alone_empties() {
        let out = merge_flag_lists(["foo bar", "-*"].into_iter());
        assert!(out.is_empty());
    }

    #[test]
    fn merge_flag_lists_signed_dash_star_preserved_as_leading_marker() {
        // The signed USE merge clears accumulated state on `-*` and keeps the
        // token as a leading marker so downstream knows a clear-all happened
        // (to suppress a `+`-defaulted IUSE flag the user cleared).
        let out = merge_flag_lists_signed(["foo -bar", "-* build"].into_iter());
        assert_eq!(out, vec!["-*".to_string(), "build".to_string()]);
    }

    #[test]
    fn merge_flag_lists_signed_dash_star_then_disable_survives() {
        // After a clear-all, a subsequent disable is a fresh explicit disable.
        let out = merge_flag_lists_signed(["a", "-* -debug"].into_iter());
        assert_eq!(out, vec!["-*".to_string(), "-debug".to_string()]);
    }

    #[test]
    fn merge_flag_lists_signed_dash_star_round_trips() {
        // Feeding the output back as `prev` re-fires the clear (a no-op on the
        // already-reduced list) and keeps the marker, so it chains across layers.
        let first = merge_flag_lists_signed(["foo", "-* build"].into_iter());
        let second = merge_flag_lists_signed([first.join(" ").as_str(), "ssl"].into_iter());
        assert_eq!(
            second,
            vec!["-*".to_string(), "build".to_string(), "ssl".to_string()]
        );
    }

    // --- ProfileStack tests ---

    use std::io::Write as _;
    use tempfile::TempDir;

    /// Create a minimal profile directory with an optional `eapi` file and
    /// an optional `parent` file listing relative paths.
    fn make_profile(dir: &TempDir, name: &str, parents: &[&str]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        // no eapi file → defaults to EAPI 0
        if !parents.is_empty() {
            let mut f = std::fs::File::create(path.join("parent")).unwrap();
            for p in parents {
                writeln!(f, "{p}").unwrap();
            }
        }
        path
    }

    #[test]
    fn package_provided_merges_incrementally_across_stack() {
        // parent provides two CPVs (one with a leading `=`); leaf removes one
        // and adds another. Result is order-preserving with the removal applied.
        let dir = tempfile::tempdir().unwrap();
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(
            parent.join("package.provided"),
            "dev-lang/python-3.14.0\n=sys-devel/gcc-14.2.0\n",
        )
        .unwrap();
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(
            leaf.join("package.provided"),
            "-sys-devel/gcc-14.2.0\ndev-lang/perl-5.42.0\n",
        )
        .unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let provided = stack.package_provided().unwrap();
        let got: Vec<String> = provided.iter().map(|c| c.to_string()).collect();
        assert_eq!(got, vec!["dev-lang/python-3.14.0", "dev-lang/perl-5.42.0"]);
    }

    #[test]
    fn stack_single_profile_no_parents() {
        let dir = tempfile::tempdir().unwrap();
        let p = make_profile(&dir, "leaf", &[]);
        let stack = ProfileStack::build(p).unwrap();
        assert_eq!(stack.profiles().len(), 1);
    }

    #[test]
    fn stack_linear_chain() {
        // grand → parent → leaf
        let dir = tempfile::tempdir().unwrap();
        make_profile(&dir, "grand", &[]);
        make_profile(&dir, "parent", &["../grand"]);
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        let stack = ProfileStack::build(leaf).unwrap();
        assert_eq!(stack.profiles().len(), 3);
        // leaf must be last
        assert_eq!(stack.leaf().path(), stack.profiles().last().unwrap().path());
    }

    #[test]
    fn stack_diamond_inheritance() {
        // base ← left ← leaf
        //      ← right ←/
        // base must appear exactly once.
        let dir = tempfile::tempdir().unwrap();
        make_profile(&dir, "base", &[]);
        make_profile(&dir, "left", &["../base"]);
        make_profile(&dir, "right", &["../base"]);
        let leaf = make_profile(&dir, "leaf", &["../left", "../right"]);
        let stack = ProfileStack::build(leaf).unwrap();
        assert_eq!(stack.profiles().len(), 4, "base should appear only once");
    }

    #[test]
    fn use_mask_merges_incrementally() {
        let dir = tempfile::tempdir().unwrap();
        // parent masks foo and bar
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(parent.join("use.mask"), "foo\nbar\n").unwrap();
        // leaf unmasks foo, adds baz
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(leaf.join("use.mask"), "-foo\nbaz\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let masked = stack.use_mask().unwrap();
        assert!(
            !masked.contains(&"foo".to_string()),
            "foo should be unmasked"
        );
        assert!(masked.contains(&"bar".to_string()));
        assert!(masked.contains(&"baz".to_string()));
    }

    #[test]
    fn use_force_merges_incrementally() {
        let dir = tempfile::tempdir().unwrap();
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(parent.join("use.force"), "ipv6\n").unwrap();
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(leaf.join("use.force"), "-ipv6\nnls\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let forced = stack.use_force().unwrap();
        assert!(!forced.contains(&"ipv6".to_string()));
        assert!(forced.contains(&"nls".to_string()));
    }

    #[test]
    fn user_profile_is_top_layer_and_overrides() {
        // Portage appends /etc/portage/profile as the highest-priority layer.
        let dir = tempfile::tempdir().unwrap();
        let leaf = make_profile(&dir, "leaf", &[]);
        std::fs::write(leaf.join("use.force"), "ipv6\n").unwrap();

        let user = dir.path().join("user-profile");
        std::fs::create_dir(&user).unwrap();
        // Unforce ipv6 (incremental removal from the top) and force nls.
        std::fs::write(user.join("use.force"), "-ipv6\nnls\n").unwrap();

        let stack = ProfileStack::build(leaf)
            .unwrap()
            .with_user_profile(user)
            .unwrap();
        let forced = stack.use_force().unwrap();
        assert!(
            !forced.contains(&"ipv6".to_string()),
            "user layer unforced ipv6"
        );
        assert!(forced.contains(&"nls".to_string()), "user layer forced nls");
    }

    #[test]
    fn user_profile_reads_directory_form_package_use_mask() {
        // /etc/portage/profile/package.use.mask is commonly a directory.
        let dir = tempfile::tempdir().unwrap();
        let leaf = make_profile(&dir, "leaf", &[]);

        let user = dir.path().join("user-profile");
        std::fs::create_dir(&user).unwrap();
        let pum = user.join("package.use.mask");
        std::fs::create_dir(&pum).unwrap();
        std::fs::write(pum.join("cross"), "cross-foo/gcc multilib cet\n").unwrap();

        let stack = ProfileStack::build(leaf)
            .unwrap()
            .with_user_profile(user)
            .unwrap();
        let masked = stack.package_use_mask().unwrap();
        let entry = masked
            .iter()
            .find(|(d, _)| d.to_string().contains("cross-foo/gcc"))
            .expect("directory-form package.use.mask read");
        assert!(entry.1.contains(&"multilib".to_string()));
        assert!(entry.1.contains(&"cet".to_string()));
    }

    #[test]
    fn with_user_profile_absent_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let leaf = make_profile(&dir, "leaf", &[]);
        let before = ProfileStack::build(leaf.clone()).unwrap().profiles().len();
        let after = ProfileStack::build(leaf)
            .unwrap()
            .with_user_profile(dir.path().join("does-not-exist"))
            .unwrap()
            .profiles()
            .len();
        assert_eq!(before, after, "absent user profile must not add a layer");
    }

    #[test]
    fn package_mask_merges_incrementally() {
        let dir = tempfile::tempdir().unwrap();
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(parent.join("package.mask"), "dev-libs/foo\n").unwrap();
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(leaf.join("package.mask"), "-dev-libs/foo\ndev-libs/bar\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let masked = stack.package_mask().unwrap();
        let names: Vec<_> = masked.iter().map(|d| d.to_string()).collect();
        assert!(!names.iter().any(|n| n.contains("foo")), "foo unmasked");
        assert!(names.iter().any(|n| n.contains("bar")));
    }

    #[test]
    fn packages_build_accumulated_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(parent.join("packages.build"), "sys-devel/binutils\n").unwrap();
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(leaf.join("packages.build"), "sys-devel/gcc\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let pkgs = stack.packages_build().unwrap();
        let names: Vec<_> = pkgs.iter().map(|d| d.to_string()).collect();
        assert_eq!(names, vec!["sys-devel/binutils", "sys-devel/gcc"]);
    }

    #[test]
    fn packages_build_honours_incremental_removal() {
        let dir = tempfile::tempdir().unwrap();
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(
            parent.join("packages.build"),
            "sys-devel/binutils\nsys-devel/gcc\n",
        )
        .unwrap();
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(leaf.join("packages.build"), "-sys-devel/gcc\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let names: Vec<_> = stack
            .packages_build()
            .unwrap()
            .iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(names, vec!["sys-devel/binutils"]);
    }

    #[test]
    fn stage1_packages_takes_version_from_system_set_keeps_build_order() {
        // Mirrors catalyst's targets/stage1/build.py: packages.build supplies
        // order+membership; packages (@system) supplies version constraints
        // for matching keys, dropping the leading `*`.
        let dir = tempfile::tempdir().unwrap();
        let p = make_profile(&dir, "leaf", &[]);
        std::fs::write(
            p.join("packages.build"),
            "sys-devel/binutils\nsys-apps/baselayout\nsys-devel/gcc\n",
        )
        .unwrap();
        std::fs::write(p.join("packages"), "*>=sys-devel/gcc-13\n*sys-libs/glibc\n").unwrap();

        let stack = ProfileStack::build(p).unwrap();
        let names: Vec<_> = stack
            .stage1_packages()
            .unwrap()
            .iter()
            .map(|d| d.to_string())
            .collect();
        // Build order preserved; gcc slot replaced with the versioned atom;
        // glibc (no packages.build match) is not injected.
        assert_eq!(
            names,
            vec![
                "sys-devel/binutils",
                "sys-apps/baselayout",
                ">=sys-devel/gcc-13"
            ]
        );
    }

    #[test]
    fn is_deprecated_false_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = make_profile(&dir, "leaf", &[]);
        let stack = ProfileStack::build(p).unwrap();
        assert!(!stack.is_deprecated());
    }

    #[test]
    fn is_deprecated_true_when_file_present() {
        let dir = tempfile::tempdir().unwrap();
        let p = make_profile(&dir, "leaf", &[]);
        std::fs::write(p.join("deprecated"), "Use foo instead.\n").unwrap();
        let stack = ProfileStack::build(p).unwrap();
        assert!(stack.is_deprecated());
    }

    #[test]
    fn packages_accumulated_from_stack() {
        let dir = tempfile::tempdir().unwrap();
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(parent.join("packages"), "*sys-libs/glibc\n").unwrap();
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(leaf.join("packages"), "*sys-kernel/linux-headers\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let pkgs = stack.packages().unwrap();
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.iter().all(|(is_sys, _)| *is_sys));
    }

    #[test]
    fn packages_removal_echoes_the_star_marker_of_the_add_it_cancels() {
        // A removal line repeats the *original* text of the addition it
        // cancels, `*` marker and all — e.g. real
        // profiles/arch/riscv/packages has `-*sys-apps/busybox`, removing
        // `default/linux`'s `*sys-apps/busybox` system add.
        // building the riscv profile's packages.build list for the first
        // time — no prior profile stack this codebase exercised happened to
        // use this removal form.
        let dir = tempfile::tempdir().unwrap();
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(
            parent.join("packages"),
            "*sys-apps/busybox\n*sys-libs/glibc\n",
        )
        .unwrap();
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(leaf.join("packages"), "-*sys-apps/busybox\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let names: Vec<_> = stack
            .packages()
            .unwrap()
            .into_iter()
            .map(|(_, d)| d.to_string())
            .collect();
        assert_eq!(names, vec!["sys-libs/glibc"]);
    }

    #[test]
    fn directory_as_file_use_mask() {
        // profile-file-dirs: use.mask is a directory with multiple files
        let dir = tempfile::tempdir().unwrap();
        let p = make_profile(&dir, "leaf", &[]);
        let mask_dir = p.join("use.mask");
        std::fs::create_dir(&mask_dir).unwrap();
        std::fs::write(mask_dir.join("01-base"), "foo\nbar\n").unwrap();
        std::fs::write(mask_dir.join("02-extra"), "baz\n").unwrap();

        let stack = ProfileStack::build(p).unwrap();
        let masked = stack.use_mask().unwrap();
        assert_eq!(masked, vec!["foo", "bar", "baz"]);
    }

    // --- @profile (profile-set) tests ---
    //
    // `@profile` is the plain (non-`*`) `packages` lines, but only from
    // profiles whose enclosing repo declares `profile-set` in `profile-formats`
    // — a portage extension (PMS 5.2.6 only says plain lines are ignored for the
    // *system* set). The temp dir acts as the repo root for the walk-up in
    // `resolve_profile_formats` (writing `metadata/layout.conf` there).

    /// Write a `metadata/layout.conf` body at `dir` (the repo root)
    fn write_layout_conf(dir: &TempDir, body: &str) {
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), body).unwrap();
    }

    #[test]
    fn profile_set_keeps_plain_lines_drops_system_when_profile_set_enabled() {
        let dir = tempfile::tempdir().unwrap();
        write_layout_conf(&dir, "profile-formats = portage-2 profile-set\n");
        let leaf = make_profile(&dir, "leaf", &[]);
        std::fs::write(
            leaf.join("packages"),
            "*sys-libs/glibc\napp-shells/bash\n", // one system, one plain
        )
        .unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let got: Vec<String> = stack
            .profile_set()
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(got, vec!["app-shells/bash"]);
        // And @system still gets the `*` line.
        let sys: Vec<String> = stack
            .system_set()
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(sys, vec!["sys-libs/glibc"]);
    }

    #[test]
    fn profile_set_empty_without_profile_set_flag() {
        // Repo declares portage-2 only (no profile-set): plain lines do NOT
        // contribute to @profile, matching real portage on the stock Gentoo
        // repo. This is the "almost always empty" case.
        let dir = tempfile::tempdir().unwrap();
        write_layout_conf(&dir, "profile-formats = portage-2\n");
        let leaf = make_profile(&dir, "leaf", &[]);
        std::fs::write(leaf.join("packages"), "*sys-libs/glibc\napp-shells/bash\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        assert!(stack.profile_set().unwrap().is_empty());
        // system_set is unaffected.
        assert_eq!(stack.system_set().unwrap().len(), 1);
    }

    #[test]
    fn profile_set_empty_when_no_enclosing_repo() {
        // No metadata/layout.conf anywhere up the tree → default has no
        // profile-set → @profile empty, even with plain lines present.
        let dir = tempfile::tempdir().unwrap();
        let leaf = make_profile(&dir, "leaf", &[]);
        std::fs::write(leaf.join("packages"), "app-shells/bash\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        assert!(stack.profile_set().unwrap().is_empty());
    }

    #[test]
    fn profile_set_incremental_removal_across_profile_set_profiles() {
        // Both profiles' repos declare profile-set; the leaf's `-app-shells/bash`
        // removes the parent's plain add (removal applies within the
        // profile-set-contributing subset, matching portage's stack_lists).
        let dir = tempfile::tempdir().unwrap();
        write_layout_conf(&dir, "profile-formats = profile-set\n");
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(
            parent.join("packages"),
            "app-shells/bash\nsys-apps/coreutils\n",
        )
        .unwrap();
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(leaf.join("packages"), "-app-shells/bash\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let got: Vec<String> = stack
            .profile_set()
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(got, vec!["sys-apps/coreutils"]);
    }

    #[test]
    fn profile_set_skips_profiles_in_repos_lacking_profile_set() {
        // Parent repo lacks profile-set; leaf repo declares it. Only the leaf's
        // plain lines contribute (and only its own removal context applies).
        // Build two sibling "repos" under separate temp dirs so their
        // layout.conf walk-ups resolve independently.
        let parent_repo = tempfile::tempdir().unwrap();
        write_layout_conf(&parent_repo, "profile-formats = portage-2\n");
        let parent = make_profile(&parent_repo, "parent", &[]);
        std::fs::write(parent.join("packages"), "app-shells/bash\n").unwrap();

        let child_repo = tempfile::tempdir().unwrap();
        write_layout_conf(&child_repo, "profile-formats = profile-set\n");
        // Leaf's parent file points at the absolute path of the parent profile.
        let leaf = make_profile(&child_repo, "leaf", &[]);
        std::fs::write(leaf.join("parent"), format!("{}\n", parent.display())).unwrap();
        std::fs::write(leaf.join("packages"), "sys-apps/coreutils\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let got: Vec<String> = stack
            .profile_set()
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        // Only the leaf (profile-set repo) contributes; parent's bash is dropped.
        assert_eq!(got, vec!["sys-apps/coreutils"]);
    }

    #[test]
    fn profile_set_user_profile_always_contributes() {
        // Portage hardcodes /etc/portage/profile to profile-set, so its plain
        // `packages` lines feed @profile even with no repo declaring it. This
        // is the one real-world case where stock-Gentoo @profile is non-empty.
        let dir = tempfile::tempdir().unwrap();
        write_layout_conf(&dir, "profile-formats = portage-2\n"); // no profile-set
        let base = make_profile(&dir, "leaf", &[]);
        std::fs::write(base.join("packages"), "*sys-libs/glibc\n").unwrap();

        // Site-local user profile with a plain advisory line.
        let user_dir = dir.path().join("etc/portage/profile");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(user_dir.join("packages"), "app-shells/dash\n").unwrap();

        let stack = ProfileStack::build(base)
            .unwrap()
            .with_user_profile(user_dir)
            .unwrap();
        let got: Vec<String> = stack
            .profile_set()
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(got, vec!["app-shells/dash"]);
    }

    #[test]
    fn profile_set_stops_at_repo_name_marker_not_an_outer_ancestors_layout_conf() {
        // The profile's real enclosing repo (`inner`, marked by
        // `profiles/repo_name` per PMS) has no `metadata/layout.conf` of its
        // own. An *outer* ancestor directory happens to have one declaring
        // `profile-set` — but `inner` must NOT borrow it; the walk should
        // stop at the confirmed repo boundary and default to empty, the same
        // as `profile_set_empty_when_no_enclosing_repo`.
        let outer = tempfile::tempdir().unwrap();
        write_layout_conf(&outer, "profile-formats = profile-set\n");

        let inner = outer.path().join("inner");
        std::fs::create_dir_all(inner.join("profiles")).unwrap();
        std::fs::write(inner.join("profiles").join("repo_name"), "inner\n").unwrap();

        let leaf = inner.join("leaf");
        std::fs::create_dir_all(&leaf).unwrap();
        std::fs::write(leaf.join("packages"), "app-shells/bash\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        assert!(stack.profile_set().unwrap().is_empty());
    }
}
