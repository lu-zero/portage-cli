//! Rust builtin implementation of `inherit` for eclass sourcing
//!
//! Reimplements the bash `inherit()` function as a Rust builtin to avoid
//! brush-core's variable scoping bug where arrays defined in a sourced file
//! become invisible after nested `source` calls within bash functions.
//!
//! A builtin's `source_script()` calls happen outside any bash function frame,
//! sidestepping the scoping issue entirely.
//!
//! Eclasses are read through the trees of the repository and its masters. Their ASTs are
//! cached in a shared [`papaya::HashMap`] so that each is parsed once across all shells in a
//! regen run.
//!
//! See [PMS 10.2](https://projects.gentoo.org/pms/9/pms.html#x1-10200010.2)
//! for eclass metadata variable accumulation.

use camino::Utf8Path;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use brush_core::builtins;

use super::die::DieFlag;
use crate::error::stopped_message;
use crate::tree::{EntryKind, TreeSource};
use brush_parser::ast::Program;

/// Accumulating metadata variables (PMS 10.2) for EAPI < 8
const ACCUM_VARS_BASE: &[&str] = &[
    "IUSE",
    "REQUIRED_USE",
    "DEPEND",
    "BDEPEND",
    "RDEPEND",
    "PDEPEND",
    "IDEPEND",
];

/// All accumulating metadata variables for EAPI >= 8 (base + PROPERTIES + RESTRICT)
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

/// Precomputed `E_*` variable names parallel to `ACCUM_VARS_BASE`
pub(crate) const E_VARS_BASE: &[&str] = &[
    "E_IUSE",
    "E_REQUIRED_USE",
    "E_DEPEND",
    "E_BDEPEND",
    "E_RDEPEND",
    "E_PDEPEND",
    "E_IDEPEND",
];

/// Precomputed `E_*` variable names parallel to `ACCUM_VARS_ALL`
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

/// An eclass's AST and the md5 of the text it was parsed from
pub(crate) type ParsedEclass = std::result::Result<(Program, md5::Digest), String>;

/// Parsed eclasses shared across shells, keyed by where each was read
pub(crate) type EclassCache = Arc<papaya::HashMap<String, ParsedEclass>>;

/// One sourced eclass: its name and the md5 of the text sourced, which `_eclasses_` records
#[derive(Clone, Debug)]
pub(crate) struct InheritedEclass {
    pub(crate) name: String,
    pub(crate) md5: md5::Digest,
}

/// Persistent state for the `inherit` builtin
///
/// - `inherited`: transitive list of eclasses sourced in this shell instance,
///   used for dedup within a single ebuild and for emitting `_eclasses_` in
///   the metadata cache.
/// - `cache`: a shared AST cache. All shells in a regen run share the same
///   underlying `papaya::HashMap` via `Arc`, so each eclass is parsed at most
///   once.
#[derive(Clone)]
pub(crate) struct InheritState {
    /// Transitive eclasses sourced so far in this shell, in inheritance order
    pub(crate) inherited: Vec<InheritedEclass>,
    /// Shared AST cache
    pub(crate) cache: EclassCache,
    /// Trees searched for `eclass/<name>.eclass`, in order
    pub(crate) trees: Vec<Arc<dyn TreeSource>>,
}

impl Default for InheritState {
    fn default() -> Self {
        Self {
            inherited: Vec::new(),
            cache: Arc::new(papaya::HashMap::new()),
            trees: Vec::new(),
        }
    }
}

/// Global counters for cache effectiveness diagnostics
static CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static CACHE_MISSES: AtomicU64 = AtomicU64::new(0);

/// Source eclasses and manage metadata variable accumulation per PMS 10
#[derive(winnow_args::Args)]
#[arg(disable_help_flag, disable_version_flag, disable_help_subcommand)]
pub(crate) struct InheritCommand {
    /// Eclass names to inherit
    #[arg(positional, stop_flags, required)]
    eclasses: Vec<String>,
}

brush_winnow_args_adapter::impl_parse_args!(InheritCommand);

