//! Rust builtin implementation of `inherit` for eclass sourcing.
//!
//! Reimplements the bash `inherit()` function as a Rust builtin to avoid
//! brush-core's variable scoping bug where arrays defined in a sourced file
//! become invisible after nested `source` calls within bash functions.
//!
//! A builtin's `source_script()` calls happen outside any bash function frame,
//! sidestepping the scoping issue entirely.
//!
//! Eclass ASTs are cached in a shared [`papaya::HashMap`] so that the same
//! eclass is only parsed once across all shells in a regen run.
//!
//! See [PMS 10.2](https://projects.gentoo.org/pms/9/pms.html#x1-10200010.2)
//! for eclass metadata variable accumulation.

use camino::Utf8PathBuf;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use brush_core::builtins;
use brush_parser::ast::Program;
use clap::Parser;

/// Accumulating metadata variables (PMS 10.2) for EAPI < 8.
const ACCUM_VARS_BASE: &[&str] = &[
    "IUSE",
    "REQUIRED_USE",
    "DEPEND",
    "BDEPEND",
    "RDEPEND",
    "PDEPEND",
    "IDEPEND",
];

/// All accumulating metadata variables for EAPI >= 8 (base + PROPERTIES + RESTRICT).
const ACCUM_VARS_ALL: &[&str] = &[
    "IUSE",
    "REQUIRED_USE",
    "DEPEND",
    "BDEPEND",
    "RDEPEND",
    "PDEPEND",
    "IDEPEND",
    "PROPERTIES",
    "RESTRICT",
];

/// Precomputed `E_*` variable names parallel to `ACCUM_VARS_BASE`.
pub(crate) const E_VARS_BASE: &[&str] = &[
    "E_IUSE",
    "E_REQUIRED_USE",
    "E_DEPEND",
    "E_BDEPEND",
    "E_RDEPEND",
    "E_PDEPEND",
    "E_IDEPEND",
];

/// Precomputed `E_*` variable names parallel to `ACCUM_VARS_ALL`.
pub(crate) const E_VARS_ALL: &[&str] = &[
    "E_IUSE",
    "E_REQUIRED_USE",
    "E_DEPEND",
    "E_BDEPEND",
    "E_RDEPEND",
    "E_PDEPEND",
    "E_IDEPEND",
    "E_PROPERTIES",
    "E_RESTRICT",
];

/// One sourced eclass: its name and the resolved file path it was loaded from.
///
/// Tracking the path is essential for cache writing/validation: an eclass may
/// come from a master repo's `eclass/` directory rather than the local repo,
/// and reconstructing the path from the name alone would search only the local
/// tree.
#[derive(Clone, Debug)]
pub(crate) struct InheritedEclass {
    pub(crate) name: String,
    pub(crate) path: Utf8PathBuf,
}

/// Persistent state for the `inherit` builtin.
///
/// - `inherited`: transitive list of eclasses sourced in this shell instance,
///   used for dedup within a single ebuild and for emitting `_eclasses_` in
///   the metadata cache.
/// - `cache`: a shared AST cache. All shells in a regen run share the same
///   underlying `papaya::HashMap` via `Arc`, so each eclass is parsed at most
///   once.
#[derive(Clone)]
pub(crate) struct InheritState {
    /// Transitive eclasses sourced so far in this shell, in inheritance order.
    pub(crate) inherited: Vec<InheritedEclass>,
    /// Shared AST cache keyed by eclass name.
    pub(crate) cache: Arc<papaya::HashMap<String, Program>>,
}

impl Default for InheritState {
    fn default() -> Self {
        Self {
            inherited: Vec::new(),
            cache: Arc::new(papaya::HashMap::new()),
        }
    }
}

/// Global counters for cache effectiveness diagnostics.
static CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static CACHE_MISSES: AtomicU64 = AtomicU64::new(0);

/// Source eclasses and manage metadata variable accumulation per PMS 10.
#[derive(Parser)]
pub(crate) struct InheritCommand {
    /// Eclass names to inherit.
    #[arg(required = true)]
    eclasses: Vec<String>,
}

impl builtins::Command for InheritCommand {
    type State = InheritState;
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        mut context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let eapi: u32 = context
            .shell
            .env_str("EAPI")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let (accum_vars, e_vars): (&[&str], &[&str]) = if eapi >= 8 {
            (ACCUM_VARS_ALL, E_VARS_ALL)
        } else {
            (ACCUM_VARS_BASE, E_VARS_BASE)
        };

        let is_top_level = get_var(context.shell, "ECLASS").is_empty();
        let mut inherit = get_var(context.shell, "INHERIT");

        let cache: Arc<papaya::HashMap<String, Program>> = self.state(&context)?.cache.clone();

