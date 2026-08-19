use brush_core::builtins;
use clap::Parser;

// ── ___eapi_* predicates ──────────────────────────────────────────────────────

/// All 74 `___eapi_*` EAPI predicate functions from portage's `eapi.sh`
///
/// Each checks whether a given EAPI has a specific feature.  Takes an
/// optional first argument to override `$EAPI`.
///
/// Registered under every `___eapi_*` name; dispatches via `command_name`.
#[derive(Parser)]
pub(crate) struct EapiPredicateCommand {
    eapi_override: Option<String>,
}

impl builtins::Command for EapiPredicateCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let eapi: u32 = if let Some(s) = &self.eapi_override {
            s.parse().unwrap_or(0)
        } else {
            context
                .shell
                .env_str("EAPI")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
        };
        let result = eapi_predicate(&context.command_name, eapi);
        Ok(brush_core::ExecutionResult::new(u8::from(!result)))
    }
}

fn eapi_predicate(name: &str, eapi: u32) -> bool {
    match name {
        "___eapi_default_src_test_disables_parallel_jobs" => eapi <= 4,
        "___eapi_has_S_WORKDIR_fallback" => eapi <= 3,
        "___eapi_has_pkg_pretend" => eapi >= 4,
        "___eapi_has_src_configure" => eapi >= 2,
        "___eapi_has_src_prepare" => eapi >= 2,
        "___eapi_has_BDEPEND" => eapi >= 7,
        "___eapi_has_BROOT" => eapi >= 7,
        "___eapi_has_IDEPEND" => eapi >= 8,
        "___eapi_has_PORTDIR_ECLASSDIR" => eapi <= 6,
        "___eapi_has_RDEPEND_DEPEND_fallback" => eapi <= 3,
        "___eapi_has_SYSROOT" => eapi >= 7,
        "___eapi_has_accumulated_PROPERTIES" => eapi >= 8,
        "___eapi_has_accumulated_RESTRICT" => eapi >= 8,
        "___eapi_has_prefix_variables" => eapi >= 3,
        "___eapi_has_assert" => eapi <= 8,
        "___eapi_has_docompress" => eapi >= 4,
        "___eapi_has_dohard" => eapi <= 3,
        "___eapi_has_doheader" => eapi >= 5,
        "___eapi_has_dohtml" => eapi <= 6,
        "___eapi_has_dolib_libopts" => eapi <= 6,
        "___eapi_has_domo" => eapi <= 8,
        "___eapi_has_dosed" => eapi <= 3,
        "___eapi_has_dostrip" => eapi >= 7,
        "___eapi_has_eapply" => eapi >= 6,
        "___eapi_has_eapply_user" => eapi >= 6,
        "___eapi_has_edo" => eapi >= 9,
        "___eapi_has_einstall" => eapi <= 5,
        "___eapi_has_einstalldocs" => eapi >= 6,
        "___eapi_has_get_libdir" => eapi >= 6,
        "___eapi_has_hasq" => eapi <= 7,
        "___eapi_has_hasv" => eapi <= 7,
        "___eapi_has_in_iuse" => eapi >= 6,
        "___eapi_has_nonfatal" => eapi >= 4,
        "___eapi_has_pipestatus" => eapi >= 9,
        "___eapi_has_useq" => eapi <= 7,
        "___eapi_has_usex" => eapi >= 5,
        "___eapi_has_ver_replacing" => eapi >= 9,
        "___eapi_has_version_functions" => eapi >= 7,
        "___eapi_best_version_and_has_version_support_--host-root" => eapi == 5 || eapi == 6,
        "___eapi_best_version_and_has_version_support_-b_-d_-r" => eapi >= 7,
        "___eapi_die_can_respect_nonfatal" => eapi >= 6,
        "___eapi_doconfd_respects_insopts" => eapi <= 7,
        "___eapi_dodoc_supports_-r" => eapi >= 4,
        "___eapi_doenvd_respects_insopts" => eapi <= 7,
        "___eapi_doheader_respects_insopts" => eapi <= 7,
        "___eapi_doinitd_respects_exeopts" => eapi <= 7,
        "___eapi_doins_and_newins_preserve_symlinks" => eapi >= 4,
        "___eapi_domo_respects_into" => eapi <= 6,
        "___eapi_econf_passes_--datarootdir" => eapi >= 8,
        "___eapi_econf_passes_--disable-dependency-tracking" => eapi >= 4,
        "___eapi_econf_passes_--disable-silent-rules" => eapi >= 5,
        "___eapi_econf_passes_--disable-static" => eapi >= 8,
        "___eapi_econf_passes_--docdir_and_--htmldir" => eapi >= 6,
        "___eapi_econf_passes_--with-sysroot" => eapi >= 7,
        "___eapi_has_DESTTREE_INSDESTTREE" => eapi <= 6,
        "___eapi_has_dosym_r" => eapi >= 8,
        "___eapi_helpers_can_die" => eapi >= 4,
        "___eapi_newins_supports_reading_from_standard_input" => eapi >= 5,
        "___eapi_unpack_is_case_sensitive" => eapi <= 5,
        "___eapi_unpack_supports_7z" => eapi <= 7,
        "___eapi_unpack_supports_absolute_paths" => eapi >= 6,
        "___eapi_unpack_supports_lha" => eapi <= 7,
        "___eapi_unpack_supports_rar" => eapi <= 7,
        "___eapi_unpack_supports_txz" => eapi >= 6,
        "___eapi_unpack_supports_xz" => eapi >= 3,
        "___eapi_use_enable_and_use_with_support_empty_third_argument" => eapi >= 4,
        "___eapi_usev_has_second_arg" => eapi >= 8,
        "___eapi_bash_3_2" => eapi <= 5,
        "___eapi_bash_4_2" => eapi == 6 || eapi == 7,
        "___eapi_bash_5_0" => eapi == 8,
        "___eapi_bash_5_3" => eapi >= 9,
        "___eapi_enables_failglob_in_global_scope" => eapi >= 6,
        "___eapi_has_ENV_UNSET" => eapi >= 7,
        "___eapi_has_strict_keepdir" => eapi >= 8,
        _ => false,
    }
}

