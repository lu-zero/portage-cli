use std::collections::{BTreeMap, HashMap};

use camino::Utf8Path;
use portage_atom::Dep;
use portage_atom::interner::Interned;
use portage_atom_pubgrub::{ProfileUseNode, UseLayer, UseOverride};
use portage_repo::{AcceptSet, LicenseGroupRegistry, MakeConf, ProfileStack, Repository};

use crate::force_mask::{ForceMask, ForceMaskLayer, fold_signed, index_by_cpn};
use crate::repo::AcceptToken;

type Result<T> = anyhow::Result<T>;

/// Resolved USE environment for the solver and display
pub struct UseEnv {
    /// Folded profile `make.defaults` (Portage's `defaults` layer, minus
    /// per-package `package.use`)
    ///
    /// Shared Arc base for every CPV.
    pub defaults: UseLayer,
    /// `make.conf` / extra_confs USE delta (Portage's `conf` layer)
    pub conf: UseLayer,
    /// Process-environment USE layer (`ResolvedUse::env_use`), **parsed once**
    ///
    /// Portage's `env` layer, folded in *after* `package_use`. A `-*` here
    /// wipes `package_use`; a conf-level `-*` must not.
    pub env_use: UseLayer,
    /// Keys from `USE_EXPAND` — used to group expanded flags in display
    pub expand: Vec<String>,
    /// Keys from `USE_EXPAND_HIDDEN` — groups to suppress in display
    pub expand_hidden: Vec<String>,
    /// Per-package USE overrides from `/etc/portage/package.use` and
    /// `package.env` — portage's `pkg` layer, above `conf`/make.conf
    pub package_use: Vec<(Dep, Vec<UseOverride>)>,
    /// Per-profile-node `package.use` plus that node's make.defaults flags
    ///
    /// Parent first — Portage `setcpv` interleave inside `defaults`.
    pub profile_package_use: Vec<ProfileUseNode>,
    /// Masked packages: repo-global `profiles/package.mask`, the profile
    /// stack, and `/etc/portage/package.mask`
    pub package_mask: Vec<Dep>,
    /// Site unmasks from `/etc/portage/package.unmask` — a matching entry
    /// cancels any mask for that package
    pub package_unmask: Vec<Dep>,
    /// Profile USE force/mask policy (global + per-package + stable variants),
    /// applied per package to effective USE and consulted by the Level-C cede gate
    pub force_mask: ForceMask,
    /// Effective global `ACCEPT_KEYWORDS`, parsed to interned tokens
    pub accept_keywords: Vec<AcceptToken>,
    /// Per-package `package.accept_keywords` (and legacy `package.keywords`) entries
    ///
    /// `(atom, [tokens])`, tokens interned.
    ///
    /// A bare atom carries an empty token list (expanded to `~arch` when the host
    /// arch is known).
    pub package_accept_keywords: Vec<(Dep, Vec<AcceptToken>)>,
    /// Effective global `ACCEPT_LICENSE` after `@GROUP` expansion and `-` denials
    pub accept_license: AcceptSet,
    /// Per-package `package.license` entries: `(atom, overlay)`, each overlay
    /// already parsed/expanded against the license groups
    pub package_license: Vec<(Dep, AcceptSet)>,
    /// Effective global `ACCEPT_PROPERTIES`
    ///
    /// Same token grammar as `ACCEPT_LICENSE` minus `@GROUP`
    /// (`AcceptSet::from_tokens_plain`).
    pub accept_properties: AcceptSet,
    /// Per-package `package.properties` entries — see `package_license`
    pub package_properties: Vec<(Dep, AcceptSet)>,
    /// Effective global `ACCEPT_RESTRICT`. See `accept_properties`
    pub accept_restrict: AcceptSet,
    /// Per-package `package.accept_restrict` entries — see `package_license`
    pub package_restrict: Vec<(Dep, AcceptSet)>,
    /// Resolved `DISTDIR` (where fetched distfiles live), for download-size accounting
    pub distdir: String,
    /// `package.provided` CPVs from the profile stack
    ///
    /// Packages the system supplies externally (e.g. a host interpreter in
    /// a Gentoo Prefix).
    ///
    /// They satisfy matching deps and are never built or shown for merge.
    pub provided: Vec<portage_atom::Cpv>,
}