impl<SE: brush_core::ShellExtensions> builtins::Builtin<SE> for InheritCommand {
    type Error = brush_core::Error;

    async fn execute(
        self,
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

        let (cache, trees) = {
            let state = context.state_mut::<InheritState>();
            (state.cache.clone(), state.trees.clone())
        };

        for eclass in &self.eclasses {
            let already_inherited = {
                let state = context.state_mut::<InheritState>();
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

            let Some((tree, rel)) = find_eclass(&trees, eclass) else {
                return Ok(die(
                    &context,
                    &format!("inherit: eclass not found: {eclass}"),
                ));
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
            let shown = tree.display(Utf8Path::new(&rel));
            let source_info = brush_core::SourceInfo::from(std::path::PathBuf::from(&shown));
            let params = context.shell.default_exec_params();

            // Use pin_owned() for an owned guard that is Send — safe across .await.
            // get_or_insert_with is atomic: the closure runs at most once per key
            // even under concurrent worker access, avoiding duplicate parses.
            let pinned = cache.pin_owned();
            let mut was_miss = false;
            let cached = pinned.get_or_insert_with(shown.clone(), || {
                was_miss = true;
                parse_eclass(&*tree, &rel, &parser_options)
            });
            if was_miss {
                CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
            } else {
                CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            }
            let (program, md5) = match cached {
                Ok((program, md5)) => (program, *md5),
                Err(error) => {
                    return Ok(die(
                        &context,
                        &format!("inherit: failed to parse {eclass}: {error}"),
                    ));
                }
            };

            let result = context
                .shell
                .source_program(program, &source_info, std::iter::empty::<&str>(), &params)
                .await;

            // Like `source eclass || die`: an eclass must finish at status 0 with normal flow.
            let failure = match result {
                Err(e) => Some(format!("inherit: failed to source {eclass}: {e}")),
                Ok(r) if r.is_success() && r.is_normal_flow() => None,
                Ok(r) => Some(stopped_message(&format!("inherit: sourcing {eclass}"), &r)),
            };
            if let Some(message) = failure {
                return Ok(die(&context, &message));
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
                let state = context.state_mut::<InheritState>();
                state.inherited.push(InheritedEclass {
                    name: eclass.clone(),
                    md5,
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

/// Return (hits, misses) since process start
pub fn cache_stats() -> (u64, u64) {
    (
        CACHE_HITS.load(Ordering::Relaxed),
        CACHE_MISSES.load(Ordering::Relaxed),
    )
}

/// Parse the eclass at `rel` in `tree`
fn parse_eclass(
    tree: &dyn TreeSource,
    rel: &str,
    options: &brush_parser::ParserOptions,
) -> ParsedEclass {
    let rel = Utf8Path::new(rel);
    let shown = tree.display(rel);
    let text = tree
        .read(rel)
        .map_err(|e| format!("cannot read {shown}: {e}"))?
        .ok_or_else(|| format!("{shown} disappeared"))?;
    let md5 = md5::compute(&text);
    let mut parser = brush_parser::Parser::new(&text[..], options);
    let program = parser
        .parse_program()
        .map_err(|e| format!("parse error in {shown}: {e}"))?;
    Ok((program, md5))
}

/// Raise `die` with `message` and fail the `inherit` call.
fn die<SE: brush_core::ShellExtensions>(
    context: &brush_core::ExecutionContext<'_, SE>,
    message: &str,
) -> brush_core::ExecutionResult {
    if let Some(flag) = context.shell.builtin_state::<DieFlag>() {
        flag.raise(message);
    }
    let _ = writeln!(context.params.stderr(context.shell), "die: {message}");
    brush_core::ExecutionResult::new(1)
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

/// The first tree holding `eclass/<name>.eclass`, with that path
fn find_eclass(trees: &[Arc<dyn TreeSource>], name: &str) -> Option<(Arc<dyn TreeSource>, String)> {
    let rel = format!("eclass/{name}.eclass");
    let tree = trees
        .iter()
        .find(|t| matches!(t.kind(Utf8Path::new(&rel)), Ok(Some(EntryKind::File))))?;
    Some((Arc::clone(tree), rel))
}
