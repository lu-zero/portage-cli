//! Shell-dependent profile operations
//!
//! Methods that source bash files (make.defaults) or read shell state (USE
//! flags) cannot live in `repo/profile.rs` without creating an upward
//! dependency from the repo module into the build module.  They live here
//! instead and are added to `Profile` / `ProfileStack` via second impl blocks.

use gentoo_core::UseToken;
use gentoo_core::use_token::global_use;
use std::collections::{HashMap, HashSet};

use portage_atom::interner::Interned;

use super::shell::EbuildShell;
use crate::error::Result;
use crate::repo::profile::{
    Profile, ProfileEnv, ProfileEnvLayer, ProfileStack, UseFlags, merge_accept_lists,
    merge_flag_lists, merge_flag_lists_signed,
};

/// One snapshot of a process environment, which the profile evaluation reads in place of
/// `std::env`
#[derive(Debug, Clone, Default)]
pub struct Environment(std::collections::BTreeMap<String, String>);

impl Environment {
    /// This process's environment, as it is now
    pub fn process() -> Self {
        Self(std::env::vars().collect())
    }

    /// The value of `name`, if set
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// Every variable, by name
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for Environment {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

/// Merge a stacked variable's layers. `USE` preserves explicit disables
/// (`-flag` kept so it can override a `+flag` IUSE default per package); the
/// `ACCEPT_*` lists keep their negations; every other incremental var drops them.
pub(super) fn merge_use_var<'a>(var: &str, iter: impl Iterator<Item = &'a str>) -> Vec<String> {
    match var {
        "USE" => merge_flag_lists_signed(iter),
        "ACCEPT_LICENSE" | "ACCEPT_PROPERTIES" | "ACCEPT_RESTRICT" => merge_accept_lists(iter),
        _ => merge_flag_lists(iter),
    }
}

/// The `portage.const.INCREMENTALS` vars plus the `ACCEPT_*` lists Portage also
/// accumulates, reset-and-accumulated the same way (raw string merge, not USE-token
/// translation) across every profile/conf layer by both [`ProfileStack::profile_env`]
/// and [`source_incremental`].
///
/// Individual USE_EXPAND-listed variables (`VIDEO_CARDS`, `PYTHON_TARGETS`,
/// `ELIBC`, …) are deliberately **not** here: they need per-layer USE-token
/// translation, not this list's plain raw-string merge. See
/// `portage-repo/docs/pms-notes.md` for the full PMS 5.3.1/5.3.2 rules,
/// the live-verification transcripts, and why the split is this shape.
const INCREMENTAL_VARS: &[&str] = &[
    "USE",
    "USE_EXPAND",
    "USE_EXPAND_HIDDEN",
    "USE_EXPAND_IMPLICIT",
    "USE_EXPAND_UNPREFIXED",
    "FEATURES",
    "ACCEPT_KEYWORDS",
    "ACCEPT_LICENSE",
    "ACCEPT_PROPERTIES",
    "ACCEPT_RESTRICT",
    "CONFIG_PROTECT",
    "CONFIG_PROTECT_MASK",
    "ENV_UNSET",
    "IUSE_IMPLICIT",
    "PROFILE_ONLY_VARIABLES",
];

impl Profile {
    /// Source this profile's `make.defaults` into the shell, if present
    ///
    /// See [PMS 5.2.4](https://projects.gentoo.org/pms/9/pms.html#makedefaults).
    pub async fn make_defaults(&self, shell: &mut EbuildShell) -> Result<()> {
        if let Some(text) = self.read_file("make.defaults")? {
            shell
                .source_make_defaults_text(&text, &self.shown("make.defaults"))
                .await?;
        }
        Ok(())
    }
}

impl ProfileStack {
    /// Source `make.defaults` from every profile in stack order
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

    /// Resolve the effective USE flags for this profile stack
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
        extra_confs: &[ConfSource<'_>],
    ) -> Result<ResolvedUse> {
        resolve_use_flags(shell, self, extra_confs, None, &Environment::process()).await
    }