/// All `___eapi_*` predicate names; used during builtin registration
pub(crate) const EAPI_PREDICATE_NAMES: &[&str] = &[
    "___eapi_default_src_test_disables_parallel_jobs",
    "___eapi_has_S_WORKDIR_fallback",
    "___eapi_has_pkg_pretend",
    "___eapi_has_src_configure",
    "___eapi_has_src_prepare",
    "___eapi_has_BDEPEND",
    "___eapi_has_BROOT",
    "___eapi_has_IDEPEND",
    "___eapi_has_PORTDIR_ECLASSDIR",
    "___eapi_has_RDEPEND_DEPEND_fallback",
    "___eapi_has_SYSROOT",
    "___eapi_has_accumulated_PROPERTIES",
    "___eapi_has_accumulated_RESTRICT",
    "___eapi_has_prefix_variables",
    "___eapi_has_assert",
    "___eapi_has_docompress",
    "___eapi_has_dohard",
    "___eapi_has_doheader",
    "___eapi_has_dohtml",
    "___eapi_has_dolib_libopts",
    "___eapi_has_domo",
    "___eapi_has_dosed",
    "___eapi_has_dostrip",
    "___eapi_has_eapply",
    "___eapi_has_eapply_user",
    "___eapi_has_edo",
    "___eapi_has_einstall",
    "___eapi_has_einstalldocs",
    "___eapi_has_get_libdir",
    "___eapi_has_hasq",
    "___eapi_has_hasv",
    "___eapi_has_in_iuse",
    "___eapi_has_nonfatal",
    "___eapi_has_pipestatus",
    "___eapi_has_useq",
    "___eapi_has_usex",
    "___eapi_has_ver_replacing",
    "___eapi_has_version_functions",
    "___eapi_best_version_and_has_version_support_--host-root",
    "___eapi_best_version_and_has_version_support_-b_-d_-r",
    "___eapi_die_can_respect_nonfatal",
    "___eapi_doconfd_respects_insopts",
    "___eapi_dodoc_supports_-r",
    "___eapi_doenvd_respects_insopts",
    "___eapi_doheader_respects_insopts",
    "___eapi_doinitd_respects_exeopts",
    "___eapi_doins_and_newins_preserve_symlinks",
    "___eapi_domo_respects_into",
    "___eapi_econf_passes_--datarootdir",
    "___eapi_econf_passes_--disable-dependency-tracking",
    "___eapi_econf_passes_--disable-silent-rules",
    "___eapi_econf_passes_--disable-static",
    "___eapi_econf_passes_--docdir_and_--htmldir",
    "___eapi_econf_passes_--with-sysroot",
    "___eapi_has_DESTTREE_INSDESTTREE",
    "___eapi_has_dosym_r",
    "___eapi_helpers_can_die",
    "___eapi_newins_supports_reading_from_standard_input",
    "___eapi_unpack_is_case_sensitive",
    "___eapi_unpack_supports_7z",
    "___eapi_unpack_supports_absolute_paths",
    "___eapi_unpack_supports_lha",
    "___eapi_unpack_supports_rar",
    "___eapi_unpack_supports_txz",
    "___eapi_unpack_supports_xz",
    "___eapi_use_enable_and_use_with_support_empty_third_argument",
    "___eapi_usev_has_second_arg",
    "___eapi_bash_3_2",
    "___eapi_bash_4_2",
    "___eapi_bash_5_0",
    "___eapi_bash_5_3",
    "___eapi_enables_failglob_in_global_scope",
    "___eapi_has_ENV_UNSET",
    "___eapi_has_strict_keepdir",
];