/// In-memory override for a `--target` sysroot whose on-disk config hasn't
/// been written yet (e.g. `em crossdev --setup -p`/an unconfirmed `-a`, on a
/// target `--init-target` would create in the same run). Lets policy
/// resolution proceed without the disk writes `-p` deliberately skips.
#[derive(Clone, Copy)]
pub struct SysrootOverride<'a> {
    /// Real, already-existing profile directory the not-yet-written
    /// `make.profile` symlink would point at.
    pub profile_dir: &'a Utf8Path,
    /// The `make.conf` content the not-yet-written file would contain,
    /// sourced directly (see [`portage_repo::ConfSource::Str`]).
    pub make_conf: &'a str,
}

/// Read the config/profile/environment sources (profile stack, `make.conf`,
/// `package.use`/`.mask`/`.unmask`/`.license`/`.accept_keywords`, USE force/
/// mask) into a resolved [`UseEnv`], the shared input every per-package
/// policy fold in this crate runs on
pub async fn build_use_env(
    repo: &Repository,
    root: Option<&Utf8Path>,
    config_overlay: Option<&Utf8Path>,
    extra_use_override: Option<&str>,
    sysroot_override: Option<&SysrootOverride<'_>>,
) -> Result<UseEnv> {
    compute_use_env(
        repo,
        root,
        config_overlay,
        extra_use_override,
        sysroot_override,
    )
    .await
}