        for eclass in &self.eclasses {
            let already_inherited = {
                let state = self.state(&context)?;
                state.inherited.iter().any(|e| &e.name == eclass)
            };
            if already_inherited {
                if is_top_level {
                    if !inherit.is_empty() {
                        inherit.push(' ');
                    }
                    inherit.push_str(eclass);
                    set_var(context.shell, "INHERIT", &inherit);
                }
                continue;
            }

            let eclass_file = find_eclass(context.shell, eclass);
            let Some(eclass_file) = eclass_file else {
                let _ = writeln!(
                    context.params.stderr(context.shell),
                    "die: inherit: eclass not found: {eclass}"
                );
                return Ok(brush_core::ExecutionResult::new(1));
            };

            let saved: Vec<(&'static str, String)> = accum_vars
                .iter()
                .map(|&var| {
                    let val = get_var(context.shell, var);
                    set_var(context.shell, var, "");
                    (var, val)
                })
                .collect();

            let prev_eclass = get_var(context.shell, "ECLASS");
            set_var(context.shell, "ECLASS", eclass);

            // Look up or parse the eclass AST. The papaya pin guard is
            // dropped before we .await, so no Send issues.
            let parser_options = context.shell.parser_options();
            let source_info = brush_core::SourceInfo::from(eclass_file.as_std_path().to_owned());
            let params = context.shell.default_exec_params();

            // Use pin_owned() for an owned guard that is Send — safe across .await.
            // get_or_insert_with is atomic: the closure runs at most once per key
            // even under concurrent worker access, avoiding duplicate parses.
            let pinned = cache.pin_owned();
            let mut was_miss = false;
            let program = pinned.get_or_insert_with(eclass.clone(), || {
                was_miss = true;
                parse_eclass_file(&eclass_file, &parser_options)
            });
            if was_miss {
                CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
            } else {
                CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            }

            let result = context
                .shell
                .source_program(program, &source_info, std::iter::empty::<&str>(), &params)
                .await;

            if let Err(e) = result {
                let _ = writeln!(
                    context.params.stderr(context.shell),
                    "die: inherit: failed to source {eclass}: {e}"
                );
                return Ok(brush_core::ExecutionResult::new(1));
            }

            set_var(context.shell, "ECLASS", &prev_eclass);

            for ((var, saved_val), &e_var) in saved.iter().zip(e_vars.iter()) {
                let contribution = get_var(context.shell, var);
                let e_val = get_var(context.shell, e_var);
                let new_e_val = match (e_val.is_empty(), contribution.is_empty()) {
                    (_, true) => e_val,
                    (true, false) => contribution,
                    (false, false) => format!("{e_val} {contribution}"),
                };
                set_var(context.shell, e_var, &new_e_val);
                set_var(context.shell, var, saved_val);
            }

            // Update state: push this eclass and sync $INHERITED.
            {
                let state = self.state_mut(&mut context)?;
                state.inherited.push(InheritedEclass {
                    name: eclass.clone(),
                    path: eclass_file.clone(),
                });
                let inherited_str = state
                    .inherited
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                set_var(context.shell, "INHERITED", &inherited_str);
            }

            if is_top_level {
                if !inherit.is_empty() {
                    inherit.push(' ');
                }
                inherit.push_str(eclass);
                set_var(context.shell, "INHERIT", &inherit);
            }
        }

        Ok(brush_core::ExecutionResult::success())
    }
}

/// Return (hits, misses) since process start.
pub fn cache_stats() -> (u64, u64) {
    (
        CACHE_HITS.load(Ordering::Relaxed),
        CACHE_MISSES.load(Ordering::Relaxed),
    )
}

/// Parse an eclass file into a `Program`.
pub(crate) fn parse_eclass_file(
    path: &Utf8PathBuf,
    options: &brush_parser::ParserOptions,
) -> Program {
    let mut buf = String::new();
    let mut file =
        std::fs::File::open(path).unwrap_or_else(|e| panic!("cannot open {}: {e}", path));
    file.read_to_string(&mut buf)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path));
    let mut parser = brush_parser::Parser::new(buf.as_bytes(), options);
    parser
        .parse_program()
        .unwrap_or_else(|e| panic!("parse error in {}: {e}", path))
}

fn get_var<SE: brush_core::ShellExtensions>(shell: &brush_core::Shell<SE>, name: &str) -> String {
    shell
        .env_str(name)
        .map(|cow| cow.into_owned())
        .unwrap_or_default()
}

fn set_var<SE: brush_core::ShellExtensions>(
    shell: &mut brush_core::Shell<SE>,
    name: &str,
    value: &str,
) {
    let _ = shell.set_env_global(
        name,
        brush_core::ShellVariable::new(brush_core::ShellValue::String(value.to_string())),
    );
}

fn find_eclass<SE: brush_core::ShellExtensions>(
    shell: &brush_core::Shell<SE>,
    name: &str,
) -> Option<Utf8PathBuf> {
    let dirs = shell.env_str("__PORTAGE_ECLASS_DIRS")?;
    let filename = format!("{name}.eclass");
    for dir in dirs.split(':') {
        let path = Utf8PathBuf::from(dir).join(&filename);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}
