//! The configuration of one config root, resolved once: every layer Portage stacks, kept as data
//!
//! Layers, lowest first: env.d's `profile.env`, `make.globals`, the profile stack's
//! `make.defaults`, `make.conf`, and the process environment. A plain variable takes the
//! highest layer that sets it; the incremental ones stack across all of them. The roots
//! (`ROOT`, `EROOT`, …) are not a layer: the caller adds them where it shows or exports them.

use std::collections::{BTreeMap, BTreeSet};

use super::profile::{
    ConfSource, Environment, ResolvedUse, Stage, merge_use_var, resolve_use_flags_observed,
    source_incremental,
};
use super::shell::EbuildShell;
use crate::error::Result;
use crate::repo::profile::ProfileStack;

/// Portage's incremental variables: stacked as sets across every layer and kept sorted
const INCREMENTALS: &[&str] = &[
    "ACCEPT_KEYWORDS",
    "CONFIG_PROTECT",
    "CONFIG_PROTECT_MASK",
    "ENV_UNSET",
    "FEATURES",
    "IUSE_IMPLICIT",
    "PROFILE_ONLY_VARIABLES",
    "USE",
    "USE_EXPAND",
    "USE_EXPAND_HIDDEN",
    "USE_EXPAND_IMPLICIT",
    "USE_EXPAND_UNPREFIXED",
];

/// Lists that keep their order and negations (see `merge_accept_lists`)
const ACCEPT_LISTS: &[&str] = &["ACCEPT_LICENSE", "ACCEPT_PROPERTIES", "ACCEPT_RESTRICT"];

/// Names that describe a package or a phase, never configuration, and bash's `_`: ignored in
/// every layer
const NOT_CONFIG: &[&str] = &[
    "_",
    "A",
    "AA",
    "BDEPEND",
    "BROOT",
    "CATEGORY",
    "D",
    "DEPEND",
    "DESCRIPTION",
    "DOCS",
    "EAPI",
    "EBUILD_PHASE",
    "ED",
    "EPREFIX",
    "EROOT",
    "ESYSROOT",
    "HOMEPAGE",
    "IDEPEND",
    "INHERITED",
    "IUSE",
    "IUSE_EFFECTIVE",
    "KEYWORDS",
    "LICENSE",
    "MERGE_TYPE",
    "P",
    "PDEPEND",
    "PF",
    "PN",
    "PORTAGE_CONFIGROOT",
    "PROPERTIES",
    "PV",
    "PVR",
    "RDEPEND",
    "REPLACED_BY_VERSION",
    "REPLACING_VERSIONS",
    "REQUIRED_USE",
    "RESTRICT",
    "ROOT",
    "S",
    "SLOT",
    "SRC_URI",
    "SYSROOT",
    "T",
    "WORKDIR",
];

/// Where a setting's value came from
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// `etc/profile.env`, written by env-update from `etc/env.d`
    EnvD,
    /// `make.globals`
    Globals,
    /// The profile stack's `make.defaults`
    Defaults,
    /// `make.conf`
    Conf,
    /// The process environment
    Env,
}

/// One variable's value and the highest layer that set it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    /// The stacked value; an incremental one is sorted
    pub value: String,
    /// The highest layer that set or changed it
    pub layer: Layer,
}

/// What [`Settings::evaluate`] reads, all chosen by the caller
pub struct SettingsSources<'a> {
    /// The profile stack, its user profiles included
    pub stack: &'a ProfileStack,
    /// `etc/profile.env`, if there is one
    pub env_d: Option<ConfSource<'a>>,
    /// `make.globals`, if there is one
    pub globals: Option<ConfSource<'a>>,
    /// `make.conf` and anything layered like it, in order
    pub confs: &'a [ConfSource<'a>],
    /// The process environment
    pub env: &'a Environment,
}

/// The resolved configuration of one config root
#[derive(Debug, Clone, Default)]
pub struct Settings {
    vars: BTreeMap<String, Setting>,
}

type Snapshot = BTreeMap<String, String>;

fn snapshot(shell: &EbuildShell) -> Snapshot {
    shell
        .var_names()
        .into_iter()
        .filter_map(|name| shell.get_var(&name).map(|value| (name, value)))
        .collect()
}

fn words(value: &str) -> impl Iterator<Item = &str> {
    value.split_whitespace()
}

