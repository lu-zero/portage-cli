//! Generic env.d-based profile selection module.
//!
//! This module provides a shared implementation for `eselect`-like modules that
//! manage profiles via env.d directories (gcc, binutils, linker).

use std::collections::BTreeMap;
use std::io::Write as _;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};

use super::{config_portage_dir_for, get_chost, is_prefix_context_for, source_label};
use crate::cli::Cli;
use crate::style::C_STAR;
use portage_atom::Version;
use portage_resolve::Roots;

/// Trait for env.d-based profile selection modules.
pub trait EnvDProfile: Sized + 'static {
    /// The module name for display purposes.
    fn module_name() -> &'static str;

    /// The subdirectory name under env.d/ (e.g., "gcc", "binutils", "linker").
    fn env_d_subdir() -> &'static str;

    /// The name of the global environment file in env.d/.
    /// For gcc: "04gcc-{target}" (includes target)
    /// For binutils: "05binutils" (no target)
    /// For linker: "06linker" (no target)
    fn global_env_file() -> &'static str;

    /// Whether the global env file name includes a target suffix that needs to be formatted.
    /// For gcc: true (uses {target})
    /// For binutils: false
    /// For linker: false
    fn global_env_uses_target() -> bool {
        false
    }

    /// The variable name to look for in profiles to extract the target (e.g., "CTARGET", "TARGET").
    fn target_var_name() -> &'static str;

    /// Additional environment variable prefixes to extract from profiles (e.g., ["LD="] for linker).
    fn extra_env_vars() -> &'static [&'static str] {
        &[]
    }

    /// Config belonging to a *different* toolchain that this selection
    /// nonetheless owns — `gcc-config` writing clang's
    /// `gentoo-gcc-install.cfg` is the real-world case. Runs after the env.d
    /// profile and the wrappers; `vars` is the parsed env.d profile.
    /// Default: nothing.
    fn sync_foreign_config(_roots: &Roots, _vars: &BTreeMap<String, String>) -> Result<()> {
        Ok(())
    }

    /// Create the toolchain wrapper symlinks in `<EPREFIX>/usr/bin` — the half of
    /// `binutils-config`/`gcc-config` that makes `<CTARGET>-gcc` find its cross
    /// `as`/`ld`. `vars` is the parsed env.d profile. Default: none — modules that
    /// only manage env.d state (linker, clang) leave this empty.
    fn install_wrappers(
        _roots: &Roots,
        _target: &str,
        _vars: &BTreeMap<String, String>,
    ) -> Result<()> {
        Ok(())
    }
}

/// Base directory for env.d files.
pub fn env_d_dir<T: EnvDProfile>(roots: &Roots) -> Utf8PathBuf {
    let config_portage = config_portage_dir_for(roots);

    // config_portage_dir returns ${EPREFIX}/etc/portage
    // env.d is a sibling directory: ${EPREFIX}/etc/env.d
    if let Some(parent) = config_portage.parent() {
        let config_env_dir = parent.join(format!("env.d/{}", T::env_d_subdir()));
        if config_env_dir.is_dir() {
            return config_env_dir;
        }
    }

    // Fall back to system location
    let system_dir = Utf8PathBuf::from(format!("/etc/env.d/{}", T::env_d_subdir()));
    if system_dir.is_dir() {
        return system_dir;
    }

    // If neither exists, return the config-root env.d location (will be created on first use)
    config_portage
        .parent()
        .unwrap_or(Utf8Path::new("/"))
        .join(format!("env.d/{}", T::env_d_subdir()))
}

/// Path to the current profile config file: `<base>/etc/env.d/<subdir>/config-<target>`,
/// where `<base>` is the host `/` for a plain run, or the `--config-root`/
/// `--local`/`--prefix` root. Derived from the configuration portage directory so a prefix
/// activation writes into the prefix, not the host `/etc` (a same-named host
/// config must not capture the write).
fn current_config_path<T: EnvDProfile>(roots: &Roots, target: &str) -> Utf8PathBuf {
    config_portage_dir_for(roots)
        .parent()
        .unwrap_or(Utf8Path::new("/"))
        .join(format!("env.d/{}/config-{}", T::env_d_subdir(), target))
}