async fn compute_use_env(
    repo: &Repository,
    root: Option<&Utf8Path>,
    config_overlay: Option<&Utf8Path>,
    extra_use_override: Option<&str>,
    sysroot_override: Option<&SysrootOverride<'_>>,
) -> Result<UseEnv> {
    let portage_dir = root.unwrap_or(Utf8Path::new("/")).join("etc/portage");
    let root_dir = root.unwrap_or(Utf8Path::new("/"));

    // A `--target` sysroot's `make.profile` always symlinks directly to a
    // real, already-existing `::gentoo` profile directory (no em-generated
    // content of its own) — under a still-pending `sysroot_override`, use
    // that real directory straight, skipping the not-yet-written symlink.
    let profile_path = match sysroot_override {
        Some(ov) => ov.profile_dir.as_std_path().to_path_buf(),
        None => {
            let profile_link = portage_dir.join("make.profile");
            std::fs::canonicalize(profile_link.as_std_path())
                .map_err(|e| anyhow::anyhow!("cannot resolve {profile_link}: {e}"))?
        }
    };
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

    // make.conf(5): path may be a file or a directory of Flat fragments.
    // Portage sources legacy `/etc/make.conf` first, then `/etc/portage/make.conf`
    // so the latter overrides (config.py make_conf_paths order). A missing path
    // (the pending-sysroot case) lists as empty, not an error.
    let conf_owned = portage_repo::expand_make_conf_paths([
        root_dir.join("etc/make.conf"),
        root_dir.join("etc/portage/make.conf"),
    ])
    .map_err(|e| anyhow::anyhow!("listing make.conf: {e}"))?;
    let mut confs: Vec<portage_repo::ConfSource> = conf_owned
        .iter()
        .map(|p| portage_repo::ConfSource::File(p.as_path()))
        .collect();
    if let Some(ov) = sysroot_override {
        confs.push(portage_repo::ConfSource::Str(ov.make_conf));
    }

    let resolved = match extra_use_override {
        Some(content) => stack
            .use_flags_with_override(&mut shell, &confs, content)
            .await
            .map_err(|e| anyhow::anyhow!("failed to evaluate USE flags: {e}"))?,
        None => stack
            .use_flags(&mut shell, &confs)
            .await
            .map_err(|e| anyhow::anyhow!("failed to evaluate USE flags: {e}"))?,
    };

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
    let accept_license = AcceptSet::from_tokens(&accept_license_tokens, &license_groups);

    // Per-package `package.license`, in portage's precedence order: profile
    // stack, then site `/etc/portage/package.license`, then the config overlay.
    // Each line's tokens are expanded into an `AcceptSet` overlay now.
    let mut package_license: Vec<(Dep, AcceptSet)> = stack
        .package_license()
        .unwrap_or_default()
        .into_iter()
        .map(|(dep, toks)| (dep, AcceptSet::from_tokens(&toks, &license_groups)))
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

    // ACCEPT_PROPERTIES / ACCEPT_RESTRICT: same shape as ACCEPT_LICENSE minus
    // @GROUP support (no license-group registry to load or pass), process env
    // overrides the profile/make.conf value, default "*" (accept everything —
    // portage's make.globals default, why these gates almost never mask).
    let no_groups = LicenseGroupRegistry::default();
    let accept_properties_tokens = match std::env::var("ACCEPT_PROPERTIES") {
        Ok(env) if !env.is_empty() => env.split_whitespace().map(str::to_string).collect(),
        _ => {
            let v = split_var("ACCEPT_PROPERTIES");
            if v.is_empty() {
                vec!["*".to_string()]
            } else {
                v
            }
        }
    };
    let accept_properties = AcceptSet::from_tokens_plain(&accept_properties_tokens);
    let mut package_properties: Vec<(Dep, AcceptSet)> =
        load_package_license(portage_dir.join("package.properties").as_str(), &no_groups);
    if let Some(overlay) = config_overlay {
        package_properties.extend(load_package_license(
            overlay.join("package.properties").as_str(),
            &no_groups,
        ));
    }

    let accept_restrict_tokens = match std::env::var("ACCEPT_RESTRICT") {
        Ok(env) if !env.is_empty() => env.split_whitespace().map(str::to_string).collect(),
        _ => {
            let v = split_var("ACCEPT_RESTRICT");
            if v.is_empty() {
                vec!["*".to_string()]
            } else {
                v
            }
        }
    };
    let accept_restrict = AcceptSet::from_tokens_plain(&accept_restrict_tokens);
    let mut package_restrict: Vec<(Dep, AcceptSet)> = load_package_license(
        portage_dir.join("package.accept_restrict").as_str(),
        &no_groups,
    );
    if let Some(overlay) = config_overlay {
        package_restrict.extend(load_package_license(
            overlay.join("package.accept_restrict").as_str(),
            &no_groups,
        ));
    }

    let distdir = shell
        .get_var("DISTDIR")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/var/cache/distfiles".to_string());

    // Profile/make.conf fold and process-env USE, tokenized once here so
    // every per-package `resolve_effective_use` reuses interned layer tokens
    // (USE_EXPAND can put 100+ flags into pre_env — re-splitting that string
    // per CPV is pure waste). Kept as two layers: a `-*` in env must still
    // wipe package.use while a conf-level `-*` must not. Each layer also
    // carries the USE_EXPAND groups it explicitly assigned, so the per-package
    // fold can apply portage's non-incremental group replace to the ebuild's
    // own `+`-defaulted IUSE — `L10N="en-US"` in make.conf must beat
    // chromium-2.eclass's `IUSE="+l10n_${lang}"` for every bundled locale.
    let defaults = UseLayer::parse(&resolved.defaults_use);
    let conf = UseLayer::parse(&resolved.conf_use).with_group_clears(resolved.conf_expand_assigned);
    let env_use =
        UseLayer::parse(&resolved.env_use).with_group_clears(resolved.env_expand_assigned);

    // Profile `package.use` is inside Portage's `defaults` layer, after that
    // node's make.defaults and below make.conf. Per-node so a child
    // make.defaults can restate a flag a parent package.use turned off.
    let expand_keys = &expand;
    let expand_values = |key: &str| split_var(key);
    let expand_flags = |raw: Vec<(Dep, Vec<String>)>| {
        raw.into_iter()
            .map(|(dep, flags)| {
                (
                    dep,
                    expand_use_expand_colon(&flags, expand_keys, &expand_values),
                )
            })
            .collect::<Vec<_>>()
    };
    let mut defaults_by_path: HashMap<std::path::PathBuf, String> =
        resolved.profile_defaults.into_iter().collect();
    let profile_package_use: Vec<ProfileUseNode> = stack
        .profiles()
        .iter()
        .map(|p| {
            let md = p.path().join("make.defaults");
            let layer_defaults = defaults_by_path
                .remove(&md)
                .map(|s| UseLayer::parse(&s))
                .unwrap_or_else(UseLayer::empty);
            let raw = p.package_use().unwrap_or_default();
            let puse_clear_all = raw
                .iter()
                .any(|(_, flags)| flags_have_use_clear_all(flags, expand_keys));
            ProfileUseNode {
                defaults: layer_defaults,
                package_use: expand_flags(raw),
                puse_clear_all,
            }
        })
        .collect();
    let mut raw_package_use: Vec<(Dep, Vec<String>)> =
        load_package_use(portage_dir.join("package.use").as_str());
    if let Some(overlay) = config_overlay {
        raw_package_use.extend(load_package_use(overlay.join("package.use").as_str()));
    }
    let mut package_use: Vec<(Dep, Vec<UseOverride>)> = expand_flags(raw_package_use);

    // `/etc/portage/package.env`'s own USE contribution, folded in as more
    // package_use-style overrides (same `Dep`-keyed shape, matched the same
    // way by `resolve_effective_use`'s `pkg_use_tokens` fold) rather than a
    // new tier — its USE sits at the same precedence as plain
    // `package.use`, just applied after it (grouped by config directory:
    // host package.use, host package.env, then overlay of each — real
    // portage doesn't interleave config layers either). Previously
    // build-time only (`-p` could show flags the actual build didn't use).
    package_use.extend(load_package_env_use(&portage_dir).await);
    if let Some(overlay) = config_overlay {
        package_use.extend(load_package_env_use(overlay).await);
    }

    // Mask sources, in portage's order: the repo-global `profiles/package.mask`
    // (applies regardless of profile), the profile chain (with `-atom` removals
    // already resolved), and the site `/etc/portage/package.mask`. A mask is
    // overridden per package by `/etc/portage/package.unmask`.
    let mut package_mask = repo.repo_package_mask().unwrap_or_default();
    package_mask.extend(stack.package_mask().unwrap_or_default());
    package_mask.extend(load_dep_list(portage_dir.join("package.mask").as_str()));
    let package_unmask = load_dep_list(portage_dir.join("package.unmask").as_str());

    // `package.provided` — CPVs the system supplies externally (profile stack,
    // incl. the folded-in `/etc/portage/profile`). Fed to the solver as
    // dependency-satisfying, never-built packages.
    let provided = stack.package_provided().unwrap_or_default();

    // Profile USE force/mask. Both global use.force/use.mask are applied per
    // package by `ForceMask::apply` (force_mask.rs) — global use.force isn't
    // folded into `pre_env` (unlike the pre-2026-07-12 collapsed `config`),
    // so it must be applied here alongside use.mask, not assumed already
    // baked into the base state. The package-level and *.stable.* sets are
    // also per package; they carry raw `-flag` tokens so unforce/unmask is
    // resolved per package.
    let use_expand_implicit = split_var("USE_EXPAND_IMPLICIT");
    let use_expand_unprefixed = split_var("USE_EXPAND_UNPREFIXED");
    let mut expand_values = HashMap::new();
    for v in expand.iter().chain(use_expand_unprefixed.iter()) {
        let vals = split_var(&format!("USE_EXPAND_VALUES_{v}"));
        if !vals.is_empty() {
            expand_values.insert(v.clone(), vals);
        }
    }
    let force_mask = ForceMask {
        iuse_injection: crate::force_mask::IuseInjection::new(
            split_var("IUSE_IMPLICIT"),
            expand.clone(),
            use_expand_implicit,
            use_expand_unprefixed,
            expand_values,
        ),
        layers: stack
            .profiles()
            .iter()
            .map(|p| ForceMaskLayer {
                use_force: fold_signed(p.use_force().unwrap_or_default()),
                use_mask: fold_signed(p.use_mask().unwrap_or_default()),
                use_stable_force: fold_signed(p.use_stable_force().unwrap_or_default()),
                use_stable_mask: fold_signed(p.use_stable_mask().unwrap_or_default()),
                pkg_force: index_by_cpn(p.package_use_force().unwrap_or_default()),
                pkg_mask: index_by_cpn(p.package_use_mask().unwrap_or_default()),
                pkg_stable_force: index_by_cpn(p.package_use_stable_force().unwrap_or_default()),
                pkg_stable_mask: index_by_cpn(p.package_use_stable_mask().unwrap_or_default()),
            })
            .collect(),
    };

    Ok(UseEnv {
        defaults,
        conf,
        env_use,
        expand,
        expand_hidden,
        package_use,
        profile_package_use,
        package_mask,
        package_unmask,
        force_mask,
        accept_keywords,
        package_accept_keywords,
        accept_license,
        package_license,
        accept_properties,
        package_properties,
        accept_restrict,
        package_restrict,
        distdir,
        provided,
    })
}