impl Settings {
    /// Evaluate every layer in `shell`, a fresh one, and keep the result
    pub async fn evaluate(shell: &mut EbuildShell, sources: SettingsSources<'_>) -> Result<Self> {
        Ok(Self::evaluate_with_use(shell, sources, None).await?.0)
    }

    /// [`Self::evaluate`], also returning the layered USE fold a resolver starts from, with
    /// `extra_use_override` folded in as one more make.conf
    pub async fn evaluate_with_use(
        shell: &mut EbuildShell,
        sources: SettingsSources<'_>,
        extra_use_override: Option<&str>,
    ) -> Result<(Self, ResolvedUse)> {
        let initial = snapshot(shell);
        let mut stages: Vec<(Layer, Snapshot)> = Vec::new();
        for (layer, source) in [
            (Layer::EnvD, sources.env_d),
            (Layer::Globals, sources.globals),
        ] {
            if let Some(source) = source {
                source_incremental(shell, source).await?;
            }
            stages.push((layer, snapshot(shell)));
        }
        let mut observed: Vec<(Layer, Snapshot)> = Vec::new();
        let resolved = resolve_use_flags_observed(
            shell,
            sources.stack,
            sources.confs,
            extra_use_override,
            sources.env,
            &mut |stage, shell| {
                let layer = match stage {
                    Stage::Defaults => Layer::Defaults,
                    Stage::Conf => Layer::Conf,
                };
                observed.push((layer, snapshot(shell)));
            },
        )
        .await?;
        stages.extend(observed);
        // The USE evaluation applies the environment's USE and ACCEPT_* layer in the shell.
        stages.push((Layer::Env, snapshot(shell)));

        let defaults = stages
            .iter()
            .find(|(layer, _)| *layer == Layer::Defaults)
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        let profile_only: BTreeSet<String> = defaults
            .get("PROFILE_ONLY_VARIABLES")
            .map(|v| words(v).map(str::to_owned).collect())
            .unwrap_or_default();

        let mut vars: BTreeMap<String, Setting> = BTreeMap::new();
        let mut previous = &initial;
        for (layer, stage) in &stages {
            for (name, value) in stage {
                if previous.get(name) != Some(value) && !initial.contains_key(name) {
                    // A config file's newlines are spaces, as Portage reads it.
                    let value = match layer {
                        Layer::Env => value.clone(),
                        _ => value.replace('\n', " "),
                    };
                    vars.insert(
                        name.clone(),
                        Setting {
                            value,
                            layer: *layer,
                        },
                    );
                }
            }
            previous = stage;
        }
        let shell_final = &stages
            .last()
            .expect("the environment stage is always pushed")
            .1;

        // The rest of the environment, which the shell never sees.
        let use_expand: Vec<&str> = shell_final
            .get("USE_EXPAND")
            .map(|v| words(v).collect())
            .unwrap_or_default();
        let unprefixed: Vec<&str> = shell_final
            .get("USE_EXPAND_UNPREFIXED")
            .map(|v| words(v).collect())
            .unwrap_or_default();
        let in_shell_already = |name: &str| {
            name == "USE"
                || ACCEPT_LISTS.contains(&name)
                || use_expand.contains(&name)
                || unprefixed.contains(&name)
        };
        for (name, value) in sources.env.iter() {
            if in_shell_already(name) || profile_only.contains(name) {
                continue;
            }
            let value = if INCREMENTALS.contains(&name) {
                let below = vars.get(name).map(|s| s.value.as_str()).unwrap_or_default();
                merge_use_var(name, [below, value].into_iter()).join(" ")
            } else {
                value.to_owned()
            };
            vars.insert(
                name.to_owned(),
                Setting {
                    value,
                    layer: Layer::Env,
                },
            );
        }

        // Profile-only variables keep the profile's value whatever make.conf or the environment say.
        for name in &profile_only {
            match defaults.get(name) {
                Some(value) if !initial.contains_key(name) => {
                    vars.insert(
                        name.clone(),
                        Setting {
                            value: value.clone(),
                            layer: Layer::Defaults,
                        },
                    );
                }
                _ => {
                    vars.remove(name);
                }
            }
        }
        vars.retain(|name, _| !name.starts_with("__") && !NOT_CONFIG.contains(&name.as_str()));
        if !vars.contains_key("CBUILD")
            && let Some(chost) = vars.get("CHOST").cloned()
        {
            vars.insert("CBUILD".to_owned(), chost);
        }
        vars.entry("USERLAND".to_owned())
            .or_insert_with(|| Setting {
                value: if cfg!(any(
                    target_os = "freebsd",
                    target_os = "openbsd",
                    target_os = "netbsd",
                    target_os = "dragonfly"
                )) {
                    "BSD"
                } else {
                    "GNU"
                }
                .to_owned(),
                layer: Layer::Globals,
            });
        // Portage's own default when no layer accepts or denies anything.
        if vars
            .get("ACCEPT_LICENSE")
            .is_none_or(|s| s.value.is_empty())
        {
            vars.insert(
                "ACCEPT_LICENSE".to_owned(),
                Setting {
                    value: "* -@EULA".to_owned(),
                    layer: Layer::Globals,
                },
            );
        }

        let enabled: BTreeSet<&str> = resolved.enabled.iter().map(|f| f.as_str()).collect();
        if let Some(setting) = vars.get_mut("USE") {
            setting.value = enabled.iter().copied().collect::<Vec<_>>().join(" ");
        }
        // Each USE_EXPAND variable shows the flags USE ended up with, as Portage regenerates it.
        for &group in &use_expand {
            let prefix = format!("{}_", group.to_lowercase());
            let flags: BTreeSet<&str> = enabled
                .iter()
                .filter_map(|f| f.strip_prefix(prefix.as_str()))
                .collect();
            let set = vars.get(group).filter(|s| !s.value.is_empty()).cloned();
            if flags.is_empty() {
                match set {
                    Some(setting) => {
                        vars.insert(
                            group.to_owned(),
                            Setting {
                                value: String::new(),
                                ..setting
                            },
                        );
                    }
                    None => {
                        vars.remove(group);
                    }
                }
                continue;
            }
            let written: Vec<&str> = set
                .as_ref()
                .map(|s| words(&s.value).collect())
                .unwrap_or_default();
            let mut value: Vec<&str> = written
                .iter()
                .copied()
                .filter(|f| flags.contains(f))
                .collect();
            value.extend(flags.iter().copied().filter(|f| !written.contains(f)));
            let layer = set.as_ref().map_or(Layer::Defaults, |s| s.layer);
            vars.insert(
                group.to_owned(),
                Setting {
                    value: value.join(" "),
                    layer,
                },
            );
        }
        for &group in &unprefixed {
            if let Some(setting) = vars.get_mut(group) {
                setting.value = words(&setting.value)
                    .filter(|f| enabled.contains(f))
                    .collect::<Vec<_>>()
                    .join(" ");
            }
        }
        for &name in INCREMENTALS {
            if let Some(setting) = vars.get_mut(name) {
                let sorted: BTreeSet<&str> = words(&setting.value).collect();
                setting.value = sorted.into_iter().collect::<Vec<_>>().join(" ");
            }
        }
        Ok((Self { vars }, resolved))
    }