/// Path to the global environment file, rooted like [`current_config_path`].
fn global_env_path<T: EnvDProfile>(roots: &Roots, target: &str) -> Utf8PathBuf {
    let file_name = if T::global_env_uses_target() {
        T::global_env_file().replace("{target}", target)
    } else {
        T::global_env_file().to_string()
    };

    config_portage_dir_for(roots)
        .parent()
        .unwrap_or(Utf8Path::new("/"))
        .join(format!("env.d/{}", file_name))
}

/// A profile with its target.
#[derive(Debug, Clone)]
pub struct Profile<T: EnvDProfile> {
    pub name: String,
    /// Whether this profile is from the host system or the current config root
    pub is_host: bool,
    /// The module type marker
    _marker: std::marker::PhantomData<T>,
}

/// List all profiles, grouped by target.
fn list_all_profiles<T: EnvDProfile>(roots: &Roots) -> Result<BTreeMap<String, Vec<Profile<T>>>> {
    let mut profiles_by_target: BTreeMap<String, Vec<Profile<T>>> = BTreeMap::new();

    // Check if we're in a prefix/local context
    let is_prefix_context = is_prefix_context_for(roots);

    // Collect profiles from the current config root (prefix/local)
    let prefix_base_dir = env_d_dir::<T>(roots);
    if prefix_base_dir.is_dir() {
        collect_profiles::<T>(&prefix_base_dir, &mut profiles_by_target, false)?;
    }

    // If in prefix context, also check system location
    if is_prefix_context {
        let system_dir = Utf8PathBuf::from(format!("/etc/env.d/{}", T::env_d_subdir()));
        if system_dir.is_dir() {
            collect_profiles::<T>(&system_dir, &mut profiles_by_target, true)?;
        }
    }

    for profiles in profiles_by_target.values_mut() {
        profiles.sort_by(|a, b| compare_profile_names(&a.name, &b.name));
    }

    Ok(profiles_by_target)
}

/// Helper to collect profiles from a directory.
fn collect_profiles<T: EnvDProfile>(
    base_dir: &Utf8PathBuf,
    profiles_by_target: &mut BTreeMap<String, Vec<Profile<T>>>,
    is_host: bool,
) -> Result<()> {
    for entry in std::fs::read_dir(base_dir)? {
        let entry = entry?;
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        let name = path.file_name().unwrap_or_default().to_string();

        // Skip config files
        if name.starts_with("config-") {
            continue;
        }

        // Skip non-profile files
        if name.ends_with(".conf") || name.is_empty() || !name.contains(char::is_numeric) {
            continue;
        }

        // Read the profile to get the target variable, via the same
        // quote-aware parser `set_profile` already uses — the previous
        // hand-rolled `starts_with('"') && ends_with('"')` check here
        // panicked (`begin > end` slice) on a 1-byte value that was just a
        // bare stray `"`.
        let target: Option<String> = std::fs::read_to_string(&path).ok().and_then(|content| {
            let key = T::target_var_name().trim_end_matches('=');
            parse_env_vars(&content).remove(key)
        });

        // If no target found, try to extract from profile name
        let profile_target = target.unwrap_or_else(|| {
            if let Some(pos) = name.rfind('-') {
                let version_part = &name[pos + 1..];
                if version_part.chars().all(|c| c.is_ascii_digit() || c == '.') {
                    return name[..pos].to_string();
                }
            }
            name.clone()
        });

        profiles_by_target
            .entry(profile_target.clone())
            .or_default()
            .push(Profile {
                name: name.clone(),
                is_host,
                _marker: std::marker::PhantomData,
            });
    }
    Ok(())
}

/// Get the current profile for a target.
pub(super) fn get_current_profile<T: EnvDProfile>(roots: &Roots, target: &str) -> Option<String> {
    let config_path = current_config_path::<T>(roots, target);
    let content = std::fs::read_to_string(&config_path).ok()?;
    parse_env_vars(&content).remove("CURRENT")
}

