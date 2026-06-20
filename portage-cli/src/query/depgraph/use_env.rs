use camino::Utf8Path;
use portage_atom::Dep;
use portage_atom::interner::Interned;
use portage_atom_pubgrub::{UseConfig, UseFlagState, UseOverride};
use portage_repo::{AcceptLicense, LicenseGroupRegistry, ProfileStack, Repository};

use super::force_mask::{ForceMask, index_by_cpn};
use super::repo::AcceptToken;

type Result<T> = anyhow::Result<T>;

/// Resolved USE environment for the solver and display.
pub(super) struct UseEnv {
    pub config: UseConfig,
    /// Keys from `USE_EXPAND` — used to group expanded flags in display.
    pub expand: Vec<String>,
    /// Keys from `USE_EXPAND_HIDDEN` — groups to suppress in display.
    pub expand_hidden: Vec<String>,
    /// Per-package USE flag overrides from the profile and `/etc/portage/package.use`.
    pub package_use: Vec<(Dep, Vec<UseOverride>)>,
    /// Masked packages: repo-global `profiles/package.mask`, the profile
    /// stack, and `/etc/portage/package.mask`.
    pub package_mask: Vec<Dep>,
    /// Site unmasks from `/etc/portage/package.unmask` — a matching entry
    /// cancels any mask for that package.
    pub package_unmask: Vec<Dep>,
    /// Profile USE force/mask policy (global + per-package + stable variants),
    /// applied per package to effective USE and consulted by the Level-C cede gate.
    pub force_mask: ForceMask,
    /// Effective global `ACCEPT_KEYWORDS`, parsed to interned tokens.
    pub accept_keywords: Vec<AcceptToken>,
    /// Per-package `package.accept_keywords` (and legacy `package.keywords`)
    /// entries: `(atom, [tokens])`, tokens interned. A bare atom carries an
    /// empty token list (expanded to `~arch` when the host arch is known).
    pub package_accept_keywords: Vec<(Dep, Vec<AcceptToken>)>,
    /// Effective global `ACCEPT_LICENSE` after `@GROUP` expansion and `-` denials.
    pub accept_license: AcceptLicense,
    /// Per-package `package.license` entries: `(atom, overlay)`, each overlay
    /// already parsed/expanded against the license groups.
    pub package_license: Vec<(Dep, AcceptLicense)>,
    /// Resolved `DISTDIR` (where fetched distfiles live), for download-size accounting.
    pub distdir: String,
}

pub(super) async fn build_use_env(
    repo: &Repository,
    root: Option<&Utf8Path>,
    config_overlay: Option<&Utf8Path>,
) -> Result<UseEnv> {
    compute_use_env(repo, root, config_overlay).await
}

