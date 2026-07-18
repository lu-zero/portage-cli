//! Shell-dependent profile operations.
//!
//! Methods that source bash files (make.defaults) or read shell state (USE
//! flags) cannot live in `repo/profile.rs` without creating an upward
//! dependency from the repo module into the build module.  They live here
//! instead and are added to `Profile` / `ProfileStack` via second impl blocks.

use std::collections::{HashMap, HashSet};

use portage_atom::interner::{DefaultInterner, Interned};

use super::shell::EbuildShell;
use crate::error::Result;
use crate::repo::profile::{
    Profile, ProfileEnv, ProfileEnvLayer, ProfileStack, UseFlags, merge_flag_lists,
    merge_flag_lists_signed,
};

/// Merge an incremental variable's layers. `USE` preserves explicit disables
/// (`-flag` kept so it can override a `+flag` IUSE default per package); every
/// other incremental var (`USE_EXPAND` values, …) drops them as before.
fn merge_use_var<'a>(var: &str, iter: impl Iterator<Item = &'a str>) -> Vec<String> {
    if var == "USE" {
        merge_flag_lists_signed(iter)
    } else {
        merge_flag_lists(iter)
    }
}

impl Profile {
    /// Source this profile's `make.defaults` into the shell, if present.
    ///
    /// See [PMS 5.2.4](https://projects.gentoo.org/pms/9/pms.html#makedefaults).
    pub async fn make_defaults(&self, shell: &mut EbuildShell) -> Result<()> {
        let path = self.path().join("make.defaults");
        if path.is_file() {
            shell.source_make_defaults(&path).await?;
        }
        Ok(())
    }
}

impl ProfileStack {
    /// Source `make.defaults` from every profile in stack order.
    ///
    /// Variables set by ancestor profiles are visible to child profiles.
    ///
    /// See [PMS 5.2.4](https://projects.gentoo.org/pms/9/pms.html#make-defaults).
    pub async fn make_defaults(&self, shell: &mut EbuildShell) -> Result<()> {
        for p in self.profiles() {
            p.make_defaults(shell).await?;
        }
        Ok(())
    }

    /// Resolve the effective USE flags for this profile stack.
    ///
    /// Sources profile `make.defaults`, extra confs (e.g. `/etc/portage/make.conf`),
    /// then applies the process-environment layer, `use.force`, and `use.mask`.
    ///
    /// Returns the enabled flags plus the flags explicitly disabled by a `-flag`
    /// USE token (see `ResolvedUse`).  The shell's bash state is updated as a
    /// side-effect (necessary for bash evaluation); the Rust-side `use_flags`
    /// HashSet is **not** set — call `configure_shell` for that.
    pub async fn use_flags(
        &self,
        shell: &mut EbuildShell,
        extra_confs: &[&std::path::Path],
    ) -> Result<ResolvedUse> {
        resolve_use_flags(shell, self, extra_confs, None).await
    }

    /// As [`use_flags`](Self::use_flags), plus a transient conf-layer USE
    /// override (see the `resolve_use_flags` function's `extra_use_override`
    /// parameter).
    pub async fn use_flags_with_override(
        &self,
        shell: &mut EbuildShell,
        extra_confs: &[&std::path::Path],
        extra_use_override: &str,
    ) -> Result<ResolvedUse> {
        resolve_use_flags(shell, self, extra_confs, Some(extra_use_override)).await
    }