    /// As [`use_flags`](Self::use_flags), plus a transient conf-layer USE
    /// override (see the `resolve_use_flags` function's `extra_use_override`
    /// parameter).
    pub async fn use_flags_with_override(
        &self,
        shell: &mut EbuildShell,
        extra_confs: &[ConfSource<'_>],
        extra_use_override: &str,
    ) -> Result<ResolvedUse> {
        resolve_use_flags(
            shell,
            self,
            extra_confs,
            Some(extra_use_override),
            &Environment::process(),
        )
        .await
    }

    /// Build the layered profile environment by sourcing each `make.defaults`
    /// through brush with per-layer isolation, in the **same** shell. Before
    /// each file, `INCREMENTAL_VARS` reset to empty so its own assignments
    /// are captured as a clean delta, then the accumulated values are
    /// restored for the next file to reference (`${USE}`, …).
    ///
    /// Per PMS 5.3.2, `USE_EXPAND`-listed variables (`VIDEO_CARDS`, `L10N`,
    /// …) are incremental too — see `portage-repo/docs/pms-notes.md` for the
    /// `make_defaults_use`-equivalent translate-and-fold algorithm this
    /// reproduces, including why a later layer's `USE="-*"` wipes an earlier
    /// layer's USE_EXPAND-derived tokens too, folded one layer at a time.
    ///
    /// Known gap: a value assigned to a `USE_EXPAND` variable *before* any
    /// layer has declared it via `USE_EXPAND` folds in untranslated until a
    /// later layer both declares and re-touches it — doesn't occur in any
    /// real Gentoo profile (`USE_EXPAND` is declared once, early, by the
    /// base profile). Returns with the shell holding the fully-accumulated
    /// profile state, ready for `make.conf` to source on top.
    pub async fn profile_env(&self, shell: &mut EbuildShell) -> Result<ProfileEnv> {
        let mut layers: Vec<ProfileEnvLayer> = Vec::new();
        // External accumulator: keeps the merged state per incremental var.
        // Keys are always drawn from INCREMENTAL_VARS, hence &'static str.
        // Seeded with what lower layers (env.d, make.globals) left in the shell.
        let mut acc: HashMap<&'static str, String> = INCREMENTAL_VARS
            .iter()
            .filter_map(|&v| {
                shell
                    .get_var(v)
                    .filter(|val| !val.is_empty())
                    .map(|val| (v, val))
            })
            .collect();
        // Raw accumulated value of each USE_EXPAND-listed variable
        // (VIDEO_CARDS, L10N, …) across the profile chain, so `${VIDEO_CARDS}`
        // etc. stay visible to later layers/the ebuild env. Dynamic keys,
        // unlike INCREMENTAL_VARS, since the variable *names* only become
        // known once USE_EXPAND itself has accumulated far enough.
        let mut expand_acc: HashMap<String, String> = HashMap::new();

        for profile in self.profiles() {
            let Some(text) = profile.read_file("make.defaults")? else {
                continue;
            };
            let path = profile.path().join("make.defaults");

            // USE_EXPAND/USE_EXPAND_UNPREFIXED names known from *prior*
            // layers, used only to decide what to reset before sourcing so
            // this layer's own delta for an already-known variable is
            // cleanly isolated from an ancestor's value. A variable this
            // layer is the first ever to mention needs no reset: nothing
            // could have set it before.
            let prior_expand_keys: Vec<String> = acc
                .get("USE_EXPAND")
                .map(|s| s.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default();
            let prior_unprefixed_keys: Vec<String> = acc
                .get("USE_EXPAND_UNPREFIXED")
                .map(|s| s.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default();

            // Reset the true incrementals to empty so this file's assignments
            // are its pure contribution, not a replacement of accumulated
            // state. Currently-known USE_EXPAND-listed variables get the
            // same treatment now (unlike before this fix).
            let mut reset: String = INCREMENTAL_VARS
                .iter()
                .map(|v| format!("{v}=\"\"\n"))
                .collect();
            for v in prior_expand_keys.iter().chain(prior_unprefixed_keys.iter()) {
                reset.push_str(&format!("{v}=\"\"\n"));
            }
            shell.run_string(&reset).await?;

            // Source the file through brush — all bash features available,
            // cross-file non-incremental vars (set by earlier files) are visible.
            shell
                .source_make_defaults_text(&text, &profile.shown("make.defaults"))
                .await?;

            // Capture this layer's contribution to the true incrementals only.
            let mut vars: HashMap<&'static str, String> = HashMap::new();
            for &var in INCREMENTAL_VARS {
                if let Some(val) = shell.get_var(var)
                    && !val.is_empty()
                {
                    vars.insert(var, val);
                }
            }

            // Merge this layer's incremental contributions except USE, which
            // is handled below combined with this layer's USE_EXPAND-derived
            // tokens (real portage's per-layer order: expand tokens first,
            // this layer's own USE last — `config.py`'s
            // `expand_use.append(use)`).
            for (&var, val) in &vars {
                if var == "USE" {
                    continue;
                }
                let prev = acc.get(var).cloned().unwrap_or_default();
                let merged = merge_use_var(var, [prev.as_str(), val.as_str()].into_iter());
                acc.insert(var, merged.join(" "));
            }

            // Re-read USE_EXPAND/USE_EXPAND_UNPREFIXED *after* the merge
            // above, so a name this same layer just declared (the common
            // case: a base profile's make.defaults both declares
            // USE_EXPAND="VIDEO_CARDS" and sets VIDEO_CARDS= in one file) is
            // picked up this iteration, not one layer late.
            let expand_keys: Vec<String> = acc
                .get("USE_EXPAND")
                .map(|s| s.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default();
            let unprefixed_keys: Vec<String> = acc
                .get("USE_EXPAND_UNPREFIXED")
                .map(|s| s.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default();

            let (unpref_tokens, pref_tokens) =
                expand_var_tokens(|k| shell.get_var(k), &unprefixed_keys, &expand_keys);
            let use_contribution: Option<String> =
                if unpref_tokens.is_empty() && pref_tokens.is_empty() {
                    vars.get("USE").cloned()
                } else {
                    let mut combined = unpref_tokens;
                    combined.extend(pref_tokens);
                    if let Some(u) = vars.get("USE") {
                        combined.push(u.clone());
                    }
                    Some(combined.join(" "))
                };
            if let Some(val) = &use_contribution {
                let prev = acc.get("USE").cloned().unwrap_or_default();
                let merged = merge_use_var("USE", [prev.as_str(), val.as_str()].into_iter());
                acc.insert("USE", merged.join(" "));
            }

            // This layer's own raw contribution to each USE_EXPAND-listed
            // variable (so `${VIDEO_CARDS}` etc. remain visible, accumulated,
            // to later profile layers and the ebuild environment). NOT
            // stored into `vars`/`ProfileEnvLayer` — that stays the raw,
            // unmodified per-file snapshot its doc comment promises.
            for key in expand_keys.iter().chain(unprefixed_keys.iter()) {
                if let Some(val) = shell.get_var(key)
                    && !val.is_empty()
                {
                    let prev = expand_acc.get(key).cloned().unwrap_or_default();
                    let merged = merge_flag_lists_signed([prev.as_str(), val.as_str()].into_iter());
                    expand_acc.insert(key.clone(), merged.join(" "));
                }
            }

            // Restore the accumulated state into the shell so the next file
            // sees the full inherited environment.
            let restore: String = acc
                .iter()
                .map(|(k, v)| format!("{}={}\n", k, shell_quote(v)))
                .chain(
                    expand_acc
                        .iter()
                        .map(|(k, v)| format!("{}={}\n", k, shell_quote(v))),
                )
                .collect();
            shell.run_string(&restore).await?;

            layers.push(ProfileEnvLayer {
                path,
                vars,
                use_tokens: use_contribution.unwrap_or_default(),
            });
        }

        Ok(ProfileEnv { layers })
    }

    /// Configure a shell with this profile stack's effective USE flags
    ///
    /// See `configure_shell` for the full description.
    pub async fn configure_shell(
        &self,
        shell: &mut EbuildShell,
        extra_confs: &[ConfSource<'_>],
    ) -> Result<()> {
        configure_shell(shell, self, extra_confs).await
    }
}

/// Configure a shell with the effective USE flags from a profile stack
///
/// Calls [`resolve_use_flags`] then sets the Rust-side `use_flags` HashSet so
/// the `use()` / `usev()` / `usex()` builtins work correctly during phase execution.
///
/// See [`resolve_use_flags`] for the full evaluation order.
pub async fn configure_shell(
    shell: &mut EbuildShell,
    stack: &ProfileStack,
    extra_confs: &[ConfSource<'_>],
) -> Result<()> {
    let resolved =
        resolve_use_flags(shell, stack, extra_confs, None, &Environment::process()).await?;
    let refs: Vec<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
    shell.set_use_flags(&refs)
}

/// The outcome of resolving a profile stack's USE: the flags on with no package to fold for, and
/// each layer's tokens for the per-package fold.
///
/// The layers stay apart because Portage folds USE in order (`pkginternal < defaults < conf <
/// pkg < env`) and a `-*` clears only what came before it; see `docs/design/architecture.md`'s
/// USE stacking precedence section.
pub struct ResolvedUse {
    /// The flags on with no package to fold for: every layer, then `use.force`/`use.mask`
    pub enabled: UseFlags,
    /// What the global config left in `USE` below the profiles (`make.globals`, env.d), then every
    /// profile node's `make.defaults` tokens, in order.
    pub defaults: Vec<UseToken>,
    /// `make.conf`, extra confs and the in-process override, in order, as USE tokens.
    pub conf: Vec<UseToken>,
    /// Per `make.defaults` file, that layer's translated USE contribution.
    pub profile_defaults: Vec<(std::path::PathBuf, Vec<UseToken>)>,
    /// The environment layer as USE tokens: unprefixed `USE_EXPAND` values, `USE`, then each
    /// `USE_EXPAND` variable present, which replaces its group. Folded after `package.use`.
    pub env: Vec<UseToken>,
}

/// Resolve the effective USE flags for a profile stack without setting the
/// Rust-side execution state.
///
/// `extra_confs` is a list of additional shell scripts (e.g.
/// `/etc/portage/make.conf`) sourced **after** the profile env is applied but
/// **before** `use.force`/`use.mask` are applied.
///
/// Computation order (translate-and-fold detail: [`ProfileStack::profile_env`]):
/// 1. Each `make.defaults`, per-layer USE_EXPAND-translated and folded
/// 2. Each `extra_confs` script, same incremental treatment
/// 3. Process env: `USE`/`USE_EXPAND`/`USE_EXPAND_UNPREFIXED`, merged
/// 4. Profile `use.force` — unconditional add
/// 5. Profile `use.mask` — unconditional remove
///
/// The shell's bash state is updated as a side-effect.
pub(super) async fn resolve_use_flags(
    shell: &mut EbuildShell,
    stack: &ProfileStack,
    extra_confs: &[ConfSource<'_>],
    extra_use_override: Option<&str>,
    env: &Environment,
) -> Result<ResolvedUse> {
    resolve_use_flags_observed(
        shell,
        stack,
        extra_confs,
        extra_use_override,
        env,
        &mut |_, _| {},
    )
    .await
}

/// The points of [`resolve_use_flags_observed`] at which the shell holds one more layer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    /// After the profile stack's `make.defaults`
    Defaults,
    /// After `make.conf` and any override
    Conf,
}

/// [`resolve_use_flags`], calling `observe` with the shell as each [`Stage`] is reached
pub(super) async fn resolve_use_flags_observed(
    shell: &mut EbuildShell,
    stack: &ProfileStack,
    extra_confs: &[ConfSource<'_>],
    extra_use_override: Option<&str>,
    env: &Environment,
    observe: &mut dyn FnMut(Stage, &EbuildShell),
) -> Result<ResolvedUse> {
    let below_profiles = shell.get_var("USE").unwrap_or_default();
    let ProfileEnv { layers } = stack.profile_env(shell).await?;
    observe(Stage::Defaults, shell);
    let profile_defaults: Vec<(std::path::PathBuf, Vec<UseToken>)> = layers
        .into_iter()
        .map(|l| {
            let tokens = profile_tokens(&l.use_tokens, &l.path);
            (l.path, tokens)
        })
        .collect();
    let mut defaults: Vec<UseToken> = below_profiles
        .split_whitespace()
        .filter_map(UseToken::user)
        .collect();
    defaults.extend(profile_defaults.iter().flat_map(|(_, t)| t.iter().copied()));

    let mut conf: Vec<UseToken> = Vec::new();
    for source in extra_confs {
        conf.extend(source_incremental(shell, *source).await?.tokens);
    }
    // A transient, in-process conf-layer override (e.g. `em stages --stage1`'s
    // `USE="-* build ${BOOTSTRAP_USE}"`) — folded at the exact same position
    // as a real make.conf, after it, so it behaves as "one more conf file"
    // rather than the process-environment layer a raw `std::env::set_var`
    // would land at (which would incorrectly wipe `package.use`, layer 5).
    if let Some(content) = extra_use_override {
        conf.extend(
            source_incremental(shell, ConfSource::Str(content))
                .await?
                .tokens,
        );
    }
    observe(Stage::Conf, shell);

    let env_tokens = layer_tokens(
        |k| env.get(k).map(str::to_owned),
        env.get("USE"),
        &var_list(shell, "USE_EXPAND_UNPREFIXED"),
        &var_list(shell, "USE_EXPAND"),
    );

    apply_env_layer(shell, env).await?;

    let mut enabled = global_use(defaults.iter().chain(&conf).chain(&env_tokens).copied());
    for flag in stack.use_force()? {
        let flag = Interned::intern(&flag);
        if !enabled.contains(&flag) {
            enabled.push(flag);
        }
    }
    let mask: HashSet<String> = stack.use_mask()?.into_iter().collect();
    enabled.retain(|f| !mask.contains(f.as_str()));

    Ok(ResolvedUse {
        enabled: UseFlags(enabled),
        defaults,
        conf,
        profile_defaults,
        env: env_tokens,
    })
}

/// A `make.defaults` USE contribution as tokens; one PMS does not allow is dropped with a warning
fn profile_tokens(use_tokens: &str, path: &std::path::Path) -> Vec<UseToken> {
    use_tokens
        .split_whitespace()
        .filter_map(|tok| match UseToken::profile(tok) {
            Ok(tok) => Some(tok),
            Err(e) => {
                tracing::warn!("{}: {e}", path.display());
                None
            }
        })
        .collect()
}

/// The whitespace-separated names in `var` (`USE_EXPAND`, `USE_EXPAND_UNPREFIXED`)
fn var_list(shell: &EbuildShell, var: &str) -> Vec<String> {
    shell
        .get_var(var)
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// One make.conf or environment layer as USE tokens, in Portage's order: unprefixed `USE_EXPAND`
/// values, `USE`, then each `USE_EXPAND` variable the layer assigned, which replaces its group
fn layer_tokens(
    get: impl Fn(&str) -> Option<String>,
    use_var: Option<&str>,
    unprefixed_keys: &[String],
    expand_keys: &[String],
) -> Vec<UseToken> {
    let mut out: Vec<UseToken> = Vec::new();
    for key in unprefixed_keys {
        if let Some(val) = get(key) {
            out.extend(val.split_whitespace().filter_map(UseToken::user));
        }
    }
    out.extend(
        use_var
            .into_iter()
            .flat_map(str::split_whitespace)
            .filter_map(UseToken::user),
    );
    for key in expand_keys {
        if let Some(val) = get(key) {
            for tok in UseToken::assignment(key, &val) {
                match tok {
                    Ok(tok) => out.push(tok),
                    Err(e) => tracing::warn!("{key}: {e}; ignored"),
                }
            }
        }
    }
    out
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
fn env_layer_use(shell: &EbuildShell, env: &Environment) -> Vec<String> {
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
    let (unpref, pref) = expand_var_tokens(
        |k| env.get(k).map(str::to_owned),
        &unprefixed_keys,
        &expand_keys,
    );
    let mut out = unpref;
    out.extend(env.get("USE").map(str::to_owned));
    out.extend(pref);
    out
}

/// Remove every USE token belonging to `var`'s expansion (`video_cards_*`
/// for `VIDEO_CARDS`, enabled or disabled) from an already-merged/signed USE
/// token string. Used by [`source_incremental`] when a non-profile layer
/// explicitly re-assigns a USE_EXPAND-listed variable: real portage clears
/// every accumulated token with that prefix before adding the new layer's
/// own, rather than merging. See `portage-repo/docs/pms-notes.md` for the
/// live-verified make.conf example this replaced a union with.
fn strip_expand_tokens(use_str: &str, var: &str) -> String {
    let prefix = format!("{}_", var.to_lowercase());
    use_str
        .split_whitespace()
        .filter(|tok| {
            let bare = tok.strip_prefix('-').unwrap_or(tok);
            !bare.starts_with(prefix.as_str())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Merge process-environment USE and `ACCEPT_*` variables into the shell as a final
/// incremental layer: `USE`/`USE_EXPAND_UNPREFIXED` and the `ACCEPT_*` lists merge with the
/// accumulated state (`-`-prefixed tokens remove); a `USE_EXPAND` variable
/// present in the environment gets the same non-incremental replace
/// [`source_incremental`] applies to make.conf, one layer higher (so it
/// also outranks make.conf's value). Translated via [`env_layer_use`].
///
/// This is how `PYTHON_TARGETS=python3_15 em cat/pkg` selects a target,
/// mirroring how `CC=my-cc` is applied in `init_build_env`.
async fn apply_env_layer(shell: &mut EbuildShell, env: &Environment) -> Result<()> {
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

    let mut restore = String::new();
    let mut use_base = shell.get_var("USE").unwrap_or_default();
    for var in &expand_keys {
        if let Some(env_val) = env.get(var) {
            use_base = strip_expand_tokens(&use_base, var);
            restore += &format!("{}={}\n", var, shell_quote(env_val.trim()));
        }
    }
    for var in &unprefixed_keys {
        if expand_keys.contains(var) {
            continue;
        }
        if let Some(env_val) = env.get(var) {
            let existing = shell.get_var(var).unwrap_or_default();
            let merged = merge_use_var(var, [existing.as_str(), env_val].into_iter());
            restore += &format!("{}={}\n", var, shell_quote(&merged.join(" ")));
        }
    }
    for var in ["ACCEPT_LICENSE", "ACCEPT_PROPERTIES", "ACCEPT_RESTRICT"] {
        if let Some(env_val) = env.get(var) {
            let existing = shell.get_var(var).unwrap_or_default();
            let merged = merge_use_var(var, [existing.as_str(), env_val].into_iter());
            restore += &format!("{}={}\n", var, shell_quote(&merged.join(" ")));
        }
    }
    let layer_use = env_layer_use(shell, env);
    if layer_use.is_empty() {
        if use_base != shell.get_var("USE").unwrap_or_default() {
            restore += &format!("USE={}\n", shell_quote(&use_base));
        }
    } else {
        let contrib = layer_use.join(" ");
        let merged = merge_use_var("USE", [use_base.as_str(), contrib.as_str()].into_iter());
        restore += &format!("USE={}\n", shell_quote(&merged.join(" ")));
    }
    if !restore.is_empty() {
        shell.run_string(&restore).await?;
    }
    Ok(())
}

/// Where one `source_incremental` layer's content comes from: a real conf
/// file (`/etc/portage/make.conf`), or a raw string — e.g. a transient
/// `USE="-* build ${BOOTSTRAP_USE}"` override synthesized in-process (`em
/// stages --stage1`'s recipe), which needs the exact same conf-layer
/// incremental treatment without needing a real file on disk.
#[derive(Clone, Copy)]
pub enum ConfSource<'a> {
    /// A real conf file on disk (e.g. `/etc/portage/make.conf`).
    File(&'a std::path::Path),
    /// Raw conf-layer content sourced directly, no file needed.
    Str(&'a str),
}

/// One conf-layer's own incremental contribution (not merged with saved state).
pub(super) struct IncrementalContribution {
    tokens: Vec<UseToken>,
}

/// Source a single config file (e.g. `make.conf`) with incremental USE semantics
///
/// `INCREMENTAL_VARS` are reset to empty before sourcing so the file's own
/// assignments represent its pure contribution, then merged back in via
/// [`merge_flag_lists`]. USE_EXPAND-listed variables (`VIDEO_CARDS`, `L10N`,
/// …) instead get a hard replace when this layer *explicitly assigns* one
/// (even to `""`): [`strip_expand_tokens`] strips every existing token with
/// that prefix first — unlike [`ProfileStack::profile_env`]'s plain merge
/// for the same variables within the profile chain. `USE_EXPAND_UNPREFIXED`
/// variables (`ARCH`, …) keep a plain incremental add — no structural prefix
/// to strip by. See `portage-repo/docs/pms-notes.md` for the details.
///
/// Also returns the layer as USE tokens for the per-package fold, which the flat `USE` strip
/// cannot serve: an assignment replaces its group among the package's own IUSE defaults too.
pub(super) async fn source_incremental(
    shell: &mut EbuildShell,
    source: ConfSource<'_>,
) -> Result<IncrementalContribution> {
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

    // Save current accumulated values and reset vars to empty.
    let saved: HashMap<&'static str, String> = INCREMENTAL_VARS
        .iter()
        .filter_map(|&v| shell.get_var(v).map(|val| (v, val)))
        .collect();
    let saved_expand: HashMap<String, String> = expand_keys
        .iter()
        .chain(unprefixed_keys.iter())
        .filter_map(|v| shell.get_var(v).map(|val| (v.clone(), val)))
        .collect();

    let mut reset: String = INCREMENTAL_VARS
        .iter()
        .map(|v| format!("{v}=\"\"\n"))
        .collect();
    for v in expand_keys.iter().chain(unprefixed_keys.iter()) {
        reset.push_str(&format!("unset {v}\n"));
    }
    shell.run_string(&reset).await?;

    // Source the layer's content through brush.
    match source {
        ConfSource::File(path) => shell.source_make_defaults(path).await?,
        ConfSource::Str(content) => shell.source_conf_str(content).await?,
    }

    // Collect this file's contributions to the true incrementals only.
    let mut contributed: HashMap<&'static str, String> = HashMap::new();
    for &var in INCREMENTAL_VARS {
        if let Some(val) = shell.get_var(var)
            && !val.is_empty()
        {
            contributed.insert(var, val);
        }
    }

    // USE_EXPAND-listed variables this layer explicitly assigned (possibly
    // ""): strip their existing prefix tokens from the *prior* accumulated
    // USE before this layer's own combined delta is merged on top.
    let mut use_base = saved.get("USE").cloned().unwrap_or_default();
    let mut merged_expand: HashMap<String, String> = saved_expand;
    for key in &expand_keys {
        if let Some(val) = shell.get_var(key) {
            use_base = strip_expand_tokens(&use_base, key);
            if val.is_empty() {
                merged_expand.remove(key);
            } else {
                merged_expand.insert(key.clone(), val);
            }
        }
    }
    for key in &unprefixed_keys {
        if let Some(val) = shell.get_var(key) {
            if val.is_empty() {
                merged_expand.remove(key);
            } else {
                merged_expand.insert(key.clone(), val);
            }
        }
    }

    // Read before `restore` below sets every accumulated variable again.
    let tokens = layer_tokens(
        |k| shell.get_var(k),
        contributed.get("USE").map(String::as_str),
        &unprefixed_keys,
        &expand_keys,
    );

    // This layer's own combined USE delta: unprefixed values, then this
    // layer's own USE=, then prefixed (expand) values — portage's conf/env
    // per-layer order (same as `env_layer_use`'s).
    let (unpref_tokens, pref_tokens) =
        expand_var_tokens(|k| shell.get_var(k), &unprefixed_keys, &expand_keys);
    let mut delta = unpref_tokens;
    if let Some(u) = contributed.get("USE") {
        delta.push(u.clone());
    }
    delta.extend(pref_tokens);

    // Merge contributions (except USE, handled above/below) with saved state.
    let mut merged_acc: HashMap<&'static str, String> = saved;
    for (&var, new_val) in &contributed {
        if var == "USE" {
            continue;
        }
        let prev = merged_acc.get(var).cloned().unwrap_or_default();
        let merged = merge_use_var(var, [prev.as_str(), new_val.as_str()].into_iter());
        merged_acc.insert(var, merged.join(" "));
    }
    if !delta.is_empty() {
        let contrib = delta.join(" ");
        let merged = merge_use_var("USE", [use_base.as_str(), contrib.as_str()].into_iter());
        merged_acc.insert("USE", merged.join(" "));
    } else {
        merged_acc.insert("USE", use_base);
    }

    let restore: String = merged_acc
        .iter()
        .map(|(k, v)| format!("{}={}\n", k, shell_quote(v)))
        .chain(
            merged_expand
                .iter()
                .map(|(k, v)| format!("{}={}\n", k, shell_quote(v))),
        )
        .collect();
    shell.run_string(&restore).await?;

    Ok(IncrementalContribution { tokens })
}

/// Quote a value for use in a bash assignment (`VAR="..."` form)
fn shell_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
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
        Repository::builder()
            .in_memory_cache()
            .open(dir.path())
            .unwrap()
    }

    // Most real `make.conf`s do a plain `FEATURES="candy ccache"` with no
    // interpolation, relying on portage itself to merge it with the
    // profile's own `FEATURES` (make.conf(5) "Incremental Variables") —
    // unlike the sibling test below, which only covers explicit
    // `${FEATURES} ccache`. Before this fix, `FEATURES`/`ACCEPT_KEYWORDS`/
    // `ACCEPT_LICENSE`/`CONFIG_PROTECT`/`CONFIG_PROTECT_MASK`/
    // `IUSE_IMPLICIT` weren't in the incremental list, so a plain
    // assignment silently overwrote the profile's value instead of merging.
    #[tokio::test]
    async fn make_conf_merges_features_without_interpolation() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(
            profile.join("make.defaults"),
            "FEATURES=\"test-fail-continue\"\n",
        )
        .unwrap();
        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();

        // Plain assignment, no ${FEATURES} interpolation — the real-world
        // make.conf idiom that relies on portage's own incremental merge.
        let make_conf = dir.path().join("make.conf");
        std::fs::write(&make_conf, "FEATURES=\"candy ccache\"\n").unwrap();
        configure_shell(&mut shell, &stack, &[ConfSource::File(&make_conf)])
            .await
            .unwrap();

        let features_var = shell.get_var("FEATURES").unwrap_or_default();
        let features: HashSet<&str> = features_var.split_whitespace().collect();
        assert!(
            features.contains("test-fail-continue"),
            "profile's FEATURES must survive make.conf's plain assignment: {features:?}"
        );
        assert!(features.contains("candy"), "{features:?}");
        assert!(features.contains("ccache"), "{features:?}");
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
        // `-cxx` for a flag never enabled elsewhere must reach the per-package fold, which
        // overrides a `+cxx` IUSE default with it.
        std::fs::write(profile.join("make.defaults"), "USE=\"foo -cxx\"\n").unwrap();
        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack.use_flags(&mut shell, &[]).await.unwrap();
        let enabled: Vec<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        assert!(enabled.contains(&"foo"), "foo enabled");
        assert!(!enabled.contains(&"cxx"), "cxx not enabled");
        assert!(
            resolved.defaults.contains(&UseToken::user("-cxx").unwrap()),
            "explicit -cxx reaches the per-package fold"
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
        assert!(enabled.contains(&"cxx"), "cxx re-enabled by child");
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

    // Profile-injected USE_EXPAND defaults (the `elibc_glibc`/`kernel_linux`/
    // `python_targets_*` family: `profiles/base/make.defaults` sets
    // `USE_EXPAND="… ELIBC …"` + `ELIBC="glibc"`) must be translated into USE
    // tokens *inside the fold*, so they reach `ResolvedUse::defaults` and every
    // per-package `resolve_effective_use` downstream — dep conditionals like
    // `!elibc_glibc? ( dev-libs/libintl )` depend on it.
    #[tokio::test]
    async fn use_expand_defaults_reach_the_fold() {
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

        let on: HashSet<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        assert!(on.contains("elibc_glibc"), "ELIBC expanded: {on:?}");
        assert!(
            on.contains("python_targets_python3_13"),
            "PYTHON_TARGETS expanded: {on:?}"
        );
        assert!(on.contains("amd64"), "ARCH added unprefixed");
        assert!(on.contains("foo"), "plain USE kept");
    }

    // A `-*` in a child profile's own `USE=` *does* clear an ancestor's
    // USE_EXPAND expansions (`ELIBC`, `VIDEO_CARDS`, …) — corrected
    // 2026-08-10, live-verified against real portage 3.0.81.2. Not
    // `_lazy_use_expand` final-value semantics (that only reconstructs a
    // USE_EXPAND variable's *display* value, it doesn't compute the set):
    // `make_defaults_use` folds every profile layer's own USE_EXPAND tokens
    // plus its own `USE=` into one combined string per layer, joins all
    // layers, then runs one shared incremental fold — so a later layer's
    // `-*` wipes everything before it, expand-derived tokens included.
    #[tokio::test]
    async fn child_profile_wildcard_clears_ancestor_use_expand_expansion() {
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

        let on: Vec<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        assert!(
            resolved.defaults.contains(&UseToken::ClearAll),
            "the clear-all reaches the per-package fold"
        );
        assert!(on.contains(&"build"));
        assert!(
            !on.contains(&"elibc_glibc"),
            "child's -* wipes the parent's ELIBC expansion too, matching real portage: {on:?}"
        );
    }

    // Companion to the wildcard test above: without a `-*` in the way, a
    // child profile extending (not replacing) a parent's USE_EXPAND-listed
    // variable value gets the *union*, not last-wins — PMS 5.3.2 + live
    // portage 3.0.81.2 (`portage-repo/docs/pms-notes.md`). This is the
    // scenario that was actually broken before this fix: `VIDEO_CARDS`
    // went from `{dummy, fbdev}` (real portage) to just `{fbdev}` (old
    // `em`, last-assignment-wins across the whole profile chain).
    #[tokio::test]
    async fn profile_chain_unions_use_expand_values_without_conf_override() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let base = make_profile(&dir, "base", &[]);
        std::fs::write(
            base.join("make.defaults"),
            "USE_EXPAND=\"VIDEO_CARDS\"\nVIDEO_CARDS=\"dummy fbdev\"\n",
        )
        .unwrap();
        let arch = make_profile(&dir, "arch", &["../base"]);
        std::fs::write(arch.join("make.defaults"), "VIDEO_CARDS=\"fbdev\"\n").unwrap();

        let stack = ProfileStack::build(arch).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack.use_flags(&mut shell, &[]).await.unwrap();

        let on: HashSet<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        assert!(
            on.contains("video_cards_dummy"),
            "base's video_cards_dummy survives (union, not last-wins): {on:?}"
        );
        assert!(
            on.contains("video_cards_fbdev"),
            "arch's video_cards_fbdev also present: {on:?}"
        );
    }

    // Negation within the profile chain: a child's `-flag` on a
    // USE_EXPAND-listed variable cancels just that one token from an
    // ancestor, matching PMS 5.3.1's stack-and-cancel algorithm (and live
    // portage 3.0.81.2).
    #[tokio::test]
    async fn profile_chain_use_expand_negation_cancels_one_ancestor_token() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let base = make_profile(&dir, "base", &[]);
        std::fs::write(
            base.join("make.defaults"),
            "USE_EXPAND=\"VIDEO_CARDS\"\nVIDEO_CARDS=\"dummy fbdev\"\n",
        )
        .unwrap();
        let arch = make_profile(&dir, "arch", &["../base"]);
        std::fs::write(
            arch.join("make.defaults"),
            "VIDEO_CARDS=\"-dummy amdgpu\"\n",
        )
        .unwrap();

        let stack = ProfileStack::build(arch).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack.use_flags(&mut shell, &[]).await.unwrap();

        let on: HashSet<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        assert!(
            !on.contains("video_cards_dummy"),
            "arch's -dummy cancels base's video_cards_dummy: {on:?}"
        );
        assert!(
            on.contains("video_cards_fbdev"),
            "base's fbdev, untouched by arch, survives: {on:?}"
        );
        assert!(
            on.contains("video_cards_amdgpu"),
            "arch's own addition present: {on:?}"
        );
    }

    // Regression for the real bug this was found from: a descendant profile
    // (or make.conf) re-assigning a USE_EXPAND variable to a *disjoint*
    // value must fully replace the ancestor's value, not add to it —
    // `VIDEO_CARDS="dummy fbdev"` (base) → `VIDEO_CARDS="fbdev"` (arch) →
    // `VIDEO_CARDS="amdgpu"` (make.conf) must end with only
    // `video_cards_amdgpu`, matching real `portageq envvar VIDEO_CARDS`/`USE`
    // on a `default/linux/arm64` profile with `VIDEO_CARDS="amdgpu"` in
    // make.conf (`dummy`/`fbdev` never appear).
    #[tokio::test]
    async fn use_expand_final_value_replaces_ancestor_values_not_unions_them() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let base = make_profile(&dir, "base", &[]);
        std::fs::write(
            base.join("make.defaults"),
            "USE_EXPAND=\"VIDEO_CARDS\"\nVIDEO_CARDS=\"dummy fbdev\"\n",
        )
        .unwrap();
        let arch = make_profile(&dir, "arch", &["../base"]);
        std::fs::write(arch.join("make.defaults"), "VIDEO_CARDS=\"fbdev\"\n").unwrap();

        let make_conf = dir.path().join("make.conf");
        std::fs::write(&make_conf, "VIDEO_CARDS=\"amdgpu\"\n").unwrap();

        let stack = ProfileStack::build(arch).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack
            .use_flags(&mut shell, &[ConfSource::File(&make_conf)])
            .await
            .unwrap();

        let on: HashSet<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        assert!(
            on.contains("video_cards_amdgpu"),
            "final make.conf value present: {on:?}"
        );
        assert!(
            !on.contains("video_cards_dummy"),
            "base profile's overwritten value must not survive: {on:?}"
        );
        assert!(
            !on.contains("video_cards_fbdev"),
            "arch profile's overwritten value must not survive: {on:?}"
        );
    }

    // make.conf's own USE_EXPAND values are folded *after* its USE (portage's
    // conf-layer order), so they survive a `USE="-*"` in the same file while
    // the profile's expansions from the layer below are cleared.
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
            .use_flags(&mut shell, &[ConfSource::File(&make_conf)])
            .await
            .unwrap();

        let on: Vec<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        assert!(
            !on.contains(&"python_targets_python3_12"),
            "profile-layer expansion cleared by make.conf's -*: {on:?}"
        );
        assert!(
            on.contains(&"python_targets_python3_13"),
            "make.conf's own expand value folded after its USE: {on:?}"
        );
        assert!(on.contains(&"build"));
    }

    // make.conf assigning a USE_EXPAND-listed variable to the *empty
    // string* is how a user fully clears a group the profile stack built
    // up — it must still trigger the prefix-strip even though it
    // contributes zero tokens of its own (an explicit "assigned but
    // empty" must be told apart from "never mentioned by this layer", the
    // reason `source_incremental` resets these vars with `unset` rather
    // than `VAR=""`).
    #[tokio::test]
    async fn make_conf_empty_assignment_clears_profile_use_expand_group() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(
            profile.join("make.defaults"),
            "USE_EXPAND=\"VIDEO_CARDS\"\nVIDEO_CARDS=\"dummy fbdev\"\n",
        )
        .unwrap();
        let make_conf = dir.path().join("make.conf");
        std::fs::write(&make_conf, "VIDEO_CARDS=\"\"\n").unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack
            .use_flags(&mut shell, &[ConfSource::File(&make_conf)])
            .await
            .unwrap();

        let on: Vec<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        assert!(
            !on.contains(&"video_cards_dummy") && !on.contains(&"video_cards_fbdev"),
            "make.conf's explicit empty VIDEO_CARDS clears the whole group: {on:?}"
        );
    }

    // A group the profile set but the conf layer leaves alone is no assignment of that layer.
    #[tokio::test]
    async fn the_conf_layer_holds_only_what_it_assigned() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(
            profile.join("make.defaults"),
            "USE_EXPAND=\"L10N\"\nL10N=\"fr\"\n",
        )
        .unwrap();
        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        let resolved = stack
            .use_flags_with_override(&mut shell, &[], "USE=\"l10n_*\"\n")
            .await
            .unwrap();
        assert_eq!(resolved.conf, [UseToken::user("l10n_*").unwrap()]);
    }

    // A transient conf-layer override (`em stages --stage1`'s `USE="-* build ${BOOTSTRAP_USE}"`)
    // is one more conf file, not the environment: an env `-*` would wipe `package.use` too.
    #[tokio::test]
    async fn use_flags_with_override_is_a_conf_layer() {
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

        let on: Vec<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        assert_eq!(
            on,
            ["build"],
            "the override's own -* clears the defaults below"
        );
        assert!(
            resolved.env.is_empty(),
            "the override must not leak into env (the real process env): {:?}",
            resolved.env
        );
        assert!(
            resolved
                .defaults
                .contains(&UseToken::user("unrelated").unwrap()),
            "make.defaults stays in defaults"
        );
        assert_eq!(
            resolved.conf,
            [UseToken::ClearAll, UseToken::user("build").unwrap()],
            "the override is the conf layer, without defaults"
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
        configure_shell(&mut shell, &stack, &[ConfSource::File(&make_conf)])
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

    // Two-layer profile: base sets unicode, child sets crypt — both must survive.
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