async fn compute_use_env(
    repo: &Repository,
    root: Option<&Utf8Path>,
    config_overlay: Option<&Utf8Path>,
) -> Result<UseEnv> {
    let portage_dir = root.unwrap_or(Utf8Path::new("/")).join("etc/portage");
    let root_dir = root.unwrap_or(Utf8Path::new("/"));

    let profile_link = portage_dir.join("make.profile");
    let profile_path = std::fs::canonicalize(profile_link.as_std_path())
        .map_err(|e| anyhow::anyhow!("cannot resolve {profile_link}: {e}"))?;
    // Portage appends `/etc/portage/profile` as the top (highest-priority)
    // profile layer, so its use.force/use.mask/package.use*/package.mask override
    // the resolved make.profile chain. Fold it in so Level-C never cedes a flag a
    // site override pins and the plan honours site masks (portage(5)).
    let stack = ProfileStack::build(profile_path)
        .map_err(|e| anyhow::anyhow!("failed to build profile stack: {e}"))?
        .with_user_profile(portage_dir.join("profile").into_std_path_buf())
        .map_err(|e| anyhow::anyhow!("failed to load /etc/portage/profile: {e}"))?;
    let mut shell = repo
        .shell()
        .await
        .map_err(|e| anyhow::anyhow!("failed to start ebuild shell: {e}"))?;

    let make_conf_candidates = [
        root_dir.join("etc/portage/make.conf"),
        root_dir.join("etc/make.conf"),
    ];
    let confs: Vec<&std::path::Path> = make_conf_candidates
        .iter()
        .filter(|p| p.as_std_path().exists())
        .map(|p| p.as_std_path())
        .collect();

    let flags = stack
        .use_flags(&mut shell, &confs)
        .await
        .map_err(|e| anyhow::anyhow!("failed to evaluate USE flags: {e}"))?;

    let split_var = |name: &str| -> Vec<String> {
        shell
            .get_var(name)
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    };

    let expand = split_var("USE_EXPAND");
    let expand_hidden = split_var("USE_EXPAND_HIDDEN");
    let accept_keywords = effective_accept_keywords(&split_var, &shell);

    // Per-package keyword acceptance, in portage's precedence order: the profile
    // stack first, then site `/etc/portage/package.accept_keywords` (and the
    // legacy `package.keywords`), then the user config overlay last. Bare atoms
    // (no token) are preserved — they mean "accept this package's `~arch`".
    let mut package_accept_keywords: Vec<(Dep, Vec<AcceptToken>)> = stack
        .package_accept_keywords()
        .unwrap_or_default()
        .into_iter()
        .map(|(dep, toks)| {
            let parsed = toks.iter().filter_map(|t| AcceptToken::parse(t)).collect();
            (dep, parsed)
        })
        .collect();
    package_accept_keywords.extend(load_package_keywords(
        portage_dir.join("package.accept_keywords").as_str(),
    ));
    package_accept_keywords.extend(load_package_keywords(
        portage_dir.join("package.keywords").as_str(),
    ));
    if let Some(overlay) = config_overlay {
        package_accept_keywords.extend(load_package_keywords(
            overlay.join("package.accept_keywords").as_str(),
        ));
    }
    let license_groups = LicenseGroupRegistry::from_repo(repo)
        .map_err(|e| anyhow::anyhow!("failed to load license groups: {e}"))?;
    let accept_license_tokens = {
        let from_profile = {
            let v = split_var("ACCEPT_LICENSE");
            if v.is_empty() {
                vec!["*".to_string()]
            } else {
                v
            }
        };
        // Portage honours ACCEPT_LICENSE from the process environment.
        match std::env::var("ACCEPT_LICENSE") {
            Ok(env) if !env.is_empty() => env.split_whitespace().map(str::to_string).collect(),
            _ => from_profile,
        }
    };
    let accept_license = AcceptLicense::from_tokens(&accept_license_tokens, &license_groups);

    // Per-package `package.license`, in portage's precedence order: profile
    // stack, then site `/etc/portage/package.license`, then the config overlay.
    // Each line's tokens are expanded into an `AcceptLicense` overlay now.
    let mut package_license: Vec<(Dep, AcceptLicense)> = stack
        .package_license()
        .unwrap_or_default()
        .into_iter()
        .map(|(dep, toks)| (dep, AcceptLicense::from_tokens(&toks, &license_groups)))
        .collect();
    package_license.extend(load_package_license(
        portage_dir.join("package.license").as_str(),
        &license_groups,
    ));
    if let Some(overlay) = config_overlay {
        package_license.extend(load_package_license(
            overlay.join("package.license").as_str(),
            &license_groups,
        ));
    }

    let distdir = shell
        .get_var("DISTDIR")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/var/cache/distfiles".to_string());

    let mut config = UseConfig::new();
    for flag in flags {
        config.set(flag, UseFlagState::Enabled);
    }

    let mut package_use: Vec<(Dep, Vec<UseOverride>)> = stack
        .package_use()
        .unwrap_or_default()
        .into_iter()
        .map(|(dep, flags)| (dep, flags.iter().map(|f| UseOverride::parse(f)).collect()))
        .collect();
    package_use.extend(load_package_use(portage_dir.join("package.use").as_str()));
    // User config overlay (e.g. `--local`'s ~/.gentoo/etc/portage), applied
    // last so it overrides per flag — lets an unprivileged user set package.use
    // without writing the host /etc/portage.
    if let Some(overlay) = config_overlay {
        package_use.extend(load_package_use(overlay.join("package.use").as_str()));
    }

    // Mask sources, in portage's order: the repo-global `profiles/package.mask`
    // (applies regardless of profile), the profile chain (with `-atom` removals
    // already resolved), and the site `/etc/portage/package.mask`. A mask is
    // overridden per package by `/etc/portage/package.unmask`.
    let mut package_mask = repo.repo_package_mask().unwrap_or_default();
    package_mask.extend(stack.package_mask().unwrap_or_default());
    package_mask.extend(load_dep_list(portage_dir.join("package.mask").as_str()));
    let package_unmask = load_dep_list(portage_dir.join("package.unmask").as_str());

    // Profile USE force/mask. Global use.force is folded into `config` by
    // resolve_use_flags (enabled set); global use.mask must additionally be
    // applied per package (force_mask.rs) so it overrides a package's `+flag`
    // IUSE default rather than merely being absent from the enabled set. The
    // package-level and *.stable.* sets are also per package; they carry raw
    // `-flag` tokens so unforce/unmask is resolved per package.
    let intern_flags = |v: Vec<String>| v.iter().map(|s| Interned::intern(s)).collect();
    let force_mask = ForceMask {
        use_force: intern_flags(stack.use_force().unwrap_or_default()),
        use_mask: intern_flags(stack.use_mask().unwrap_or_default()),
        use_stable_force: intern_flags(stack.use_stable_force().unwrap_or_default()),
        use_stable_mask: intern_flags(stack.use_stable_mask().unwrap_or_default()),
        pkg_force: index_by_cpn(stack.package_use_force().unwrap_or_default()),
        pkg_mask: index_by_cpn(stack.package_use_mask().unwrap_or_default()),
        pkg_stable_force: index_by_cpn(stack.package_use_stable_force().unwrap_or_default()),
        pkg_stable_mask: index_by_cpn(stack.package_use_stable_mask().unwrap_or_default()),
    };

    Ok(UseEnv {
        config,
        expand,
        expand_hidden,
        package_use,
        package_mask,
        package_unmask,
        force_mask,
        accept_keywords,
        package_accept_keywords,
        accept_license,
        package_license,
        distdir,
    })
}