    /// Build the layered profile environment by sourcing each `make.defaults`
    /// through brush with per-layer isolation.
    ///
    /// Each file is sourced in the **same** shell (preserving cross-file
    /// variable visibility for non-incremental vars like `EAPI`, computed
    /// paths, etc.).  Before each file the incremental variables (`USE`,
    /// `USE_EXPAND`, and every key listed in `USE_EXPAND`) are reset to
    /// empty so the file's own assignments are captured as a clean delta.
    /// After sourcing, the accumulated values are restored into the shell
    /// for the next file to reference via `${USE}`, `${PYTHON_TARGETS}`, etc.
    ///
    /// When this method returns the shell holds the fully-accumulated profile
    /// state, ready for `make.conf` to be sourced on top.
    pub async fn profile_env(&self, shell: &mut EbuildShell) -> Result<ProfileEnv> {
        let mut layers: Vec<ProfileEnvLayer> = Vec::new();
        // External accumulator: keeps the merged state per incremental var.
        let mut acc: HashMap<String, String> = HashMap::new();

        for profile in self.profiles() {
            let path = profile.path().join("make.defaults");
            if !path.is_file() {
                continue;
            }

            // Determine which vars to isolate for this layer.
            // Start with the fixed incremental vars, then add all currently
            // known USE_EXPAND keys so their values are also captured cleanly.
            let mut expand_keys: Vec<String> = acc
                .get("USE_EXPAND")
                .map(|v| v.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default();
            let mut unprefixed_keys: Vec<String> = acc
                .get("USE_EXPAND_UNPREFIXED")
                .map(|v| v.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default();
            let mut incr: Vec<String> = vec![
                "USE".into(),
                "USE_EXPAND".into(),
                "USE_EXPAND_HIDDEN".into(),
                "USE_EXPAND_IMPLICIT".into(),
                "USE_EXPAND_UNPREFIXED".into(),
            ];
            for key in expand_keys.iter().chain(&unprefixed_keys) {
                if !incr.contains(key) {
                    incr.push(key.clone());
                }
            }

            // Reset all incremental vars to empty so this file's assignments
            // are its pure contribution, not a replacement of accumulated state.
            let reset: String = incr.iter().map(|v| format!("{}=\"\"\n", v)).collect();
            shell.run_string(&reset).await?;

            // Source the file through brush — all bash features available,
            // cross-file non-incremental vars (set by earlier files) are visible.
            shell.source_make_defaults(&path).await?;

            // Capture this layer's contributions.
            let mut vars: HashMap<String, String> = HashMap::new();

            // Collect the fixed incremental vars.
            for var in &incr {
                if let Some(val) = shell.get_var(var)
                    && !val.is_empty()
                {
                    vars.insert(var.clone(), val);
                }
            }
            // Discover any new USE_EXPAND keys this file introduced.
            let new_expand = shell.get_var("USE_EXPAND").unwrap_or_default();
            for key in new_expand.split_whitespace() {
                if !expand_keys.iter().any(|k| k == key) {
                    expand_keys.push(key.to_string());
                }
                if !incr.contains(&key.to_string())
                    && let Some(val) = shell.get_var(key)
                    && !val.is_empty()
                {
                    vars.insert(key.to_string(), val);
                }
            }
            // Same for USE_EXPAND_UNPREFIXED keys.
            let new_unprefixed = shell.get_var("USE_EXPAND_UNPREFIXED").unwrap_or_default();
            for key in new_unprefixed.split_whitespace() {
                if !unprefixed_keys.iter().any(|k| k == key) {
                    unprefixed_keys.push(key.to_string());
                }
                if !incr.contains(&key.to_string())
                    && let Some(val) = shell.get_var(key)
                    && !val.is_empty()
                {
                    vars.insert(key.to_string(), val);
                }
            }

            // Translate this layer's USE_EXPAND / USE_EXPAND_UNPREFIXED
            // values into USE tokens and fold them, together with the layer's
            // own `USE`, into the accumulator. Portage prepends the expansions
            // to each make.defaults file's USE (config.py `regenerate()`,
            // `make_defaults_use`) so profile defaults like `ELIBC="glibc"`
            // participate in the incremental fold as ordinary flags
            // (`elibc_glibc`) — this is how they reach `ResolvedUse::pre_env`
            // and every per-package `resolve_effective_use` fold downstream.
            let (unpref, pref) =
                expand_var_tokens(|k| vars.get(k).cloned(), &unprefixed_keys, &expand_keys);
            let mut layer_use: Vec<String> = unpref;
            layer_use.extend(pref);
            layer_use.extend(vars.get("USE").cloned());
            if !layer_use.is_empty() {
                let prev = acc.get("USE").cloned().unwrap_or_default();
                let contrib = layer_use.join(" ");
                let merged = merge_use_var("USE", [prev.as_str(), contrib.as_str()].into_iter());
                acc.insert("USE".into(), merged.join(" "));
            }

            // Merge the remaining per-variable contributions.
            for (var, val) in &vars {
                if var == "USE" {
                    continue;
                }
                let prev = acc.get(var.as_str()).cloned().unwrap_or_default();
                let merged = merge_use_var(var, [prev.as_str(), val.as_str()].into_iter());
                acc.insert(var.clone(), merged.join(" "));
            }

            // Restore the accumulated state into the shell so the next file
            // sees the full inherited environment.
            let restore: String = acc
                .iter()
                .map(|(k, v)| format!("{}={}\n", k, shell_quote(v)))
                .collect();
            shell.run_string(&restore).await?;

            layers.push(ProfileEnvLayer { path, vars });
        }

        Ok(ProfileEnv { layers })
    }

    /// Configure a shell with this profile stack's effective USE flags.
    ///
    /// See `configure_shell` for the full description.
    pub async fn configure_shell(
        &self,
        shell: &mut EbuildShell,
        extra_confs: &[&std::path::Path],
    ) -> Result<()> {
        configure_shell(shell, self, extra_confs).await
    }
}

/// Configure a shell with the effective USE flags from a profile stack.
///
/// Calls [`resolve_use_flags`] then sets the Rust-side `use_flags` HashSet so
/// the `use()` / `usev()` / `usex()` builtins work correctly during phase execution.
///
/// See [`resolve_use_flags`] for the full evaluation order.
pub async fn configure_shell(
    shell: &mut EbuildShell,
    stack: &ProfileStack,
    extra_confs: &[&std::path::Path],
) -> Result<()> {
    let resolved = resolve_use_flags(shell, stack, extra_confs, None).await?;
    let refs: Vec<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
    shell.set_use_flags(&refs)
}

/// The outcome of resolving a profile stack's USE: the enabled flags plus the
/// flags an explicit `-flag` USE token turned off. `disabled` is carried so the
/// per-package step can record them as explicit `Disabled` and override a
/// `+flag` IUSE default — portage gives a configured `USE=-flag` precedence over
/// the ebuild's default. `enabled`/`disabled` are disjoint. Package-independent
/// (no `package.use`, no ebuild's own IUSE defaults) — see [`Self::pre_env`]/
/// [`Self::env_use`] for the pieces a per-package fold still needs.
///
/// `pre_env` and `env_use` exist because real portage resolves USE as **one
/// ordered fold over fixed layers** (`pkginternal < defaults < conf < pkg <
/// env`, Portage's own `USE_ORDER`/`config.py` `regenerate()`/`setcpv()`), and
/// `-*` is not a mode — it's an ordinary token that clears whatever the fold
/// accumulated from *lower* layers so far. `package.use` (`pkg`) sits between
/// `conf` and `env`, so a `-*` in `make.conf` doesn't affect it but a `-*` in
/// the environment does; `em`'s canonical per-package resolver
/// (`portage_solver::resolve_effective_use`) needs the fold's state
/// immediately before `pkg`/`env` to reproduce this — a single collapsed
/// `enabled`/`disabled` set (or a `wildcard_reset` bool derived from it) can't
/// express it, since the information is *where in the fold order* a `-*`
/// appeared, not a fact about the final state. See
/// `docs/architecture.md`'s USE resolution section for the full model.
pub struct ResolvedUse {
    pub enabled: UseFlags,
    pub disabled: Vec<Interned<DefaultInterner>>,
    /// Raw, marker-preserving fold of profile `make.defaults` + `extra_confs`
    /// (`make.conf`), i.e. the shell's `USE` value immediately before the
    /// environment layer is applied. Feed this into
    /// [`merge_flag_lists_signed`](crate::repo::profile::merge_flag_lists_signed)-style
    /// per-package folding *before* `package.use` and *before* the raw `env_use`.
    pub pre_env: String,
    /// The environment layer's `USE` contribution, unmerged: the raw `USE`
    /// from the process environment plus the environment's translated
    /// `USE_EXPAND`/`USE_EXPAND_UNPREFIXED` values (see [`env_layer_use`]).
    /// This is specifically what determines whether `package.use` gets wiped:
    /// it must be folded in *after* `package.use`, matching portage's
    /// `pkg < env` layer order.
    pub env_use: String,
}

/// Resolve the effective USE flags for a profile stack without setting the
/// Rust-side execution state.
///
/// `extra_confs` is a list of additional shell scripts (e.g.
/// `/etc/portage/make.conf`) sourced **after** the profile env is applied but
/// **before** `use.force`/`use.mask` are applied.
///
/// Computation order:
/// 1. Each `make.defaults` sourced through brush with per-layer USE isolation
///    (see [`ProfileStack::profile_env`]); each layer's `USE_EXPAND`/
///    `USE_EXPAND_UNPREFIXED` values are translated into USE tokens and
///    folded with that layer's own `USE` (portage's `make_defaults_use`
///    translation, `config.py` `regenerate()`)
/// 2. Each `extra_confs` script sourced with the same incremental treatment
///    (its expand values folded after its `USE`, portage's conf-layer order)
/// 3. Process-environment layer: `USE`, `USE_EXPAND` keys, and
///    `USE_EXPAND_UNPREFIXED` keys read from `std::env`, translated the same
///    way, and merged
/// 4. Profile `use.force` — unconditional add
/// 5. Profile `use.mask` — unconditional remove
///
/// `USE=-flag` disables (steps 1-3) are tracked separately and returned in
/// [`ResolvedUse::disabled`]; `use.force` clears a flag from that set.
///
/// The shell's bash state is updated as a side-effect.
async fn resolve_use_flags(
    shell: &mut EbuildShell,
    stack: &ProfileStack,
    extra_confs: &[&std::path::Path],
    extra_use_override: Option<&str>,
) -> Result<ResolvedUse> {
    let ProfileEnv { layers: _ } = stack.profile_env(shell).await?;

    for conf in extra_confs {
        source_incremental(shell, ConfSource::File(conf)).await?;
    }
    // A transient, in-process conf-layer override (e.g. `em stages --stage1`'s
    // `USE="-* build ${BOOTSTRAP_USE}"`) — folded at the exact same position
    // as a real make.conf, after it, so it behaves as "one more conf file"
    // rather than the process-environment layer a raw `std::env::set_var`
    // would land at (which would incorrectly wipe `package.use`, layer 5).
    if let Some(content) = extra_use_override {
        source_incremental(shell, ConfSource::Str(content)).await?;
    }

    // Snapshot the fold immediately before the environment layer — this is
    // `pkginternal < defaults < conf` in portage's layer order, the state a
    // per-package fold must resume from *before* `package.use`/`env`. See
    // `ResolvedUse::pre_env`'s doc.
    let pre_env = shell.get_var("USE").unwrap_or_default();
    let env_use = env_layer_use(shell).join(" ");

    apply_env_layer(shell).await?;

    let CollectedUse {
        mut enabled,
        mut disabled,
    } = collect_use_flags(shell);

    let flag_set: HashSet<String> = enabled.iter().cloned().collect();
    for flag in stack.use_force()? {
        if !flag_set.contains(&flag) {
            enabled.push(flag.clone());
        }
        disabled.retain(|f| f != &flag); // force wins over an explicit disable
    }

    let mask: HashSet<String> = stack.use_mask()?.into_iter().collect();
    enabled.retain(|f| !mask.contains(f.as_str()));

    Ok(ResolvedUse {
        enabled: UseFlags(
            enabled
                .iter()
                .map(|f| Interned::<DefaultInterner>::intern(f.as_str()))
                .collect(),
        ),
        disabled: disabled
            .iter()
            .map(|f| Interned::<DefaultInterner>::intern(f.as_str()))
            .collect(),
        pre_env,
        env_use,
    })
}

/// Translate a single config layer's `USE_EXPAND` / `USE_EXPAND_UNPREFIXED`
/// variable values into USE tokens, mirroring portage's `config.py`
/// `regenerate()`: `ELIBC="glibc"` → `elibc_glibc`, sign-preserving
/// (`PYTHON_TARGETS="-python3_12"` → `-python_targets_python3_12`);
/// unprefixed values pass through verbatim (`ARCH="amd64"` → `amd64`).
///
/// Portage folds these expansions per layer, *as USE tokens*, so they get the
/// same incremental `-flag`/`-*` treatment as everything else in the fold.
/// Returns `(unprefixed_tokens, prefixed_tokens)`; the caller places them
/// around the layer's own `USE` per portage's order (make.defaults: both
/// before USE; conf/env layers: unprefixed before, prefixed after).
fn expand_var_tokens(
    get: impl Fn(&str) -> Option<String>,
    unprefixed_keys: &[String],
    expand_keys: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut unprefixed = Vec::new();
    for key in unprefixed_keys {
        if let Some(val) = get(key) {
            unprefixed.extend(val.split_whitespace().map(str::to_string));
        }
    }
    let mut prefixed = Vec::new();
    for key in expand_keys {
        if let Some(val) = get(key) {
            let prefix = key.to_lowercase();
            for v in val.split_whitespace() {
                match v.strip_prefix('-') {
                    Some(n) => prefixed.push(format!("-{prefix}_{n}")),
                    None => prefixed.push(format!("{prefix}_{v}")),
                }
            }
        }
    }
    (unprefixed, prefixed)
}

/// The process environment's USE-layer contribution: unprefixed expand
/// values, the raw `USE`, then prefixed `USE_EXPAND` expansions — portage's
/// within-layer order for the `env` layer (a `-*` in the environment `USE`
/// clears lower layers but not the environment's own expand variables).
fn env_layer_use(shell: &EbuildShell) -> Vec<String> {
    let expand_keys: Vec<String> = shell
        .get_var("USE_EXPAND")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let unprefixed_keys: Vec<String> = shell
        .get_var("USE_EXPAND_UNPREFIXED")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let (unpref, pref) =
        expand_var_tokens(|k| std::env::var(k).ok(), &unprefixed_keys, &expand_keys);
    let mut out = unpref;
    out.extend(std::env::var("USE").ok());
    out.extend(pref);
    out
}

/// Merge process-environment USE variables into the shell as a final incremental layer.
///
/// Reads `USE`, all `USE_EXPAND` keys, and all `USE_EXPAND_UNPREFIXED` keys from
/// `std::env`.  Any present values are merged with the accumulated shell state using
/// the same incremental semantics as profile layers (tokens prefixed with `-` remove);
/// the expand-variable contributions are also translated into USE tokens
/// ([`env_layer_use`]) so they land in the `USE` fold itself.
///
/// This is how `PYTHON_TARGETS=python3_15 em cat/pkg` adds a target without
/// replacing the full flag set, mirroring how `CC=my-cc` is applied in `init_build_env`.
async fn apply_env_layer(shell: &mut EbuildShell) -> Result<()> {
    let use_expand = shell.get_var("USE_EXPAND").unwrap_or_default();
    let unprefixed = shell.get_var("USE_EXPAND_UNPREFIXED").unwrap_or_default();

    let mut vars: Vec<String> = Vec::new();
    for k in use_expand.split_whitespace() {
        vars.push(k.to_string());
    }
    for k in unprefixed.split_whitespace() {
        if !vars.contains(&k.to_string()) {
            vars.push(k.to_string());
        }
    }

    let mut restore = String::new();
    for var in &vars {
        if let Ok(env_val) = std::env::var(var) {
            let existing = shell.get_var(var).unwrap_or_default();
            let merged = merge_use_var(var, [existing.as_str(), env_val.as_str()].into_iter());
            restore += &format!("{}={}\n", var, shell_quote(&merged.join(" ")));
        }
    }
    let layer_use = env_layer_use(shell);
    if !layer_use.is_empty() {
        let existing = shell.get_var("USE").unwrap_or_default();
        let contrib = layer_use.join(" ");
        let merged = merge_use_var("USE", [existing.as_str(), contrib.as_str()].into_iter());
        restore += &format!("USE={}\n", shell_quote(&merged.join(" ")));
    }
    if !restore.is_empty() {
        shell.run_string(&restore).await?;
    }
    Ok(())
}

/// Source a single config file (e.g. `make.conf`) with incremental USE semantics.
///
/// Before sourcing, the incremental vars (`USE` and all known `USE_EXPAND`
/// keys) are reset to empty so the file's own assignments represent its pure
/// contribution.  After sourcing, those contributions are merged back into
/// the accumulated shell state using [`merge_flag_lists`].
/// Where one `source_incremental` layer's content comes from: a real conf
/// file (`/etc/portage/make.conf`), or a raw string — e.g. a transient
/// `USE="-* build ${BOOTSTRAP_USE}"` override synthesized in-process (`em
/// stages --stage1`'s recipe), which needs the exact same conf-layer
/// incremental treatment without needing a real file on disk.
enum ConfSource<'a> {
    File(&'a std::path::Path),
    Str(&'a str),
}

async fn source_incremental(shell: &mut EbuildShell, source: ConfSource<'_>) -> Result<()> {
    // Collect the set of incremental vars to isolate.
    let mut incr: Vec<String> = vec![
        "USE".into(),
        "USE_EXPAND".into(),
        "USE_EXPAND_HIDDEN".into(),
        "USE_EXPAND_IMPLICIT".into(),
        "USE_EXPAND_UNPREFIXED".into(),
    ];
    let expand = shell.get_var("USE_EXPAND").unwrap_or_default();
    for key in expand.split_whitespace() {
        if !incr.contains(&key.to_string()) {
            incr.push(key.to_string());
        }
    }
    let unprefixed = shell.get_var("USE_EXPAND_UNPREFIXED").unwrap_or_default();
    for key in unprefixed.split_whitespace() {
        if !incr.contains(&key.to_string()) {
            incr.push(key.to_string());
        }
    }

    // Save current accumulated values and reset vars to empty.
    let saved: HashMap<String, String> = incr
        .iter()
        .filter_map(|v| shell.get_var(v).map(|val| (v.clone(), val)))
        .collect();

    let reset: String = incr.iter().map(|v| format!("{}=\"\"\n", v)).collect();
    shell.run_string(&reset).await?;

    // Source the layer's content through brush.
    match source {
        ConfSource::File(path) => shell.source_make_defaults(path).await?,
        ConfSource::Str(content) => shell.run_string(content).await?,
    }

    // Collect this file's contributions.
    let mut contributed: HashMap<String, String> = HashMap::new();
    for var in &incr {
        if let Some(val) = shell.get_var(var)
            && !val.is_empty()
        {
            contributed.insert(var.clone(), val);
        }
    }
    // Pick up any new USE_EXPAND keys the file introduced.
    let mut expand_keys: Vec<String> = expand.split_whitespace().map(str::to_string).collect();
    let new_expand = shell.get_var("USE_EXPAND").unwrap_or_default();
    for key in new_expand.split_whitespace() {
        if !expand_keys.iter().any(|k| k == key) {
            expand_keys.push(key.to_string());
        }
        if !incr.contains(&key.to_string())
            && let Some(val) = shell.get_var(key)
            && !val.is_empty()
        {
            contributed.insert(key.to_string(), val);
        }
    }
    let unprefixed_keys: Vec<String> = unprefixed.split_whitespace().map(str::to_string).collect();

    // Translate the file's USE_EXPAND contributions into USE tokens. In a
    // conf layer portage folds unprefixed values *before* the file's USE and
    // prefixed expansions *after* it (config.py `regenerate()`'s layer loop),
    // so `PYTHON_TARGETS=…` in make.conf survives a `USE="-*"` in the same
    // file.
    let (unpref, pref) = expand_var_tokens(
        |k| contributed.get(k).cloned(),
        &unprefixed_keys,
        &expand_keys,
    );
    let mut layer_use: Vec<String> = unpref;
    layer_use.extend(contributed.get("USE").cloned());
    layer_use.extend(pref);

    // Merge contributions with saved state and restore.
    let mut merged_acc: HashMap<String, String> = saved;
    if !layer_use.is_empty() {
        let prev = merged_acc.get("USE").cloned().unwrap_or_default();
        let contrib = layer_use.join(" ");
        let merged = merge_use_var("USE", [prev.as_str(), contrib.as_str()].into_iter());
        merged_acc.insert("USE".into(), merged.join(" "));
    }
    for (var, new_val) in &contributed {
        if var == "USE" {
            continue;
        }
        let prev = merged_acc.get(var.as_str()).cloned().unwrap_or_default();
        let merged = merge_use_var(var, [prev.as_str(), new_val.as_str()].into_iter());
        merged_acc.insert(var.clone(), merged.join(" "));
    }

    let restore: String = merged_acc
        .iter()
        .map(|(k, v)| format!("{}={}\n", k, shell_quote(v)))
        .collect();
    shell.run_string(&restore).await?;

    Ok(())
}

/// Quote a value for use in a bash assignment (`VAR="..."` form).
fn shell_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Enabled and explicitly-disabled USE flags resolved from the shell state.
///
/// `disabled` carries flags a `-flag` token turned off (from the signed `USE`
/// merge), kept so the per-package step can override a `+flag` IUSE default.
///
/// This collapses the fold to a package-independent flat set — used by
/// [`configure_shell`] (real ebuild execution, which needs a simple flag
/// list for the `use()`/`usex()` builtins, no `package.use`/per-package IUSE
/// involved). The per-package resolver reads [`ResolvedUse::pre_env`]/
/// [`ResolvedUse::env_use`] instead, which preserve *where* a `-*` appeared.
struct CollectedUse {
    enabled: Vec<String>,
    disabled: Vec<String>,
}

fn collect_use_flags(shell: &EbuildShell) -> CollectedUse {
    let use_str = shell.get_var("USE").unwrap_or_default();
    let mut flags: Vec<String> = Vec::new();
    let mut disabled: Vec<String> = Vec::new();

    for token in use_str.split_whitespace() {
        // `-*` clear-all (make.conf(5)): discard everything gathered so far.
        // `merge_flag_lists_signed` preserves it as a leading marker in the
        // shell's `USE` value; this collapsed view has no further use for the
        // marker itself (unlike the per-package fold), so it's just consumed.
        if token == "-*" {
            flags.clear();
            disabled.clear();
            continue;
        }
        if let Some(name) = token.strip_prefix('-') {
            flags.retain(|f| f != name);
            if !disabled.iter().any(|f| f == name) {
                disabled.push(name.to_string());
            }
        } else {
            disabled.retain(|f| f != token);
            if !flags.iter().any(|f| f == token) {
                flags.push(token.to_string());
            }
        }
    }

    // `USE_EXPAND`/`USE_EXPAND_UNPREFIXED` values are already translated into
    // USE tokens per layer (see `expand_var_tokens`' callers), so the shell's
    // `USE` is the complete fold — no post-hoc re-expansion, which would
    // resurrect expansions a later layer's `-*`/`-flag` legitimately cleared.

    CollectedUse {
        enabled: flags,
        disabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::profile::ProfileStack;
    use crate::repo::repository::Repository;
    use std::collections::HashSet;
    use tempfile::TempDir;

    fn make_profile(dir: &TempDir, name: &str, parents: &[&str]) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let parent_content = parents
            .iter()
            .map(|p| format!("{}\n", p))
            .collect::<String>();
        if !parent_content.is_empty() {
            std::fs::write(path.join("parent"), &parent_content).unwrap();
        }
        path
    }

    fn make_test_repo(dir: &TempDir) -> Repository {
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        Repository::open(dir.path()).unwrap()
    }

    #[tokio::test]
    async fn source_env_file_composes_features_and_overrides_flags() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        // Baseline make.conf-ish environment.
        std::fs::write(
            profile.join("make.defaults"),
            "FEATURES=\"sandbox\"\nCFLAGS=\"-O2\"\n",
        )
        .unwrap();
        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[]).await.unwrap();

        // A package.env file sourced on top: FEATURES composes, CFLAGS replaces.
        let env_file = dir.path().join("ccache");
        std::fs::write(
            &env_file,
            "FEATURES=\"${FEATURES} ccache\"\nCFLAGS=\"-O3\"\n",
        )
        .unwrap();
        shell.source_env_file(&env_file).await.unwrap();

        let features_var = shell.get_var("FEATURES").unwrap_or_default();
        let features: HashSet<&str> = features_var.split_whitespace().collect();
        assert!(features.contains("sandbox"), "baseline FEATURES kept");
        assert!(features.contains("ccache"), "env-file FEATURES composed in");
        assert_eq!(
            shell.get_var("CFLAGS").as_deref(),
            Some("-O3"),
            "CFLAGS replaced"
        );
    }

    #[tokio::test]
    async fn configure_shell_applies_make_defaults_use() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(profile.join("make.defaults"), "USE=\"foo bar\"\n").unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[]).await.unwrap();