/// `make.conf` often sets `ACCEPT_KEYWORDS="${ARCH} ~${ARCH}"`
///
/// When `ARCH` is not yet visible at source time, brush leaves `~` only —
/// rebuild from `ARCH` once the profile stack has settled.
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

/// Load `package.use` as raw `(atom, [token])` lines
///
/// `#` comments, optionally a directory (children summed in lexical
/// order).
///
/// Tokens stay verbatim — USE_EXPAND `KEY:` groups are expanded later by
/// [`expand_use_expand_colon`].
fn load_package_use(path: &str) -> Vec<(Dep, Vec<String>)> {
    let mut result = Vec::new();
    for line in portage_repo::read_config_lines(path).unwrap_or_default() {
        let mut parts = line.split_whitespace();
        let Some(atom_str) = parts.next() else {
            continue;
        };
        let Ok(dep) = Dep::parse(atom_str) else {
            continue;
        };
        let flags: Vec<String> = parts.map(str::to_string).collect();
        if !flags.is_empty() {
            result.push((dep, flags));
        }
    }
    result
}

/// `/etc/portage/package.env`'s own USE contribution, as `package_use`-shaped
/// overrides: for each matched atom, source its env files (in line order,
/// each on top of the last, seeded empty) via a real shell via
/// [`MakeConf::apply_to`] — so `USE="${USE} -flag"` self-reference
/// evaluates correctly — and take the resulting tokens as this atom's
/// override list, [`UseOverride::parse`]d like a `package.use` line
///
/// Seeded empty (not the profile's USE) because this collects *this atom's
/// own* contribution, independent of any candidate's baseline — that folds
/// in later, per candidate, by `resolve_effective_use`. An unrelated
/// variable unset here expands empty rather than the real build env's
/// value — same approximation `binpkg::DesiredBuildEnv::key_for` makes,
/// never worse than "resolver ignores package.env USE" today.
async fn load_package_env_use(portage_dir: &Utf8Path) -> Vec<(Dep, Vec<UseOverride>)> {
    let entries =
        portage_repo::package_env::load_package_env(portage_dir.join("package.env").as_std_path());
    let env_dir = portage_dir.join("env");
    let mut out = Vec::new();
    for (dep, names) in entries {
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        env.insert("USE".to_string(), String::new());
        for name in &names {
            if let Ok(mc) = MakeConf::load(&env_dir.join(name)) {
                let _ = mc.apply_to(&mut env).await;
            }
        }
        let tokens: Vec<UseOverride> = env
            .get("USE")
            .map(|use_str| use_str.split_whitespace().map(UseOverride::parse).collect())
            .unwrap_or_default();
        if !tokens.is_empty() {
            out.push((dep, tokens));
        }
    }
    out
}