    /// The value of `name`, if any layer sets it
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(|s| s.value.as_str())
    }

    /// `name`'s value with the layer it came from
    pub fn setting(&self, name: &str) -> Option<&Setting> {
        self.vars.get(name)
    }

    /// Every variable, by name
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Setting)> {
        self.vars
            .iter()
            .map(|(name, setting)| (name.as_str(), setting))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Repository;

    struct Fixture {
        dir: tempfile::TempDir,
        repo: Repository,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
            std::fs::write(dir.path().join("metadata/layout.conf"), "").unwrap();
            std::fs::create_dir_all(dir.path().join("profiles/test")).unwrap();
            let repo = Repository::builder()
                .in_memory_cache()
                .open(dir.path())
                .unwrap();
            Self { dir, repo }
        }

        fn write(&self, file: &str, text: &str) -> std::path::PathBuf {
            let path = self.dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            path
        }

        async fn settings(&self, globals: &str, conf: &str, env: &[(&str, &str)]) -> Settings {
            self.settings_with_env_d("", globals, conf, env).await
        }

        async fn settings_with_env_d(
            &self,
            env_d: &str,
            globals: &str,
            conf: &str,
            env: &[(&str, &str)],
        ) -> Settings {
            let env_d = self.write("profile.env", env_d);
            let globals = self.write("make.globals", globals);
            let conf = self.write("make.conf", conf);
            let stack = ProfileStack::build(self.dir.path().join("profiles/test")).unwrap();
            let env: Environment = env.iter().copied().collect();
            let mut shell = self.repo.shell().await.unwrap();
            Settings::evaluate(
                &mut shell,
                SettingsSources {
                    stack: &stack,
                    env_d: Some(ConfSource::File(&env_d)),
                    globals: Some(ConfSource::File(&globals)),
                    confs: &[ConfSource::File(&conf)],
                    env: &env,
                },
            )
            .await
            .unwrap()
        }
    }

    #[tokio::test]
    async fn incrementals_stack_across_globals_profile_conf_and_environment() {
        let f = Fixture::new();
        f.write("profiles/test/make.defaults", "FEATURES=\"sandbox\"\n");
        let s = f
            .settings(
                "FEATURES=\"userpriv strict\"\n",
                "FEATURES=\"-strict ccache\"\n",
                &[("FEATURES", "-sandbox test")],
            )
            .await;
        assert_eq!(s.get("FEATURES"), Some("ccache test userpriv"));
        assert_eq!(s.setting("FEATURES").unwrap().layer, Layer::Env);
    }

    #[tokio::test]
    async fn env_d_is_the_lowest_layer_of_config_protection() {
        let f = Fixture::new();
        let s = f
            .settings_with_env_d(
                "export CONFIG_PROTECT='/usr/share/config'\nexport CONFIG_PROTECT_MASK='/etc/ca-certificates.conf /etc/x'\n",
                "CONFIG_PROTECT=\"/etc\"\nCONFIG_PROTECT_MASK=\"/etc/env.d\"\n",
                "CONFIG_PROTECT_MASK=\"-/etc/x\"\n",
                &[],
            )
            .await;
        assert_eq!(s.get("CONFIG_PROTECT"), Some("/etc /usr/share/config"));
        assert_eq!(
            s.get("CONFIG_PROTECT_MASK"),
            Some("/etc/ca-certificates.conf /etc/env.d")
        );
    }

    #[tokio::test]
    async fn the_highest_layer_wins_for_a_plain_variable() {
        let f = Fixture::new();
        f.write("profiles/test/make.defaults", "CHOST=\"profile\"\n");
        let s = f
            .settings(
                "PORTAGE_TMPDIR=\"/var/tmp\"\nCHOST=\"globals\"\n",
                "",
                &[("CFLAGS", "-O0")],
            )
            .await;
        assert_eq!(s.setting("PORTAGE_TMPDIR").unwrap().layer, Layer::Globals);
        assert_eq!(s.get("CHOST"), Some("profile"));
        assert_eq!(s.get("CFLAGS"), Some("-O0"));
        assert_eq!(s.get("ACCEPT_LICENSE"), Some("* -@EULA"));
    }

    #[tokio::test]
    async fn make_conf_and_the_environment_cannot_set_profile_only_variables() {
        let f = Fixture::new();
        f.write(
            "profiles/test/make.defaults",
            "ARCH=\"arm64\"\nPROFILE_ONLY_VARIABLES=\"ARCH\"\n",
        );
        let s = f.settings("", "ARCH=\"amd64\"\n", &[("ARCH", "x86")]).await;
        assert_eq!(s.get("ARCH"), Some("arm64"));
    }

    #[tokio::test]
    async fn use_expand_variables_follow_the_final_use() {
        let f = Fixture::new();
        f.write(
            "profiles/test/make.defaults",
            "USE_EXPAND=\"CPU_FLAGS_ARM VIDEO_CARDS ALSA_CARDS\"\nVIDEO_CARDS=\"fbdev\"\n",
        );
        f.write("profiles/test/use.force", "cpu_flags_arm_v8\n");
        let s = f.settings("", "", &[]).await;
        assert_eq!(s.get("CPU_FLAGS_ARM"), Some("v8"));
        assert_eq!(s.get("VIDEO_CARDS"), Some("fbdev"));
        assert_eq!(s.get("ALSA_CARDS"), None);
    }

    #[tokio::test]
    async fn the_shells_own_variables_are_not_settings() {
        let f = Fixture::new();
        let s = f.settings("", "__LOCAL=\"x\"\n", &[]).await;
        for name in ["BASHPID", "IFS", "PWD", "OPTIND", "__LOCAL"] {
            assert_eq!(s.get(name), None, "{name}");
        }
    }
}