/// `make.conf` often sets `ACCEPT_KEYWORDS="${ARCH} ~${ARCH}"`. When `ARCH` is
/// not yet visible at source time, brush leaves `~` only — rebuild from `ARCH`
/// once the profile stack has settled.
fn effective_accept_keywords(
    split_var: &dyn Fn(&str) -> Vec<String>,
    shell: &portage_repo::EbuildShell,
) -> Vec<AcceptToken> {
    let arch = shell.get_var("ARCH").unwrap_or_default();
    let ak = split_var("ACCEPT_KEYWORDS");
    let parse = |toks: &[String]| toks.iter().filter_map(|t| AcceptToken::parse(t)).collect();
    if arch.is_empty() {
        return parse(&ak);
    }
    let testing = format!("~{arch}");
    let has_arch = ak.iter().any(|k| k == &arch || k == &testing);
    if has_arch {
        return parse(&ak);
    }
    // Broken expansion (e.g. `["~"]` or empty) — mirror portage's make.conf default.
    parse(&[arch, testing])
}

fn load_package_use(path: &str) -> Vec<(Dep, Vec<UseOverride>)> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Vec::new();
    }
    let files: Vec<_> = if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        v.sort();
        v
    } else {
        vec![p.to_path_buf()]
    };
    let mut result = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(atom_str) = parts.next() else {
                continue;
            };
            let Ok(dep) = Dep::parse(atom_str) else {
                continue;
            };
            let flags: Vec<UseOverride> = parts.map(UseOverride::parse).collect();
            if !flags.is_empty() {
                result.push((dep, flags));
            }
        }
    }
    result
}

/// Load `package.accept_keywords` / `package.keywords`: `(atom, [tokens])` per
/// line, `#` comments, optionally a directory. Tokens are parsed to interned
/// [`AcceptToken`]s at read time. Unlike [`load_package_use`], a bare atom (no
/// tokens) is *kept* with an empty token list — portage reads it as "accept
/// this package's `~arch`" (expanded once the host arch is known).
fn load_package_keywords(path: &str) -> Vec<(Dep, Vec<AcceptToken>)> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Vec::new();
    }
    let files: Vec<_> = if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        v.sort();
        v
    } else {
        vec![p.to_path_buf()]
    };
    let mut result = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(atom_str) = parts.next() else {
                continue;
            };
            let Ok(dep) = Dep::parse(atom_str) else {
                continue;
            };
            let tokens: Vec<AcceptToken> = parts.filter_map(AcceptToken::parse).collect();
            result.push((dep, tokens));
        }
    }
    result
}

/// Load `package.license`: `(atom, overlay)` per line, `#` comments, optionally a
/// directory. Each line's license tokens (`@GROUP`, `-deny`, `*`, names) are
/// expanded against `groups` into a per-package [`AcceptLicense`] overlay now,
/// so resolution never re-parses them.
fn load_package_license(path: &str, groups: &LicenseGroupRegistry) -> Vec<(Dep, AcceptLicense)> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Vec::new();
    }
    let files: Vec<_> = if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        v.sort();
        v
    } else {
        vec![p.to_path_buf()]
    };
    let mut result = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(atom_str) = parts.next() else {
                continue;
            };
            let Ok(dep) = Dep::parse(atom_str) else {
                continue;
            };
            let tokens: Vec<String> = parts.map(String::from).collect();
            result.push((dep, AcceptLicense::from_tokens(&tokens, groups)));
        }
    }
    result
}

/// Load a simple atom list (one dep per line, `#` comments, optionally a directory).
pub(super) fn load_dep_list(path: &str) -> Vec<Dep> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Vec::new();
    }
    let files: Vec<_> = if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        v.sort();
        v
    } else {
        vec![p.to_path_buf()]
    };
    let mut result = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // Strip leading '-' for incremental removal entries — skip removals here
            if line.starts_with('-') {
                continue;
            }
            if let Ok(dep) = Dep::parse(line) {
                result.push(dep);
            }
        }
    }
    result
}