/// Expand the `USE_EXPAND:` colon form in `package.use` tokens to interned overrides
///
/// A USE_EXPAND name followed by `:` makes every subsequent value a member
/// of that group, e.g. `cat/pkg L10N: de en` ⇒ `l10n_de l10n_en`. A bare
/// `-*` inside a group clears its live values before the trailing values
/// rebuild it (`PYTHON_TARGETS: -* python2_7` ⇒ only
/// `python_targets_python2_7`).
///
/// Only keys present in `use_expand` start a group; any other token — including
/// one that merely ends in `:` — is parsed as an ordinary flag, so plain flags
/// and a bare `-*` keep working.
/// A bare `-*` (not inside a `KEY:` group) is a whole-USE clear-all.
fn flags_have_use_clear_all(flags: &[String], use_expand: &[String]) -> bool {
    let mut in_group = false;
    for tok in flags {
        if let Some(key) = tok.strip_suffix(':')
            && use_expand.iter().any(|k| k == key)
        {
            in_group = true;
            continue;
        }
        if !in_group && tok == "-*" {
            return true;
        }
    }
    false
}

fn expand_use_expand_colon(
    tokens: &[String],
    use_expand: &[String],
    expand_values: &dyn Fn(&str) -> Vec<String>,
) -> Vec<UseOverride> {
    let mut out: Vec<UseOverride> = Vec::with_capacity(tokens.len());
    let mut group: Option<&str> = None;
    for tok in tokens {
        if let Some(key) = tok.strip_suffix(':')
            && use_expand.iter().any(|k| k == key)
        {
            group = Some(key);
            continue;
        }
        let Some(key) = group else {
            out.push(UseOverride::parse(tok));
            continue;
        };
        let prefix = key.to_lowercase();
        if tok == "-*" {
            // Clear the group's live values; the tokens after rebuild it.
            for v in expand_values(key) {
                out.push(UseOverride {
                    flag: Interned::intern(&format!("{prefix}_{v}")),
                    enable: false,
                });
            }
        } else {
            let (enable, val) = match tok.strip_prefix('-') {
                Some(v) => (false, v),
                None => (true, tok.as_str()),
            };
            out.push(UseOverride {
                flag: Interned::intern(&format!("{prefix}_{val}")),
                enable,
            });
        }
    }
    out
}