/// Set the profile for a target.
fn set_profile<T: EnvDProfile>(
    roots: &Roots,
    target: &str,
    profile: &str,
    base_dir: &Utf8PathBuf,
) -> Result<()> {
    let config_path = current_config_path::<T>(roots, target);

    let mut profile_path = base_dir.join(profile);

    if !profile_path.is_file() {
        let target_dir = base_dir.join(target);
        let target_profile_path = target_dir.join(profile);
        if target_profile_path.is_file() {
            profile_path = target_profile_path;
        }
    }

    if !profile_path.is_file() {
        bail!("{} profile '{}' not found", T::module_name(), profile);
    }

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent))?;
    }

    std::fs::write(&config_path, format!("CURRENT={}\n", profile))
        .with_context(|| format!("writing {}", config_path))?;

    let profile_content = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("reading {}", profile_path))?;

    let global_env_path = global_env_path::<T>(roots, target);
    let mut env_content = format!("# Autogenerated by 'em select {}'.\n", T::module_name());

    let all_env_vars: Vec<&str> = ["PATH=", "LDPATH=", "MANPATH=", "INFOPATH=", "ROOTPATH="]
        .into_iter()
        .chain(T::extra_env_vars().iter().copied())
        .collect();

    for line in profile_content.lines() {
        let line = line.trim();
        if all_env_vars.iter().any(|&v| line.starts_with(v)) {
            env_content.push_str(line);
            env_content.push('\n');
        }
    }

    if let Some(parent) = global_env_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent))?;
    }
    std::fs::write(&global_env_path, env_content)
        .with_context(|| format!("writing {}", global_env_path))?;

    let vars = parse_env_vars(&profile_content);
    T::install_wrappers(roots, target, &vars)?;
    T::sync_foreign_config(roots, &vars)?;

    Ok(())
}

/// `<EPREFIX>` — the config dir (`<EPREFIX>/etc/portage`) with `etc/portage`
/// stripped: `/` for a system install, the prefix for `--local`/`--prefix`.
pub(super) fn eprefix(roots: &Roots) -> Utf8PathBuf {
    config_portage_dir_for(roots)
        .parent()
        .and_then(Utf8Path::parent)
        .map(Utf8Path::to_path_buf)
        .unwrap_or_else(|| Utf8PathBuf::from("/"))
}

/// Parse `KEY="value"` env.d lines into a map (surrounding quotes stripped).
pub(super) fn parse_env_vars(content: &str) -> BTreeMap<String, String> {
    content
        .lines()
        .filter_map(|line| line.trim().split_once('='))
        .map(|(k, v)| {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(v);
            (k.trim().to_string(), v.to_string())
        })
        .collect()
}

/// The trailing version-like component of an env.d profile filename (e.g.
/// `aarch64-unknown-linux-gnu-13` -> `13`, `2.41` on its own -> `2.41`), or
/// `None` if `name` doesn't end in one. Mirrors the suffix `collect_profiles`
/// already recognizes as a version when it strips it to derive a target.
fn version_suffix(name: &str) -> Option<&str> {
    let suffix = match name.rfind('-') {
        Some(pos) => &name[pos + 1..],
        None => name,
    };
    (!suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit() || c == '.')).then_some(suffix)
}

/// Order two profile names by their trailing version, not lexicographically
/// — plain `str::cmp` picks `"...-9"` over `"...-13"` (byte-wise, `'9' >
/// '1'`), which would activate a stale single-digit-major GCC profile over a
/// newer double-digit one. Falls back to a plain string compare if either
/// side doesn't parse as a version (e.g. neither has a recognizable suffix).
fn compare_profile_names(a: &str, b: &str) -> std::cmp::Ordering {
    let parsed = |name: &str| version_suffix(name).and_then(|v| Version::parse(v).ok());
    match (parsed(a), parsed(b)) {
        (Some(va), Some(vb)) => va.cmp(&vb),
        _ => a.cmp(b),
    }
}