        let use_val = shell.get_var("USE").unwrap_or_default();
        let flags: HashSet<&str> = use_val.split_whitespace().collect();
        assert!(flags.contains("foo"), "foo from make.defaults");
        assert!(flags.contains("bar"), "bar from make.defaults");
    }

    #[tokio::test]
    async fn use_flags_tracks_explicit_disable_of_unenabled_flag() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        // `-cxx` for a flag never enabled elsewhere must survive as an explicit
        // disable, so a per-package `+cxx` IUSE default can be overridden.
        std::fs::write(profile.join("make.defaults"), "USE=\"foo -cxx\"\n").unwrap();
        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack.use_flags(&mut shell, &[]).await.unwrap();
        let enabled: Vec<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        let disabled: Vec<&str> = resolved.disabled.iter().map(|f| f.as_str()).collect();
        assert!(enabled.contains(&"foo"), "foo enabled");
        assert!(!enabled.contains(&"cxx"), "cxx not enabled");
        assert!(
            disabled.contains(&"cxx"),
            "explicit -cxx tracked as disabled"
        );
    }

    #[tokio::test]
    async fn use_flags_reenable_clears_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        // Child re-enables what the parent disabled: net enabled, not disabled.
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(parent.join("make.defaults"), "USE=\"-cxx\"\n").unwrap();
        let child = make_profile(&dir, "child", &["../parent"]);
        std::fs::write(child.join("make.defaults"), "USE=\"cxx\"\n").unwrap();
        let stack = ProfileStack::build(child).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack.use_flags(&mut shell, &[]).await.unwrap();
        let enabled: Vec<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        let disabled: Vec<&str> = resolved.disabled.iter().map(|f| f.as_str()).collect();
        assert!(enabled.contains(&"cxx"), "cxx re-enabled by child");
        assert!(!disabled.contains(&"cxx"), "no longer disabled");
    }

    #[tokio::test]
    async fn use_flags_dash_star_clears_accumulated() {
        // The catalyst stage1 form `USE="-* build"`: a child layer clears
        // everything the parent accumulated, then rebuilds from empty.
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(parent.join("make.defaults"), "USE=\"foo bar debuginfod\"\n").unwrap();
        let child = make_profile(&dir, "child", &["../parent"]);
        std::fs::write(child.join("make.defaults"), "USE=\"-* build\"\n").unwrap();
        let stack = ProfileStack::build(child).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack.use_flags(&mut shell, &[]).await.unwrap();
        let enabled: Vec<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        assert!(enabled.contains(&"build"), "build survives the clear-all");
        assert!(!enabled.contains(&"foo"), "foo cleared by -*");
        assert!(!enabled.contains(&"bar"), "bar cleared by -*");
        assert!(
            !enabled.contains(&"debuginfod"),
            "debuginfod cleared by -* (the reported symptom)"
        );
    }

    #[tokio::test]
    async fn configure_shell_applies_use_force_and_mask() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(profile.join("make.defaults"), "USE=\"foo bar\"\n").unwrap();
        std::fs::write(profile.join("use.force"), "forced\n").unwrap();
        std::fs::write(profile.join("use.mask"), "bar\n").unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[]).await.unwrap();

        let use_val = shell.get_var("USE").unwrap_or_default();
        let flags: HashSet<&str> = use_val.split_whitespace().collect();
        assert!(flags.contains("foo"));
        assert!(!flags.contains("bar"), "bar should be masked");
        assert!(
            flags.contains("forced"),
            "forced should be added by use.force"
        );
    }

    #[tokio::test]
    async fn configure_shell_expands_use_expand() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(
            profile.join("make.defaults"),
            "USE_EXPAND=\"CPU_FLAGS_X86\"\nCPU_FLAGS_X86=\"sse2 mmx\"\n",
        )
        .unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[]).await.unwrap();

        let use_val = shell.get_var("USE").unwrap_or_default();
        let flags: HashSet<&str> = use_val.split_whitespace().collect();
        assert!(flags.contains("cpu_flags_x86_sse2"), "sse2 expanded");
        assert!(flags.contains("cpu_flags_x86_mmx"), "mmx expanded");
    }

    #[tokio::test]
    async fn configure_shell_expands_use_expand_unprefixed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(
            profile.join("make.defaults"),
            "USE_EXPAND_UNPREFIXED=\"ARCH\"\nARCH=\"amd64\"\n",
        )
        .unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[]).await.unwrap();

        let use_val = shell.get_var("USE").unwrap_or_default();
        let flags: HashSet<&str> = use_val.split_whitespace().collect();
        assert!(flags.contains("amd64"), "ARCH added unprefixed");
    }

    /// Profile-injected USE_EXPAND defaults (the `elibc_glibc`/`kernel_linux`/
    /// `python_targets_*` family: `profiles/base/make.defaults` sets
    /// `USE_EXPAND="… ELIBC …"` + `ELIBC="glibc"`) must be translated into USE
    /// tokens *inside the fold*, so they reach `ResolvedUse::pre_env` and every
    /// per-package `resolve_effective_use` downstream — dep conditionals like
    /// `!elibc_glibc? ( dev-libs/libintl )` depend on it.
    #[tokio::test]
    async fn use_expand_defaults_reach_pre_env() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(
            profile.join("make.defaults"),
            "USE_EXPAND=\"ELIBC PYTHON_TARGETS\"\n\
             USE_EXPAND_IMPLICIT=\"ELIBC\"\n\
             USE_EXPAND_UNPREFIXED=\"ARCH\"\n\
             ELIBC=\"glibc\"\n\
             PYTHON_TARGETS=\"python3_13\"\n\
             ARCH=\"amd64\"\n\
             USE=\"foo\"\n",
        )
        .unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack.use_flags(&mut shell, &[]).await.unwrap();

        let pre_env: HashSet<&str> = resolved.pre_env.split_whitespace().collect();
        assert!(
            pre_env.contains("elibc_glibc"),
            "ELIBC expanded: {pre_env:?}"
        );
        assert!(
            pre_env.contains("python_targets_python3_13"),
            "PYTHON_TARGETS expanded: {pre_env:?}"
        );
        assert!(pre_env.contains("amd64"), "ARCH added unprefixed");
        assert!(pre_env.contains("foo"), "plain USE kept");
    }

    /// A `-*` in a child profile's USE clears the parent's USE_EXPAND
    /// expansions too — they are ordinary tokens folded at the parent's layer
    /// (portage translates each make.defaults' expand values into that file's
    /// USE, so a later layer's clear-all wipes them like any other flag).
    #[tokio::test]
    async fn use_expand_expansion_cleared_by_child_wildcard_reset() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(
            parent.join("make.defaults"),
            "USE_EXPAND=\"ELIBC\"\nELIBC=\"glibc\"\n",
        )
        .unwrap();
        let child = make_profile(&dir, "child", &["../parent"]);
        std::fs::write(child.join("make.defaults"), "USE=\"-* build\"\n").unwrap();

        let stack = ProfileStack::build(child).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack.use_flags(&mut shell, &[]).await.unwrap();

        let pre_env: Vec<&str> = resolved.pre_env.split_whitespace().collect();
        assert!(
            pre_env.contains(&"-*"),
            "clear-all marker preserved for the per-package fold: {pre_env:?}"
        );
        assert!(pre_env.contains(&"build"));
        assert!(
            !pre_env.contains(&"elibc_glibc"),
            "parent-layer expansion wiped by the child's -*: {pre_env:?}"
        );
    }

    /// make.conf's own USE_EXPAND values are folded *after* its USE (portage's
    /// conf-layer order), so they survive a `USE="-*"` in the same file while
    /// the profile's expansions from the layer below are cleared.
    #[tokio::test]
    async fn conf_expand_values_survive_conf_level_wildcard() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(
            profile.join("make.defaults"),
            "USE_EXPAND=\"PYTHON_TARGETS\"\nPYTHON_TARGETS=\"python3_12\"\n",
        )
        .unwrap();
        let make_conf = dir.path().join("make.conf");
        std::fs::write(
            &make_conf,
            "USE=\"-* build\"\nPYTHON_TARGETS=\"python3_13\"\n",
        )
        .unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack
            .use_flags(&mut shell, &[make_conf.as_path()])
            .await
            .unwrap();

        let pre_env: Vec<&str> = resolved.pre_env.split_whitespace().collect();
        assert!(
            !pre_env.contains(&"python_targets_python3_12"),
            "profile-layer expansion cleared by make.conf's -*: {pre_env:?}"
        );
        assert!(
            pre_env.contains(&"python_targets_python3_13"),
            "make.conf's own expand value folded after its USE: {pre_env:?}"
        );
        assert!(pre_env.contains(&"build"));
    }

    /// `use_flags_with_override`'s whole point: a transient conf-layer
    /// override (`em stages --stage1`'s `USE="-* build ${BOOTSTRAP_USE}"`)
    /// must land in `pre_env`, not `env_use` — otherwise it behaves like a
    /// `std::env::set_var("USE", ...)` mutation (the pre-2026-07-12
    /// mechanism), which sits *above* `package.use` and wipes it. Folding it
    /// as one more conf file instead keeps `env_use` as the real,
    /// untouched process environment.
    #[tokio::test]
    async fn use_flags_with_override_lands_in_pre_env_not_env_use() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(profile.join("make.defaults"), "USE=\"unrelated\"\n").unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack
            .use_flags_with_override(&mut shell, &[], "USE=\"-* build\"\n")
            .await
            .unwrap();

        let pre_env: Vec<&str> = resolved.pre_env.split_whitespace().collect();
        assert!(
            pre_env.contains(&"-*") && pre_env.contains(&"build"),
            "override folded into pre_env: {pre_env:?}"
        );
        assert!(
            !pre_env.contains(&"unrelated"),
            "the override's own -* clears the lower defaults layer: {pre_env:?}"
        );
        assert!(
            resolved.env_use.is_empty(),
            "the override must not leak into env_use (the real process env): {:?}",
            resolved.env_use
        );
    }

    #[tokio::test]
    async fn configure_shell_make_conf_applied_before_force_mask() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(profile.join("make.defaults"), "USE=\"foo bar forced\"\n").unwrap();
        std::fs::write(profile.join("use.force"), "forced\n").unwrap();
        std::fs::write(profile.join("use.mask"), "bar\n").unwrap();

        let make_conf = dir.path().join("make.conf");
        std::fs::write(&make_conf, "USE=\"${USE} user_flag -forced\"\n").unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[make_conf.as_path()])
            .await
            .unwrap();

        let use_val = shell.get_var("USE").unwrap_or_default();
        let flags: HashSet<&str> = use_val.split_whitespace().collect();
        assert!(flags.contains("foo"));
        assert!(flags.contains("user_flag"), "user_flag from make.conf");
        assert!(!flags.contains("bar"), "bar masked by use.mask");
        assert!(
            flags.contains("forced"),
            "use.force overrides make.conf removal"
        );
    }

    /// Two-layer profile: base sets unicode, child sets crypt — both must survive.
    #[tokio::test]
    async fn configure_shell_two_layer_use_accumulation() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let base = make_profile(&dir, "base", &[]);
        std::fs::write(base.join("make.defaults"), "USE=\"unicode acl\"\n").unwrap();
        let leaf = make_profile(&dir, "leaf", &["../base"]);
        // Overwrites USE (no ${USE}) — this is the bug pattern we're fixing.
        std::fs::write(leaf.join("make.defaults"), "USE=\"crypt ssl\"\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[]).await.unwrap();

        let use_val = shell.get_var("USE").unwrap_or_default();
        let flags: HashSet<&str> = use_val.split_whitespace().collect();
        assert!(flags.contains("unicode"), "unicode from base must survive");
        assert!(flags.contains("acl"), "acl from base must survive");
        assert!(flags.contains("crypt"), "crypt from leaf must be present");
        assert!(flags.contains("ssl"), "ssl from leaf must be present");
    }
}