/// Load `package.accept_keywords` / `package.keywords`
///
/// `(atom, [tokens])` per line, `#` comments, optionally a directory.
///
/// Tokens are parsed to interned [`AcceptToken`]s at read time. Unlike
/// [`load_package_use`], a bare atom (no tokens) is *kept* with an empty token
/// list — portage reads it as "accept this package's `~arch`" (expanded once
/// the host arch is known).
fn load_package_keywords(path: &str) -> Vec<(Dep, Vec<AcceptToken>)> {
    let mut result = Vec::new();
    for line in portage_repo::read_config_lines(path).unwrap_or_default() {
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
    result
}

/// Load `package.license`: `(atom, overlay)` per line, `#` comments, optionally a directory
///
/// Each line's license tokens (`@GROUP`, `-deny`, `*`, names) are expanded
/// against `groups` into a per-package [`AcceptSet`] overlay now, so resolution
/// never re-parses them.
fn load_package_license(path: &str, groups: &LicenseGroupRegistry) -> Vec<(Dep, AcceptSet)> {
    let mut result = Vec::new();
    for line in portage_repo::read_config_lines(path).unwrap_or_default() {
        let mut parts = line.split_whitespace();
        let Some(atom_str) = parts.next() else {
            continue;
        };
        let Ok(dep) = Dep::parse(atom_str) else {
            continue;
        };
        let tokens: Vec<String> = parts.map(String::from).collect();
        result.push((dep, AcceptSet::from_tokens(&tokens, groups)));
    }
    result
}

/// Load a simple atom list (one dep per line, `#` comments, optionally a directory)
fn load_dep_list(path: &str) -> Vec<Dep> {
    let mut result = Vec::new();
    for line in portage_repo::read_config_lines(path).unwrap_or_default() {
        // Strip leading '-' for incremental removal entries — skip removals here
        if line.starts_with('-') {
            continue;
        }
        if let Ok(dep) = Dep::parse(&line) {
            result.push(dep);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{
        SysrootOverride, build_use_env, expand_use_expand_colon, flags_have_use_clear_all,
        load_package_env_use, load_package_keywords, load_package_use,
    };
    use portage_atom::Dep;
    use portage_atom_pubgrub::UseOverride;

    fn make_test_repo(dir: &std::path::Path) -> portage_repo::Repository {
        std::fs::create_dir_all(dir.join("metadata")).unwrap();
        std::fs::write(dir.join("metadata/layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.join("profiles")).unwrap();
        portage_repo::Repository::builder()
            .in_memory_cache()
            .open(dir)
            .unwrap()
    }

    // Regression: `em crossdev --setup -p` on a target that has never been
    // `--init-target`'d for real has no `etc/portage/make.profile`/
    // `make.conf` on disk yet. Without a `SysrootOverride`, resolving
    // against that root must fail exactly the way it used to (proving this
    // is a real repro, not a vacuous pass); with one, it must succeed using
    // the real profile dir plus the synthesized make.conf content.
    #[tokio::test]
    async fn sysroot_override_lets_a_cold_target_resolve() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(repo_dir.path());
        let profile_dir = repo_dir.path().join("test-profile");
        std::fs::create_dir_all(&profile_dir).unwrap();
        std::fs::write(profile_dir.join("make.defaults"), "").unwrap();
        let profile_dir = camino::Utf8Path::from_path(&profile_dir).unwrap();

        // A sysroot that has never been `--init-target`'d: no `etc/portage/*`
        // at all under `root`.
        let sysroot = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(sysroot.path()).unwrap();

        match build_use_env(&repo, Some(root), None, None, None).await {
            Ok(_) => {
                panic!("a cold, never-initialized target must not resolve without an override")
            }
            Err(e) => assert!(
                e.to_string().contains("cannot resolve"),
                "expected the make.profile canonicalize failure, got: {e}"
            ),
        }

        let ov = SysrootOverride {
            profile_dir,
            make_conf: "CHOST=\"riscv64-unknown-linux-gnu\"\n",
        };
        build_use_env(&repo, Some(root), None, None, Some(&ov))
            .await
            .expect("a SysrootOverride must let a never-initialized target resolve");
    }

    // Expected override shorthand
    fn ov(s: &str) -> UseOverride {
        UseOverride::parse(s)
    }

    // Build an `etc/portage` dir with a `package.env` and named env files,
    // matching `portage_repo::package_env`'s own test fixture shape
    fn portage_dir(package_env: &str, env_files: &[(&str, &str)]) -> tempfile::TempDir {
        let td = tempfile::TempDir::new().unwrap();
        let pd = td.path();
        std::fs::write(pd.join("package.env"), package_env).unwrap();
        std::fs::create_dir_all(pd.join("env")).unwrap();
        for (name, body) in env_files {
            std::fs::write(pd.join("env").join(name), body).unwrap();
        }
        td
    }

    #[tokio::test]
    async fn package_env_use_becomes_package_use_shaped_overrides() {
        let td = portage_dir(
            "dev-libs/foo  ccache-workaround\n",
            &[("ccache-workaround", "USE=\"${USE} -jit ccache\"\n")],
        );
        let dir = camino::Utf8Path::from_path(td.path()).unwrap();
        let out = load_package_env_use(dir).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, Dep::parse("dev-libs/foo").unwrap());
        assert_eq!(out[0].1, vec![ov("-jit"), ov("ccache")]);
    }

    #[tokio::test]
    async fn package_env_use_chains_multiple_files_in_line_order() {
        let td = portage_dir(
            "dev-libs/foo  first second\n",
            &[
                ("first", "USE=\"${USE} a\"\n"),
                ("second", "USE=\"${USE} b\"\n"),
            ],
        );
        let dir = camino::Utf8Path::from_path(td.path()).unwrap();
        let out = load_package_env_use(dir).await;
        assert_eq!(out[0].1, vec![ov("a"), ov("b")]);
    }

    #[tokio::test]
    async fn package_env_without_use_assignment_yields_no_override() {
        // An env file that only touches build vars (CFLAGS/FEATURES/etc, the
        // already-landed build-env slice) shouldn't manufacture a USE entry.
        let td = portage_dir(
            "dev-libs/foo  flags-only\n",
            &[("flags-only", "CFLAGS=\"-O3\"\n")],
        );
        let dir = camino::Utf8Path::from_path(td.path()).unwrap();
        let out = load_package_env_use(dir).await;
        assert!(out.is_empty());
    }

    #[test]
    fn colon_form_expands_values() {
        // `cat/pkg L10N: de en` ⇒ l10n_de l10n_en; a plain flag is untouched.
        let keys = ["L10N".to_string()];
        let none = |_: &str| Vec::new();
        let out = expand_use_expand_colon(
            &["foo".into(), "L10N:".into(), "de".into(), "en".into()],
            &keys,
            &none,
        );
        assert_eq!(out, vec![ov("foo"), ov("l10n_de"), ov("l10n_en")]);
    }

    #[test]
    fn colon_form_negates_value() {
        let keys = ["L10N".to_string()];
        let none = |_: &str| Vec::new();
        let out = expand_use_expand_colon(&["L10N:".into(), "-de".into()], &keys, &none);
        assert_eq!(out, vec![ov("-l10n_de")]);
    }

    #[test]
    fn colon_form_dash_star_clears_group_defaults() {
        // `PYTHON_TARGETS: -* python2_7` clears the live defaults (python3_13)
        // then adds python2_7 — the documented "pin a single impl" pattern.
        let keys = ["PYTHON_TARGETS".to_string()];
        let defaults = |k: &str| {
            if k == "PYTHON_TARGETS" {
                vec!["python3_13".to_string()]
            } else {
                Default::default()
            }
        };
        let out = expand_use_expand_colon(
            &["PYTHON_TARGETS:".into(), "-*".into(), "python2_7".into()],
            &keys,
            &defaults,
        );
        assert_eq!(
            out,
            vec![
                ov("-python_targets_python3_13"),
                ov("python_targets_python2_7")
            ]
        );
    }

    #[test]
    fn colon_form_switches_group_mid_line() {
        // A new `KEY:` switches the active group; both groups expand.
        let keys = ["L10N".to_string(), "PYTHON_TARGETS".to_string()];
        let none = |_: &str| Vec::new();
        let out = expand_use_expand_colon(
            &[
                "L10N:".into(),
                "de".into(),
                "PYTHON_TARGETS:".into(),
                "python3_13".into(),
            ],
            &keys,
            &none,
        );
        assert_eq!(out, vec![ov("l10n_de"), ov("python_targets_python3_13")]);
    }

    #[test]
    fn colon_form_ignores_unknown_key() {
        // A `:` suffix on something that isn't a USE_EXPAND key is parsed as a
        // plain flag (left intact, interned verbatim).
        let keys = ["L10N".to_string()];
        let none = |_: &str| Vec::new();
        let out = expand_use_expand_colon(&["WEIRD:".into(), "x".into()], &keys, &none);
        assert_eq!(out, vec![ov("WEIRD:"), ov("x")]);
    }

    #[test]
    fn colon_form_passes_through_plain_flags() {
        let keys = ["L10N".to_string()];
        let none = |_: &str| Vec::new();
        let out = expand_use_expand_colon(&["nls".into(), "-debug".into()], &keys, &none);
        assert_eq!(out, vec![ov("nls"), ov("-debug")]);
    }

    #[test]
    fn bare_star_is_a_use_clear_all_colon_star_is_not() {
        let keys = ["PYTHON_TARGETS".to_string()];
        assert!(flags_have_use_clear_all(
            &["-*".into(), "build".into()],
            &keys
        ));
        assert!(!flags_have_use_clear_all(
            &["PYTHON_TARGETS:".into(), "-*".into(), "python3_13".into()],
            &keys
        ));
    }

    // PMS 5.2.4 dir-form: a `/etc/portage/package.use` *directory*'s regular files are concatenated in filename order — and, matching real portage's `_recursive_basename_filter`, dotfiles and `~` editor backups are skipped
    //
    // Before the shared-`read_config_lines` fix these loaders read every regular
    // file, so a stray `.swp`/`foo~` that happened to parse as `atom flag` leaked
    // in as real data that real portage ignores.
    #[test]
    fn package_use_dir_skips_dotfiles_and_backups() {
        let td = tempfile::TempDir::new().unwrap();
        let dir = td.path().join("package.use");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("10-a"), "dev-libs/a first\n").unwrap();
        std::fs::write(dir.join("20-b"), "# c\ndev-libs/b second\n").unwrap();
        // Both must be ignored the way real portage ignores them.
        std::fs::write(dir.join(".hidden"), "dev-libs/hidden ghost\n").unwrap();
        std::fs::write(dir.join("30-b~"), "dev-libs/backup ghost\n").unwrap();

        let out = load_package_use(dir.to_str().unwrap());
        let atoms: Vec<_> = out.iter().map(|(d, _)| d.to_string()).collect();
        assert_eq!(atoms, vec!["dev-libs/a", "dev-libs/b"]);
        // Filename order preserved across the concatenation.
        assert_eq!(out[0].1, vec!["first".to_string()]);
        assert_eq!(out[1].1, vec!["second".to_string()]);
    }

    // The same dir-form dotfile/backup skip must hold for the keyword reader,
    // which shares the reader but keeps bare atoms (empty token list)
    #[test]
    fn package_keywords_dir_skips_dotfiles_and_backups() {
        let td = tempfile::TempDir::new().unwrap();
        let dir = td.path().join("package.accept_keywords");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("10-a"), "dev-libs/a ~amd64\n").unwrap();
        std::fs::write(dir.join(".hidden"), "dev-libs/hidden ~amd64\n").unwrap();
        std::fs::write(dir.join("20-a~"), "dev-libs/backup ~amd64\n").unwrap();

        let out = load_package_keywords(dir.to_str().unwrap());
        let atoms: Vec<_> = out.iter().map(|(d, _)| d.to_string()).collect();
        assert_eq!(atoms, vec!["dev-libs/a"]);
    }
}