/// Replace `link` with a symlink pointing at `content` (mkdir parent, force-replace).
pub(super) fn symlink_force(content: &Utf8Path, link: &Utf8Path) -> Result<()> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
    }
    match std::fs::symlink_metadata(link) {
        Ok(_) => std::fs::remove_file(link).with_context(|| format!("removing {link}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("stat {link}")),
    }
    std::os::unix::fs::symlink(content, link)
        .with_context(|| format!("linking {link} -> {content}"))
}

/// Run a list action.
///
/// Use `outer_roots()`, not `roots()` — same clap `--target` collision as
/// [`crate::select::config_portage_dir`]: select must not trigger sysroot
/// substitution.
pub fn run_list<T: EnvDProfile>(globals: &Cli) -> Result<()> {
    let roots = globals.outer_roots();
    let profiles_by_target = list_all_profiles::<T>(&roots)?;
    let mut out = anstream::stdout();

    if profiles_by_target.is_empty() {
        println!("No {} profiles found", T::module_name());
        return Ok(());
    }

    // Collect all profiles across all targets to calculate total count
    let all_profiles: Vec<&Profile<T>> = profiles_by_target
        .values()
        .flat_map(|profiles| profiles.iter())
        .collect();
    let total_count = all_profiles.len();
    let num_width = total_count.to_string().len();

    let mut n = 1;
    let mut first = true;
    for (target, profiles) in &profiles_by_target {
        let current = get_current_profile::<T>(&roots, target);

        if !first {
            writeln!(out).ok();
        }
        first = false;

        for profile in profiles {
            // For some modules, the CURRENT value might be just the version or the full name
            let is_current = if let Some(current_name) = &current {
                profile.name == *current_name
                    || profile.name.ends_with(&format!("-{}", current_name))
            } else {
                false
            };
            let num = format!("[{:>width$}]", n, width = num_width);
            let mut profile_display = if is_current {
                format!("{}{C_STAR} *{C_STAR:#}", profile.name)
            } else {
                profile.name.clone()
            };

            // Add source label if in prefix context
            if is_prefix_context_for(&roots) {
                let label = source_label(profile.is_host);
                profile_display.push_str(&label);
            }

            writeln!(out, "  {num} {}", profile_display).ok();
            n += 1;
        }
    }

    Ok(())
}

/// Run a show action. `outer_roots()`, not `roots()` — see [`run_list`]'s
/// doc comment.
pub fn run_show<T: EnvDProfile>(globals: &Cli, target: &str) {
    match get_current_profile::<T>(&globals.outer_roots(), target) {
        Some(profile) => println!("{}", profile),
        None => println!(
            "(no {} profile set for target '{}')",
            T::module_name(),
            target
        ),
    }
}

/// Run a set action. `outer_roots()`, not `roots()` — see [`run_list`]'s
/// doc comment.
pub fn run_set<T: EnvDProfile>(
    globals: &Cli,
    target: &str,
    profile: &str,
    base_dir: &Utf8PathBuf,
) -> Result<()> {
    let roots = globals.outer_roots();
    let profiles_by_target = list_all_profiles::<T>(&roots)?;

    let resolved_profile = if let Ok(n) = profile.parse::<usize>() {
        let mut all_profiles: Vec<&Profile<T>> = profiles_by_target
            .values()
            .flat_map(|profiles| profiles.iter())
            .collect();
        all_profiles.sort_by(|a, b| compare_profile_names(&a.name, &b.name));

        let idx = n.checked_sub(1).context("profile numbers start at 1")?;
        all_profiles
            .get(idx)
            .with_context(|| {
                format!(
                    "profile number {} out of range (1..={})",
                    n,
                    all_profiles.len()
                )
            })?
            .name
            .clone()
    } else {
        profile.to_string()
    };

    set_profile::<T>(&roots, target, &resolved_profile, base_dir)?;
    println!(">>> {} profile set: {}", T::module_name(), resolved_profile);
    println!("    for target: {}", target);

    Ok(())
}

/// Activate the newest profile installed *in this root* for `target` — used by
/// `crossdev --setup` to run `binutils-config`/`gcc-config` against the prefix
/// (the eclass `pkg_postinst` runs the host's, which targets `/`). Returns
/// `false` if no profile is installed yet (build not merged). Host profiles
/// (prefix context) are ignored — only this root's freshly built one.
pub(super) fn activate_latest<T: EnvDProfile>(roots: &Roots, target: &str) -> Result<bool> {
    let profiles_by_target = list_all_profiles::<T>(roots)?;
    let Some(profile) = profiles_by_target.get(target).and_then(|list| {
        list.iter()
            .filter(|p| !p.is_host)
            .max_by(|a, b| compare_profile_names(&a.name, &b.name))
    }) else {
        return Ok(false);
    };
    let base_dir = env_d_dir::<T>(roots);
    set_profile::<T>(roots, target, &profile.name, &base_dir)?;
    Ok(true)
}

/// Get the default target from CHOST or architecture.
pub fn get_default_target(globals: &Cli) -> String {
    get_chost(globals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::linker::LinkerProfileType;

    /// A profile file whose target line's value is a single stray quote
    /// character used to panic (`target[1..target.len() - 1]` sliced
    /// `1..0`) in the old hand-rolled quote-stripping loop here, before it
    /// was rewritten to go through `parse_env_vars` like `set_profile`
    /// already did.
    #[test]
    fn collect_profiles_does_not_panic_on_a_bare_quote_value() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("aarch64-unknown-linux-gnu-13"),
            "CTARGET=\"\n",
        )
        .unwrap();
        let base_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        let mut profiles_by_target = BTreeMap::new();
        collect_profiles::<LinkerProfileType>(&base_dir, &mut profiles_by_target, false).unwrap();

        // The point of this test is that the malformed CTARGET value
        // doesn't panic while being parsed -- the profile still gets
        // collected under whatever (malformed) target string comes out.
        assert_eq!(profiles_by_target.values().map(Vec::len).sum::<usize>(), 1);
    }

    /// `activate_latest`/the list-display sorts used to pick "newest" by
    /// plain `str::cmp`, which prefers `"...-9"` over `"...-13"` byte-wise
    /// (`'9' > '1'`) -- a real bug for GCC once a build spans a single- to
    /// double-digit major version bump (a documented bootstrap scenario in
    /// this codebase: an older bootstrap compiler and the final one can
    /// coexist in the same root). Fixed by comparing the trailing version
    /// with `portage_atom::Version` instead.
    #[test]
    fn compare_profile_names_is_version_aware_across_a_digit_boundary() {
        use std::cmp::Ordering;
        let a = "aarch64-unknown-linux-gnu-9";
        let b = "aarch64-unknown-linux-gnu-13";
        assert_eq!(compare_profile_names(a, b), Ordering::Less);
        assert_eq!(compare_profile_names(b, a), Ordering::Greater);
        // A naive string compare would have gotten this backwards.
        assert_eq!(a.cmp(b), Ordering::Greater);
    }

    #[test]
    fn activate_latest_picks_the_higher_major_not_the_lexicographic_max() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "aarch64-unknown-linux-gnu-9",
            "aarch64-unknown-linux-gnu-13",
        ] {
            std::fs::write(
                dir.path().join(name),
                "CTARGET=\"aarch64-unknown-linux-gnu\"\n",
            )
            .unwrap();
        }
        let base_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        let mut profiles_by_target = BTreeMap::new();
        collect_profiles::<LinkerProfileType>(&base_dir, &mut profiles_by_target, false).unwrap();

        let profile = profiles_by_target
            .get("aarch64-unknown-linux-gnu")
            .and_then(|list| {
                list.iter()
                    .filter(|p| !p.is_host)
                    .max_by(|a, b| compare_profile_names(&a.name, &b.name))
            })
            .unwrap();
        assert_eq!(profile.name, "aarch64-unknown-linux-gnu-13");
    }
}