// ── __ebuild_phase_funcs ──────────────────────────────────────────────────────

/// `__ebuild_phase_funcs <eapi> <phase_func>`  (portage `phase-functions.sh`)
///
/// Sets up `default()` and `default_<phase_func>()` pointing at the correct
/// EAPI-versioned implementation, and installs a fallback `<phase_func>()`
/// that calls `default` if the ebuild did not define the phase itself.
#[derive(Parser)]
pub(crate) struct EbuildPhaseFuncsCommand {
    eapi_str: String,
    phase_func: String,
}

impl builtins::Command for EbuildPhaseFuncsCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let eapi: u32 = self.eapi_str.parse().unwrap_or(0);
        let phase = &self.phase_func;

        let mut script = String::new();

        if eapi <= 1 {
            // EAPI 0/1 has no 'default' mechanism; define missing phase funcs directly.
            for name in &["pkg_nofetch", "src_unpack", "src_test"] {
                if *name == phase {
                    script += &format!(
                        "declare -F {name} >/dev/null || {name}() {{ __eapi0_{name}; }}\n"
                    );
                }
            }
            if phase == "src_compile" {
                let impl_fn = if eapi == 0 {
                    "__eapi0_src_compile"
                } else {
                    "__eapi1_src_compile"
                };
                script += &format!(
                    "declare -F src_compile >/dev/null || src_compile() {{ {impl_fn}; }}\n"
                );
            }
        } else {
            // EAPI 2+: define default() and default_<phase>().
            script += &format!("default() {{ default_{phase}; }}\n");

            if let Some(impl_fn) = resolve_phase_default(eapi, phase) {
                script += &format!("default_{phase}() {{ {impl_fn}; }}\n");
            } else {
                script += &format!(
                    "default_{phase}() {{ die \"default_{phase} has no implementation in EAPI {eapi}\"; }}\n"
                );
            }

            // Install error stubs for default_<other_phase>() so they die if
            // called outside their own phase (PMS §12.1).
            for other in &[
                "pkg_nofetch",
                "src_unpack",
                "src_prepare",
                "src_configure",
                "src_compile",
                "src_test",
                "src_install",
            ] {
                if *other != phase {
                    script += &format!(
                        "default_{other}() {{ die \"default_{other} called outside its phase (current: {phase})\"; }}\n"
                    );
                }
            }

            // Install fallback only when the ebuild did not define the phase.
            // A phase with a PMS default implementation falls back to it; a
            // phase without one (the pkg_* lifecycle hooks) is a silent no-op,
            // as in portage — `default_<phase>` still dies if an ebuild calls
            // `default` there explicitly (PMS §12.1).
            let phase_defined = shell.funcs().get(phase.as_str()).is_some();
            if !phase_defined {
                if resolve_phase_default(eapi, phase).is_some() {
                    script += &format!("{phase}() {{ default; }}\n");
                } else {
                    script += &format!("{phase}() {{ :; }}\n");
                }
            }
        }

        let source_info = brush_core::SourceInfo::from("__ebuild_phase_funcs");
        let params = shell.default_exec_params();
        shell.run_string(&script, &source_info, &params).await?;

        Ok(brush_core::ExecutionResult::success())
    }
}

/// Return the name of the bash function that implements the default behaviour
/// for `phase_func` in the given EAPI.
fn resolve_phase_default(eapi: u32, phase_func: &str) -> Option<&'static str> {
    match phase_func {
        "pkg_nofetch" => Some("__eapi0_pkg_nofetch"),
        "src_unpack" => Some("__eapi0_src_unpack"),
        "src_test" => Some("__eapi0_src_test"),
        "src_configure" => Some("__eapi2_src_configure"),
        "src_compile" => Some("__eapi2_src_compile"),
        "src_prepare" => {
            if eapi >= 8 {
                Some("__eapi8_src_prepare")
            } else if eapi >= 6 {
                Some("__eapi6_src_prepare")
            } else {
                Some("__eapi2_src_prepare")
            }
        }
        "src_install" => {
            if eapi >= 6 {
                Some("__eapi6_src_install")
            } else if eapi >= 4 {
                Some("__eapi4_src_install")
            } else {
                None
            }
        }
        _ => None,
    }
}
