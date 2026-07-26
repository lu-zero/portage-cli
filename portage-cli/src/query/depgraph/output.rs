use std::collections::HashMap;
use std::io::Write as _;

use anstyle::{AnsiColor, Effects, Style};
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    CededFlag, DepClass, DroppedDep, PortagePackage, UseConfig, UseFlagRequirement, UseFlagState,
    UseOverride, resolve_effective_use,
};
use portage_metadata::CacheEntry;

pub(super) use crate::style::C_PKG;

// emerge color scheme: bold green for keywords/atoms/tags, bold red/blue for flags
// Package names use plain green (not bold) to match portage's PKG_MERGE style
// (C_PKG now centralized in cli.rs to prevent duplication/stomping between agents).
pub(super) const C_ON: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)))
    .effects(Effects::BOLD);
pub(super) const C_OFF: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Blue)))
    .effects(Effects::BOLD);
// A flag whose state flips relative to what is installed (suffix `*`), and one
// the installed build never had in IUSE at all (suffix `%`). Portage gives
// these their own colours precisely so a changed flag is findable in a wall of
// unchanged ones.
pub(super) const C_FLIPPED: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);
pub(super) const C_NEW_FLAG: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)))
    .effects(Effects::BOLD);
// Portage-style colors for emerge -p output:
// - BRACKET: blue for [ and ] in [ebuild STATUS]
// - STATUS_N/S: green for new/new-slot
// - STATUS_U: cyan for upgrade
// - STATUS_D: blue for downgrade
// - STATUS_R: yellow for reinstall
pub(super) const C_BRACKET: Style =
    Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Blue)));
pub(super) const C_STATUS_N: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);
pub(super) const C_STATUS_S: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);
pub(super) const C_STATUS_U: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan)))
    .effects(Effects::BOLD);
pub(super) const C_STATUS_D: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Blue)))
    .effects(Effects::BOLD);
pub(super) const C_STATUS_R: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)))
    .effects(Effects::BOLD);

use super::installed::action_tag;
use super::repo::{FilterReason, RepoData, find_cache};

pub(super) fn report_conflicts(conflicts: &[super::conflicts::Conflict]) {
    use std::collections::BTreeMap;
    // Group by the package whose version is in conflict.
    let mut by_target: BTreeMap<String, Vec<&super::conflicts::Conflict>> = BTreeMap::new();
    for c in conflicts {
        by_target.entry(c.dep.cpn.to_string()).or_default().push(c);
    }
    let mut out = anstream::stderr();
    writeln!(
        out,
        "\n{C_OFF}!!!{C_OFF:#} Dependency constraint conflict(s) detected:\n"
    )
    .ok();
    for (target, cs) in &by_target {
        writeln!(out, "  {C_PKG}{target}{C_PKG:#}").ok();
        for c in cs {
            writeln!(
                out,
                "    ({C_PKG}{}-{}{C_PKG:#}, installed) requires {C_OFF}{}{C_OFF:#}",
                c.installed_cpn, c.installed_ver, c.dep,
            )
            .ok();
            writeln!(
                out,
                "    proposed: {C_PKG}{target}-{}{C_PKG:#}",
                c.proposed_ver,
            )
            .ok();
        }
        writeln!(out).ok();
    }
}

/// Report blocker (`!`/`!!`) and `::repo` violations detected post-solve.
/// The solver does not model these, so they are surfaced here like slot
/// conflicts rather than failing resolution.
pub(super) fn report_solver_violations(violations: &[portage_atom_pubgrub::Error]) {
    use portage_atom_pubgrub::Error;
    let mut out = anstream::stderr();

    let blockers: Vec<&Error> = violations
        .iter()
        .filter(|e| matches!(e, Error::BlockerConflict { .. }))
        .collect();
    if !blockers.is_empty() {
        writeln!(out, "\n{C_OFF}!!!{C_OFF:#} Blocker conflict(s) detected:\n").ok();
        for e in blockers {
            if let Error::BlockerConflict {
                pkg,
                blocker,
                strength,
            } = e
            {
                writeln!(
                    out,
                    "  {C_PKG}{pkg}{C_PKG:#} blocks {C_OFF}{blocker}{C_OFF:#} ({strength})",
                )
                .ok();
            }
        }
    }

    let repos: Vec<&Error> = violations
        .iter()
        .filter(|e| matches!(e, Error::RepoConstraintConflict(..)))
        .collect();
    if !repos.is_empty() {
        writeln!(
            out,
            "\n{C_OFF}!!!{C_OFF:#} Repository constraint conflict(s) detected:\n"
        )
        .ok();
        for e in repos {
            if let Error::RepoConstraintConflict(pkg, msg) = e {
                writeln!(out, "  {C_PKG}{pkg}{C_PKG:#}: {msg}").ok();
            }
        }
    }
}

/// A root target no acceptable ebuild satisfies.
pub(super) struct UnsatisfiableTarget {
    /// The atom as written.
    pub atom: String,
    /// Where the atom came from, for the `(dependency required by …)` trailer.
    pub origin: super::TargetOrigin,
    /// Whether the tree has matching-but-filtered versions, or none at all.
    pub problem: super::targets::TargetProblem,
    /// The matching versions and why each was filtered out. Empty for
    /// [`TargetProblem::NoEbuilds`](super::targets::TargetProblem::NoEbuilds).
    pub reasons: Vec<super::repo::AutounmaskCandidate>,
}

/// Portage's `(masked by: …)` parenthetical for one filtered version.
fn masked_by(reasons: &[FilterReason]) -> String {
    let parts: Vec<String> = reasons
        .iter()
        .map(|r| match r {
            // `keyword_needed` reports `**` when the arch has no keyword at all.
            FilterReason::Keyword(kw) if kw == "**" => "missing keyword".to_string(),
            FilterReason::Keyword(kw) => format!("{kw} keyword"),
            FilterReason::Masked => "package.mask".to_string(),
            FilterReason::License(l) => format!("{} license(s)", l.join(" ")),
        })
        .collect();
    parts.join(", ")
}

/// The candidate lines under an "all ebuilds … have been masked" header.
fn masked_candidates(target: &UnsatisfiableTarget, data: &RepoData) -> Vec<String> {
    target
        .reasons
        .iter()
        .map(|c| {
            format!(
                "- {}-{}::{} (masked by: {})",
                c.cpv.cpn,
                c.cpv.version,
                super::repo::repo_name_of(data, &c.cpv),
                masked_by(&c.reasons)
            )
        })
        .collect()
}

/// The fatal message for a root target that cannot be satisfied — an explicit
/// atom, or a member of a user-defined set (portage's `_resolve` only spares
/// the `@selected`/`@system`/`@world` family).
pub(super) fn unsatisfiable_target_message(
    target: &UnsatisfiableTarget,
    data: &RepoData,
    multi_repo: bool,
) -> String {
    use super::targets::TargetProblem;
    let atom = &target.atom;
    let mut msg = match target.problem {
        TargetProblem::NoEbuilds => format!(
            "no ebuilds found for '{atom}' (searched ::{}{})",
            data.repo_name,
            if multi_repo { " and overlays" } else { "" },
        ),
        TargetProblem::AllFiltered => {
            let mut msg = format!(
                "all ebuilds that could satisfy '{atom}' have been masked.\n\
                 one of the following masked packages is required to complete your request:"
            );
            for line in masked_candidates(target, data) {
                msg.push_str("\n  ");
                msg.push_str(&line);
            }
            msg
        }
    };
    if let Some(trailer) = target.origin.trailer() {
        msg.push('\n');
        msg.push_str(&trailer);
    }
    msg
}

/// Report the world-family root targets dropped from the solve because nothing
/// acceptable satisfies them. Advisory only — the plan still stands and the run
/// still exits 0, matching emerge.
///
/// A selective resolve prints the summary alone: an installed instance already
/// satisfies the atom, so the per-candidate detail is noise (emerge's
/// `_show_unsatisfied_dep` is reached only on the non-selective path).
pub(super) fn report_unsatisfiable_targets(
    targets: &[UnsatisfiableTarget],
    data: &RepoData,
    selective: bool,
) {
    use super::targets::TargetProblem;
    let mut out = anstream::stderr();
    let atoms: Vec<&str> = targets.iter().map(|t| t.atom.as_str()).collect();
    writeln!(
        out,
        "\n{C_OFF}!!!{C_OFF:#} Ebuilds for the following packages are either all\n\
         {C_OFF}!!!{C_OFF:#} masked or don't exist:\n{C_PKG}{}{C_PKG:#}",
        atoms.join(" ")
    )
    .ok();
    if selective {
        return;
    }
    for t in targets {
        match t.problem {
            TargetProblem::NoEbuilds => {
                writeln!(
                    out,
                    "\nem: there are no ebuilds to satisfy \"{C_PKG}{}{C_PKG:#}\".",
                    t.atom
                )
                .ok();
            }
            TargetProblem::AllFiltered => {
                writeln!(
                    out,
                    "\n{C_OFF}!!!{C_OFF:#} All ebuilds that could satisfy \
                     \"{C_PKG}{}{C_PKG:#}\" have been masked.\n\
                     {C_OFF}!!!{C_OFF:#} One of the following masked packages is required \
                     to complete your request:",
                    t.atom
                )
                .ok();
                for line in masked_candidates(t, data) {
                    writeln!(out, "{line}").ok();
                }
            }
        }
        if let Some(trailer) = t.origin.trailer() {
            writeln!(out, "{trailer}").ok();
        }
    }
}

/// Report `REQUIRED_USE` constraints left unsatisfied by the planned USE.
/// Mirrors emerge's "following REQUIRED_USE flag constraints are unsatisfied".
pub(super) fn report_required_use(violations: &[super::required_use::RequiredUseViolation]) {
    let mut out = anstream::stderr();
    writeln!(
        out,
        "\n{C_OFF}!!!{C_OFF:#} The following REQUIRED_USE flag constraints are unsatisfied:\n"
    )
    .ok();
    for v in violations {
        writeln!(out, "  {C_PKG}{}-{}{C_PKG:#}", v.cpv.cpn, v.cpv.version).ok();
        for clause in &v.unsatisfied {
            writeln!(out, "    {C_OFF}{clause}{C_OFF:#}").ok();
        }
    }
}

/// Report the USE flags `--autosolve-use` flipped to satisfy `REQUIRED_USE`.
///
/// Flips are grouped onto each in-plan `cpv` (the version the synthetic
/// `package.use` entry keys on), and each block shows the package's
/// `REQUIRED_USE` so the user can see *why* the flag had to move, plus the
/// value their configuration had asked for.
pub(super) fn report_autosolved_use<'a>(
    flips: &[&CededFlag],
    solution: impl IntoIterator<Item = (&'a PortagePackage, &'a Version)>,
    data: &RepoData,
) {
    use std::collections::BTreeMap;

    let mut by_cpn: HashMap<Cpn, Vec<&CededFlag>> = HashMap::new();
    for c in flips {
        by_cpn.entry(c.cpn).or_default().push(c);
    }

    // A flip on a CPN applies to every in-plan version of it (the synthetic
    // package.use above keys per cpv); list each cpv so the report is actionable.
    // BTreeMap keeps the output stable across runs.
    type Block<'a> = (
        Option<&'a portage_metadata::RequiredUseExpr>,
        Vec<&'a CededFlag>,
    );
    let mut blocks: BTreeMap<String, Block> = BTreeMap::new();
    for (pkg, ver) in solution {
        if pkg.is_virtual() {
            continue;
        }
        let Some(pkg_flips) = by_cpn.get(pkg.cpn()) else {
            continue;
        };
        let cpv = format!("{}/{}-{}", pkg.cpn().category, pkg.cpn().package, ver);
        let ru = find_cache(data, pkg, ver).and_then(|c| c.metadata.required_use.as_ref());
        blocks.insert(cpv, (ru, pkg_flips.clone()));
    }
    if blocks.is_empty() {
        return;
    }

    let mut out = anstream::stderr();
    writeln!(
        out,
        "\n{C_PKG}***{C_PKG:#} --autosolve-use adjusted USE flags to satisfy REQUIRED_USE:\n"
    )
    .ok();
    for (cpv, (ru, pkg_flips)) in &blocks {
        writeln!(out, "  {C_PKG}{cpv}{C_PKG:#}").ok();
        for c in pkg_flips {
            let (sign, style) = if c.value { ("+", C_ON) } else { ("-", C_OFF) };
            let configured = if c.value { "off" } else { "on" };
            writeln!(
                out,
                "    {style}{sign}{}{style:#}  {C_OFF}(configured {configured}){C_OFF:#}",
                c.flag.as_str()
            )
            .ok();
        }
        // Show only the REQUIRED_USE clauses that mention a flipped flag — the
        // full constraint can be enormous (e.g. qtbase) and bury the relevant
        // part; deduplicate so two flips sharing a clause print it once.
        if let Some(ru) = ru {
            let mut shown = std::collections::BTreeSet::new();
            for clause in ru.clauses() {
                if pkg_flips.iter().any(|c| clause.mentions(c.flag.as_str()))
                    && shown.insert(clause.to_string())
                {
                    writeln!(out, "    {C_OFF}because:{C_OFF:#} {clause}").ok();
                }
            }
        }
    }
}

/// A ceded USE decision applied to more than one in-plan slot of the same
/// package: `(cpn, slots in the plan, decided flags)`.
pub(super) type SharedSlotDecision = (Cpn, Vec<String>, Vec<String>);

/// Find ceded USE decisions shared across multiple in-plan slots.
///
/// `UseDecision` nodes are keyed per `(cpn, flag)` — slot-agnostic, because
/// cross-package USE-dep references (`Q[flag]`) and the C7 co-solve address
/// packages without a slot. When two slots of the same package are both in the
/// plan, one solver decision binds both, but Portage configures USE per
/// version — each slot may legitimately want a different value. That case is
/// surfaced as a Tier-2 advisory rather than solved per-slot (C5).
pub(super) fn shared_slot_decisions<'a>(
    ceded: &[CededFlag],
    solution: impl IntoIterator<Item = (&'a PortagePackage, &'a Version)>,
) -> Vec<SharedSlotDecision> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut flags_by_cpn: BTreeMap<Cpn, BTreeSet<String>> = BTreeMap::new();
    for c in ceded {
        flags_by_cpn
            .entry(c.cpn)
            .or_default()
            .insert(c.flag.as_str().to_string());
    }
    let mut slots_by_cpn: BTreeMap<Cpn, BTreeSet<String>> = BTreeMap::new();
    for (pkg, _) in solution {
        if pkg.is_virtual() || !flags_by_cpn.contains_key(pkg.cpn()) {
            continue;
        }
        let slot = pkg.slot().map_or_else(String::new, |s| s.as_str().into());
        slots_by_cpn.entry(*pkg.cpn()).or_default().insert(slot);
    }
    slots_by_cpn
        .into_iter()
        .filter(|(_, slots)| slots.len() >= 2)
        .map(|(cpn, slots)| {
            let flags = flags_by_cpn.remove(&cpn).unwrap_or_default();
            (
                cpn,
                slots.into_iter().collect(),
                flags.into_iter().collect(),
            )
        })
        .collect()
}

/// Advisory for [`shared_slot_decisions`]: the auto-solved values were applied
/// to every listed slot; per-slot differences need explicit `package.use`.
pub(super) fn report_shared_slot_use_decisions(shared: &[SharedSlotDecision]) {
    let mut out = anstream::stderr();
    writeln!(
        out,
        "\n{C_PKG}***{C_PKG:#} --autosolve-use note: USE decisions are shared across slots:\n"
    )
    .ok();
    for (cpn, slots, flags) in shared {
        let slots = slots
            .iter()
            .map(|s| format!(":{s}"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            out,
            "  {C_PKG}{cpn}{C_PKG:#}  slots {slots}  (flags: {})",
            flags.join(", ")
        )
        .ok();
        writeln!(
            out,
            "    {C_OFF}the same value was applied to every slot; use per-slot package.use entries if a slot needs a different value{C_OFF:#}"
        )
        .ok();
    }
}

pub(super) fn report_dropped_deps(dropped: &[DroppedDep], data: &RepoData, arch: &str) {
    // These are || alternatives bypassed by resolution — not failures.
    // Deduplicate by package and merge their alternatives across all occurrences.
    use std::collections::BTreeMap;
    let mut by_pkg: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
    let mut in_tree: std::collections::HashMap<String, bool> = Default::default();

    for d in dropped {
        if d.package.is_virtual() {
            continue;
        }
        let pkg_str = d.package.cpn_str();
        let alts = by_pkg.entry(pkg_str.clone()).or_default();
        for a in &d.alternatives {
            if !a.is_virtual() {
                alts.insert(a.cpn_str());
            }
        }
        in_tree
            .entry(pkg_str)
            .or_insert_with(|| data.versions.contains_key(d.package.cpn()));
    }

    for (pkg_str, alts) in &by_pkg {
        let reason = if *in_tree.get(pkg_str.as_str()).unwrap_or(&false) {
            format!("no {arch} keywords")
        } else {
            "not in tree".to_string()
        };
        let alt_str = if alts.is_empty() {
            String::new()
        } else {
            format!(
                ", alternatives: {}",
                alts.iter().cloned().collect::<Vec<_>>().join(" | ")
            )
        };
        eprintln!("note: dropped {pkg_str} ({reason}){alt_str}");
    }
}

/// The installed build a candidate's USE flags are diffed against.
///
/// Portage's three-tier fallback (`_emerge/resolver/output.py:683-694`): the
/// exact CPV if it is installed (catches a SLOT change of the same version),
/// else the same slot, else the **highest** instance in any other slot. That
/// last tier is what lets a new-slot install still report what changed —
/// without it an `NS` row has nothing to compare to and dumps every flag.
fn previous_entry<'a>(
    pkg: &PortagePackage,
    ver: &Version,
    installed: &'a [super::installed::VdbEntry],
) -> Option<&'a super::installed::VdbEntry> {
    let same_cpn = || installed.iter().filter(|e| e.cpn == *pkg.cpn());
    same_cpn()
        .find(|e| e.version == *ver)
        .or_else(|| same_cpn().find(|e| e.slot == pkg.slot()))
        .or_else(|| same_cpn().max_by(|a, b| a.version.cmp(&b.version)))
}

/// How one flag renders, relative to the installed build it is compared against.
///
/// Ports portage's `_create_use_string`
/// (`_emerge/resolver/output_helpers.py:262`), which decides both *which* flags
/// appear and how each is marked:
///
/// | Flag | Rendered |
/// |---|---|
/// | on, unchanged | `flag` (red) — only when `all_flags` or required |
/// | on, absent from the old IUSE | `flag%*` (yellow) |
/// | on, previously off | `flag*` (green) |
/// | off, unchanged | `-flag` (blue) — only when `all_flags` or required |
/// | off, absent from the old IUSE | `-flag%` (yellow) |
/// | off, previously on | `-flag*` (green) |
/// | dropped from IUSE entirely | `(-flag%)`/`(-flag%*)` (yellow), only when `all_flags` or required |
///
/// Anything not listed is omitted, which is why a plain `em -p` on an installed
/// package shows only what actually changes. When nothing comparable is
/// installed every flag is shown, unmarked.
fn flag_token(
    name: &str,
    enabled: bool,
    in_cur_iuse: bool,
    in_old_iuse: bool,
    in_old_use: bool,
    is_new: bool,
    show_unchanged: bool,
) -> Option<(String, bool)> {
    // Dropped from IUSE: reported as disabled, parenthesised, never as a plain
    // absence — the flag is gone, not turned off.
    if !in_cur_iuse {
        if !show_unchanged {
            return None;
        }
        let star = if in_old_use { "*" } else { "" };
        return Some((format!("({C_NEW_FLAG}-{name}%{star}{C_NEW_FLAG:#})"), false));
    }
    let token = if enabled {
        if is_new || (in_old_use && show_unchanged) {
            format!("{C_ON}{name}{C_ON:#}")
        } else if !in_old_iuse {
            format!("{C_NEW_FLAG}{name}%*{C_NEW_FLAG:#}")
        } else if !in_old_use {
            format!("{C_FLIPPED}{name}*{C_FLIPPED:#}")
        } else {
            return None;
        }
    } else if is_new || (in_old_iuse && !in_old_use && show_unchanged) {
        format!("{C_OFF}-{name}{C_OFF:#}")
    } else if !in_old_iuse {
        format!("{C_NEW_FLAG}-{name}%{C_NEW_FLAG:#}")
    } else if in_old_use {
        format!("{C_FLIPPED}-{name}*{C_FLIPPED:#}")
    } else {
        return None;
    };
    Some((token, enabled))
}

/// Format USE flags for display, diffed against `previous` — the installed
/// build portage would compare to (see [`previous_entry`]). `all_flags` (`-v`)
/// additionally shows flags that did not change.
fn format_flags(
    cache: &CacheEntry,
    effective_use: &UseConfig,
    use_expand: &[String],
    use_expand_hidden: &[String],
    req: Option<&UseFlagRequirement>,
    previous: Option<&super::installed::VdbEntry>,
    all_flags: bool,
) -> String {
    use std::collections::HashSet;

    // Each entry: (enabled_tokens, disabled_tokens).  BTreeMap keeps groups sorted.
    let mut base_flags: (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut expand_groups: std::collections::BTreeMap<&str, (Vec<String>, Vec<String>)> =
        std::collections::BTreeMap::new();

    let cur_iuse: HashSet<Interned<DefaultInterner>> = cache
        .metadata
        .iuse
        .iter()
        .map(|f| Interned::intern(f.name()))
        .collect();
    let old_iuse: HashSet<Interned<DefaultInterner>> = previous
        .map(|p| p.iuse.iter().copied().collect())
        .unwrap_or_default();
    // Portage restricts old_use to old_iuse (output.py:212): a recorded USE
    // flag the old build didn't declare can't have been a real choice.
    let old_use: HashSet<Interned<DefaultInterner>> = previous
        .map(|p| {
            p.active_use
                .iter()
                .copied()
                .filter(|f| old_iuse.contains(f))
                .collect()
        })
        .unwrap_or_default();
    let is_new = previous.is_none();

    // Sort by flag name *before* rendering: the tokens carry colour escapes
    // now, so sorting rendered strings would order by escape sequence.
    let mut any_iuse: Vec<_> = cur_iuse.union(&old_iuse).copied().collect();
    any_iuse.sort_by(|a, b| a.as_str().cmp(b.as_str()));

    for interned in any_iuse {
        let name = interned.as_str();
        let in_cur_iuse = cur_iuse.contains(&interned);
        // `effective_use` already folded this package's IUSE defaults in (via
        // `resolve_effective_use`), so every mentioned-or-defaulted flag is
        // resolved; nothing left unset needs a fallback here.
        let mut enabled =
            in_cur_iuse && matches!(effective_use.get(interned), UseFlagState::Enabled);

        // A flag the solver requires is one the user needs to see whether or
        // not it changed — portage's `reinst_flags`. Apply the required state
        // too: it is enforced at build time, so this is the post-install state.
        let required = req.is_some_and(|r| {
            r.required_enabled.contains(&interned) || r.required_disabled.contains(&interned)
        });
        if let Some(r) = req {
            if r.required_enabled.contains(&interned) {
                enabled = true;
            } else if r.required_disabled.contains(&interned) {
                enabled = false;
            }
        }

        let Some((token, is_enabled)) = flag_token(
            name,
            enabled,
            in_cur_iuse,
            old_iuse.contains(&interned),
            old_use.contains(&interned),
            is_new,
            all_flags || required,
        ) else {
            continue;
        };

        let expand_match = use_expand.iter().find(|key| {
            let prefix = format!("{}_", key.to_lowercase());
            name.starts_with(prefix.as_str())
        });

        if let Some(key) = expand_match {
            let prefix = format!("{}_", key.to_lowercase());
            let short = &name[prefix.len()..];
            // Re-render inside the group with the prefix stripped.
            let token = token.replacen(name, short, 1);
            let bucket = expand_groups.entry(key.as_str()).or_default();
            if is_enabled {
                bucket.0.push(token);
            } else {
                // Wrap disabled USE_EXPAND flags in parentheses
                bucket.1.push(if token.starts_with('(') {
                    token
                } else {
                    format!("({token})")
                });
            }
        } else if is_enabled {
            base_flags.0.push(token);
        } else {
            base_flags.1.push(token);
        }
    }

    let join_bucket = |(on, off): &(Vec<String>, Vec<String>)| -> String {
        // Already in flag-name order; enabled before disabled, as portage does.
        on.iter().chain(off).cloned().collect::<Vec<_>>().join(" ")
    };

    let mut parts = Vec::new();
    let base_str = join_bucket(&base_flags);
    if !base_str.is_empty() {
        parts.push(format!("USE=\"{base_str}\""));
    }
    for (key, bucket) in &expand_groups {
        if use_expand_hidden.iter().any(|h| h == *key) {
            continue;
        }
        parts.push(format!("{}=\"{}\"", key, join_bucket(bucket)));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  {}", parts.join(" "))
    }
}

/// Build the `:slot/subslot::repo` suffix shown in verbose mode.
///
/// Mirrors portage: show `:slot/subslot` when the subslot differs from the
/// slot, else `:slot` when the slot isn't the default `0`, else nothing —
/// always followed by `::repo`.
fn slot_repo_suffix(cache: &CacheEntry, repo_name: &str) -> String {
    let slot = cache.metadata.slot.slot.as_str();
    let subslot = cache.metadata.slot.subslot.map(|s| s.as_str().to_string());
    let mut s = String::new();
    match subslot {
        Some(sub) if sub != slot => s.push_str(&format!(":{slot}/{sub}")),
        _ if slot != "0" => s.push_str(&format!(":{slot}")),
        _ => {}
    }
    s.push_str(&format!("::{repo_name}"));
    s
}

/// Render the emerge-style `Total:` breakdown, e.g.
/// `Total: 26 packages (20 new, 1 upgrade, 5 reinstalls)`.
fn total_line(
    order: &[(PortagePackage, Version)],
    installed: &HashMap<Cpn, HashMap<Interned<DefaultInterner>, Version>>,
    sizes: &HashMap<Cpv, u64>,
) -> String {
    let (mut new, mut new_slot, mut up, mut down, mut re) = (0, 0, 0, 0, 0);
    for (pkg, ver) in order {
        match action_tag(pkg, ver, installed).0 {
            "N" => new += 1,
            "NS" => new_slot += 1,
            "U" => up += 1,
            "D" => down += 1,
            "R" => re += 1,
            _ => {}
        }
    }
    let plural = |n: usize, s: &str| format!("{n} {s}{}", if n == 1 { "" } else { "s" });
    // Order mirrors portage's PackageCounters.__str__: upgrades, downgrades,
    // new, in new slot, reinstall.
    let mut parts = Vec::new();
    if up > 0 {
        parts.push(plural(up, "upgrade"));
    }
    if down > 0 {
        parts.push(plural(down, "downgrade"));
    }
    if new > 0 {
        parts.push(format!("{new} new"));
    }
    if new_slot > 0 {
        parts.push(plural(new_slot, "in new slot"));
    }
    if re > 0 {
        parts.push(plural(re, "reinstall"));
    }

    let n = order.len();
    let pkgs = if n == 1 { "package" } else { "packages" };
    let total_bytes: u64 = order
        .iter()
        .map(|(pkg, ver)| {
            sizes
                .get(&Cpv::new(*pkg.cpn(), ver.clone()))
                .copied()
                .unwrap_or(0)
        })
        .sum();
    let downloads = format!(", Size of downloads: {}", format_kib(total_bytes));
    if parts.is_empty() {
        format!("\nTotal: {n} {pkgs}{downloads}")
    } else {
        format!("\nTotal: {n} {pkgs} ({}){downloads}", parts.join(", "))
    }
}

/// Build the 7-char status field that follows `[ebuild ` in the merge list,
/// placing each action letter at the fixed column portage uses so columns line
/// up across rows: `N`/`NS` (new / new slot), `R` (reinstall), `U`/`D`
/// (upgrade / downgrade).
fn status_field(tag: &str, forced_rebuild: bool) -> String {
    let mut f = [b' '; 7];
    // Portage's lowercase `r` (forced rebuild, e.g. a slot-operator `:=`
    // rebind) shares the `N` column.
    if forced_rebuild {
        f[1] = b'r';
    }
    match tag {
        "N" => f[1] = b'N',
        "NS" => {
            f[1] = b'N';
            f[2] = b'S';
        }
        "R" => f[2] = b'R',
        "U" => f[4] = b'U',
        "D" => f[5] = b'D',
        "UD" => {
            f[4] = b'U';
            f[5] = b'D';
        }
        _ => {}
    }
    // SAFETY: f is an array of ASCII bytes [b' '; 7], so it's valid UTF-8
    String::from_utf8(f.to_vec()).expect("array of ASCII spaces is valid UTF-8")
}

/// Colorize the status field characters according to portage conventions:
/// - N: green
/// - S: green  
/// - U: cyan
/// - D: blue
/// - R: yellow
fn colorize_status_field(field: &str) -> String {
    let mut result = String::new();
    for (i, c) in field.chars().enumerate() {
        let style = match (i, c) {
            (1, 'N') => C_STATUS_N,
            (1, 'r') => C_ON,
            (2, 'S') => C_STATUS_S,
            (2, 'R') => C_STATUS_R,
            (4, 'U') => C_STATUS_U,
            (5, 'D') => C_STATUS_D,
            _ => Style::new(), // No color for spaces or other positions
        };
        result.push_str(&format!("{style}{c}{style:#}"));
    }
    result
}

/// Format a byte count as emerge does: ceil-divided to KiB (e.g. `569527` →
/// `557 KiB`, `0` → `0 KiB`). emerge's thousands grouping is locale-dependent
/// and absent under the C locale, so none is applied here.
fn format_kib(bytes: u64) -> String {
    format!("{} KiB", bytes.div_ceil(1024))
}

/// Shared render inputs for the pretty plan printers: the resolved package data
/// plus all the USE / size / flag-requirement context. Bundled so the rooted and
/// per-entry printers don't thread a dozen positional args each.
pub(super) struct PrettyCtx<'a> {
    pub data: &'a RepoData,
    pub installed: &'a HashMap<Cpn, HashMap<Interned<DefaultInterner>, Version>>,
    pub installed_entries: &'a [super::installed::VdbEntry],
    pub pre_env: &'a portage_atom_pubgrub::UseLayer,
    pub env_use: &'a portage_atom_pubgrub::UseLayer,
    pub package_use: &'a [(Dep, Vec<UseOverride>)],
    pub use_expand: &'a [String],
    pub use_expand_hidden: &'a [String],
    pub flag_reqs: &'a HashMap<&'a PortagePackage, &'a UseFlagRequirement>,
    pub sizes: &'a HashMap<Cpv, u64>,
    pub slot_op_cpns: &'a std::collections::HashSet<Cpn>,
    pub verbose: u8,
    pub ceded: &'a [CededFlag],
    /// Profile force/mask applied post-fold (not via package.use).
    pub force_mask: &'a portage_resolve::force_mask::ForceMask,
    /// For deciding whether `*.stable.*` force/mask applies to a version.
    pub accept_keywords: &'a portage_resolve::repo::AcceptKeywords,
    /// Local binpkg index, when `-k`/`-K` (usepkg/usepkgonly) is active —
    /// used to show `[binary ...]` instead of `[ebuild ...]` for an entry
    /// whose USE matches an available binpkg, matching real emerge's `-p`.
    /// `None` when neither flag is set, or for callers with no binpkg-reuse
    /// concept at all (`equery depgraph`, the crossdev gcc probe). Remote
    /// binhosts (`-g`/`-G`) are deliberately not checked here — that would
    /// add a network fetch to a plain `-p` preview; the merge itself still
    /// checks them via `run_merge_plan`'s own index.
    pub binpkg_index: Option<&'a portage_binpkg::BinpkgIndex>,
}

/// Print the emerge-style pretty plan, honouring each entry's
/// [`MergeRoot`](portage_atom_pubgrub::MergeRoot) (cross/host split).
pub(super) fn print_pretty_rooted(
    ctx: &PrettyCtx,
    plan: &[super::root_aware::PlanEntry],
    cross: &super::root_aware::CrossContext,
) {
    if cross.active && cross.is_cross_arch() {
        let mut err = anstream::stderr();
        let chost = cross.chost.as_deref().unwrap_or("?");
        let cbuild = cross.cbuild.as_deref().unwrap_or("?");
        writeln!(
            err,
            "Root-aware cross plan: CHOST={chost} CBUILD={cbuild} sysroot={} target={}",
            cross.sysroot, cross.target
        )
        .ok();
    }
    let order: Vec<_> = plan
        .iter()
        .map(|e| (e.pkg.clone(), e.version.clone()))
        .collect();
    let merge_roots: Vec<_> = plan.iter().map(|e| e.merge_root).collect();
    print_pretty_with_roots(ctx, &order, &merge_roots, cross);
}

fn print_pretty_with_roots(
    ctx: &PrettyCtx,
    order: &[(PortagePackage, Version)],
    merge_roots: &[portage_atom_pubgrub::MergeRoot],
    cross: &super::root_aware::CrossContext,
) {
    let &PrettyCtx {
        data,
        installed,
        installed_entries,
        pre_env,
        env_use,
        package_use,
        use_expand,
        use_expand_hidden,
        flag_reqs,
        sizes,
        slot_op_cpns,
        verbose,
        ceded,
        force_mask,
        accept_keywords,
        binpkg_index,
    } = ctx;
    let mut out = anstream::stdout();

    writeln!(
        out,
        "{C_PKG}These are the packages that would be merged, in order:{C_PKG:#}\n"
    )
    .ok();
    writeln!(out, "Calculating dependencies... done!").ok();

    for ((pkg, ver), merge_root) in order.iter().zip(merge_roots) {
        let dest_suffix = match super::root_aware::display_root(*merge_root, &cross.target, cross) {
            r if r.as_str() == "/" => String::new(),
            r => format!(" to {}/", r),
        };
        let cpn = pkg.cpn();
        let (tag, old_ver) = action_tag(pkg, ver, installed);
        let req = flag_reqs.get(pkg).copied();
        let cache = find_cache(data, pkg, ver);

        // emerge -p always shows USE flags; -v additionally shows the
        // :slot/subslot::repo suffix and download size.
        let cpv = Cpv::new(*cpn, ver.clone());
        let defaults = cache
            .map(super::effective_use::iuse_defaults)
            .unwrap_or_default();
        let mut effective_use =
            resolve_effective_use(&defaults, pre_env, &cpv, pkg.slot(), package_use, env_use);
        if let Some(c) = cache {
            let stable = accept_keywords.is_stable(&c.metadata.keywords, &cpv, pkg.slot());
            let iuse = super::effective_use::iuse_set(c);
            let slot_key = pkg.slot();
            super::effective_use::apply_force_mask(
                &mut effective_use,
                force_mask,
                &cpv,
                slot_key.as_ref().map(|s| s.as_str()),
                stable,
                &iuse,
            );
        }
        super::effective_use::apply_ceded(&mut effective_use, *cpn, ceded);

        // Would `-k`/`-K` reuse a local binpkg for this exact (cpv, USE)?
        // Same `use_compatible` rule `run_merge_plan` uses to actually pick
        // one — see `PrettyCtx::binpkg_index`'s doc for why remote (`-g`/
        // `-G`) isn't checked here.
        let is_binary = binpkg_index.is_some_and(|idx| {
            let desired_use: Vec<String> = effective_use
                .enabled_flags()
                .iter()
                .map(|f| f.as_str().to_string())
                .collect();
            // Empty CHOST and build_env_key: preview skips both gates (same as "unknown");
            // the real merge path passes make.conf CHOST and build_env_key.
            idx.find_reusable(&cpv.to_string(), &desired_use, "", "")
                .is_some()
        });

        let previous = previous_entry(pkg, ver, installed_entries);

        let flag_str = cache
            .map(|c| {
                format_flags(
                    c,
                    &effective_use,
                    use_expand,
                    use_expand_hidden,
                    req,
                    previous,
                    verbose >= 1,
                )
            })
            .unwrap_or_default();

        let slot_repo = if verbose >= 1 {
            cache
                .map(|c| slot_repo_suffix(c, super::repo::repo_name_of(data, &cpv)))
                .unwrap_or_default()
        } else {
            String::new()
        };

        // emerge shows the previously-installed version only for upgrades and
        // downgrades, not for same-version reinstalls or new installs.
        let old = match tag {
            "U" | "D" => old_ver.map(|v| format!(" [{}]", v)).unwrap_or_default(),
            _ => String::new(),
        };
        // Verbose mode appends the download size (distfiles not in DISTDIR).
        let size_str = if verbose >= 1 {
            format!(" {}", format_kib(sizes.get(&cpv).copied().unwrap_or(0)))
        } else {
            String::new()
        };
        let field = status_field(tag, slot_op_cpns.contains(cpn));
        let colored_field = colorize_status_field(&field);
        let kind = if is_binary { "binary" } else { "ebuild" };
        writeln!(
            out,
            "[{C_BRACKET}{kind} {colored_field}{C_BRACKET:#}] {C_PKG}{cpn}-{ver}{slot_repo}{C_PKG:#}{old}{flag_str}{size_str}{dest_suffix}",
        ).ok();
    }

    // emerge only prints the Total line in verbose mode.
    if verbose >= 1 {
        writeln!(out, "{}", total_line(order, installed, sizes)).ok();
    }
}

fn class_str(c: DepClass) -> &'static str {
    match c {
        DepClass::Depend => "DEPEND",
        DepClass::Rdepend => "RDEPEND",
        DepClass::Bdepend => "BDEPEND",
        DepClass::Pdepend => "PDEPEND",
        DepClass::Idepend => "IDEPEND",
    }
}

pub(super) fn print_json(
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    edges: &[portage_atom_pubgrub::DepEdge],
    installed: &HashMap<Cpn, HashMap<Interned<DefaultInterner>, Version>>,
    flag_reqs: &HashMap<&PortagePackage, &UseFlagRequirement>,
) -> anyhow::Result<()> {
    let packages: Vec<serde_json::Value> = order
        .iter()
        .map(|(pkg, ver)| {
            let cpn = pkg.cpn();
            let (tag, old_ver) = action_tag(pkg, ver, installed);
            let status = match tag {
                "U" => "upgrade",
                "D" => "downgrade",
                "R" => "reinstall",
                "NS" => "new_slot",
                _ => "new",
            };
            let mut obj = serde_json::json!({
                "atom": format!("{cpn}-{ver}"),
                "cpn": cpn.to_string(),
                "version": ver.to_string(),
                "repo": data.repo_name,
                "status": status,
            });
            if let Some(old) = old_ver {
                obj["old_version"] = serde_json::Value::String(old.to_string());
            }
            if let Some(cache) = find_cache(data, pkg, ver) {
                let slot = &cache.metadata.slot;
                obj["slot"] = serde_json::Value::String(slot.slot.as_str().to_owned());
                if let Some(sub) = slot.subslot {
                    obj["subslot"] = serde_json::Value::String(sub.as_str().to_owned());
                }
                let iuse: Vec<String> = {
                    let mut iuse_flags: Vec<_> = cache.metadata.iuse.iter().collect();
                    iuse_flags.sort_by_key(|f| f.name());
                    iuse_flags
                        .iter()
                        .map(|f| match f.default {
                            Some(portage_metadata::IUseDefault::Enabled) => {
                                format!("+{}", f.name())
                            }
                            _ => format!("-{}", f.name()),
                        })
                        .collect()
                };
                obj["iuse"] = serde_json::json!(iuse);
            }
            if let Some(req) = flag_reqs.get(pkg) {
                if !req.required_enabled.is_empty() {
                    let flags: Vec<&str> =
                        req.required_enabled.iter().map(|f| f.as_str()).collect();
                    obj["required_use_enabled"] = serde_json::json!(flags);
                }
                if !req.required_disabled.is_empty() {
                    let flags: Vec<&str> =
                        req.required_disabled.iter().map(|f| f.as_str()).collect();
                    obj["required_use_disabled"] = serde_json::json!(flags);
                }
            }
            obj
        })
        .collect();

    let dep_edges: Vec<serde_json::Value> = edges
        .iter()
        .map(|e| {
            serde_json::json!({
                "from": format!("{}-{}", e.from.0.cpn(), e.from.1),
                "to": format!("{}-{}", e.to.0.cpn(), e.to.1),
                "class": class_str(e.class),
            })
        })
        .collect();

    let out = serde_json::json!({
        "packages": packages,
        "edges": dep_edges,
    });

    let json = serde_json::to_string_pretty(&out)
        .map_err(|e| anyhow::anyhow!("failed to serialize JSON output: {e}"))?;
    println!("{json}");
    Ok(())
}

pub(super) const C_DIM: Style = Style::new().effects(Effects::DIMMED);

pub(super) fn print_tree(
    roots: &[(PortagePackage, Version)],
    edges: &[portage_atom_pubgrub::DepEdge],
    installed_cpvs: &std::collections::HashSet<Cpv>,
) {
    // version map: package -> version (from edges, covers all non-virtual nodes)
    let mut version_map: HashMap<&PortagePackage, &Version> = HashMap::new();
    for e in edges {
        version_map.insert(&e.from.0, &e.from.1);
        version_map.insert(&e.to.0, &e.to.1);
    }
    // also insert roots in case they have no outgoing edges
    for (pkg, ver) in roots {
        version_map.entry(pkg).or_insert(ver);
    }

    // children map: package -> ordered list of (package, version) deps
    let mut children: HashMap<&PortagePackage, Vec<(&PortagePackage, &Version)>> = HashMap::new();
    for e in edges {
        let ver = version_map.get(&e.to.0).copied().unwrap_or(&e.to.1);
        children.entry(&e.from.0).or_default().push((&e.to.0, ver));
    }
    // deduplicate children (same package may appear via multiple dep classes,
    // and not necessarily adjacently — DEPEND/BDEPEND edges to the same package
    // can be interleaved with others, so a positional dedup is insufficient).
    for kids in children.values_mut() {
        let mut seen: std::collections::HashSet<&PortagePackage> = Default::default();
        kids.retain(|(pkg, _)| seen.insert(*pkg));
    }

    let mut tree = Tree {
        out: anstream::stdout(),
        children,
        installed_cpvs,
        visited: Default::default(),
    };
    for (i, (pkg, ver)) in roots.iter().enumerate() {
        let is_last = i == roots.len() - 1;
        tree.node(pkg, ver, "", is_last, true);
    }
}

/// Shared state of one `print_tree` walk; `node` renders one package and
/// recurses into its children.
struct Tree<'a, W: std::io::Write> {
    out: W,
    children: HashMap<&'a PortagePackage, Vec<(&'a PortagePackage, &'a Version)>>,
    installed_cpvs: &'a std::collections::HashSet<Cpv>,
    visited: std::collections::HashSet<*const PortagePackage>,
}

impl<W: std::io::Write> Tree<'_, W> {
    fn node(
        &mut self,
        pkg: &PortagePackage,
        ver: &Version,
        prefix: &str,
        is_last: bool,
        is_root: bool,
    ) {
        let already = !self.visited.insert(pkg as *const _);
        let cpn = pkg.cpn();
        let is_installed = self.installed_cpvs.contains(&Cpv::new(*cpn, ver.clone()));

        let connector = if is_root {
            ""
        } else if is_last {
            "└── "
        } else {
            "├── "
        };

        if is_installed {
            writeln!(
                self.out,
                "{prefix}{connector}{C_DIM}{cpn}-{ver}{C_DIM:#}{}",
                if already { " (*)" } else { "" }
            )
            .ok();
        } else {
            writeln!(
                self.out,
                "{prefix}{connector}{C_PKG}{cpn}-{ver}{C_PKG:#}{}",
                if already {
                    format!(" {C_DIM}(*){C_DIM:#}")
                } else {
                    String::new()
                }
            )
            .ok();
        }

        if already {
            return;
        }

        let kids: Vec<(&PortagePackage, &Version)> = self
            .children
            .get(pkg)
            .map(|v| v.to_vec())
            .unwrap_or_default();
        let child_prefix = if is_root {
            prefix.to_string()
        } else if is_last {
            format!("{prefix}    ")
        } else {
            format!("{prefix}│   ")
        };

        for (i, (child, child_ver)) in kids.iter().enumerate() {
            let child_is_last = i == kids.len() - 1;
            self.node(child, child_ver, &child_prefix, child_is_last, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip ANSI so the marker matrix is asserted on text, not colour codes.
    fn plain(
        name: &str,
        enabled: bool,
        in_cur: bool,
        in_old_iuse: bool,
        in_old_use: bool,
        is_new: bool,
        show_unchanged: bool,
    ) -> Option<String> {
        flag_token(
            name,
            enabled,
            in_cur,
            in_old_iuse,
            in_old_use,
            is_new,
            show_unchanged,
        )
        .map(|(tok, _)| {
            let mut out = String::new();
            let mut chars = tok.chars();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    for c in chars.by_ref() {
                        if c == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        })
    }

    // The whole point of the markers: a flag that flips relative to the
    // installed build is the one thing a plain `em -p` must not bury.
    #[test]
    fn a_flipped_flag_is_starred_even_without_verbose() {
        // off -> on, and on -> off
        assert_eq!(
            plain("doc", true, true, true, false, false, false).as_deref(),
            Some("doc*")
        );
        assert_eq!(
            plain("doc", false, true, true, true, false, false).as_deref(),
            Some("-doc*")
        );
    }

    #[test]
    fn an_unchanged_flag_is_hidden_until_verbose() {
        assert_eq!(plain("doc", true, true, true, true, false, false), None);
        assert_eq!(plain("doc", false, true, true, false, false, false), None);
        assert_eq!(
            plain("doc", true, true, true, true, false, true).as_deref(),
            Some("doc")
        );
        assert_eq!(
            plain("doc", false, true, true, false, false, true).as_deref(),
            Some("-doc")
        );
    }

    // A flag the old build never declared is `%` — distinct from one that
    // merely flipped, since there was no previous choice to flip.
    #[test]
    fn a_flag_absent_from_the_old_iuse_is_percent_marked() {
        assert_eq!(
            plain("lto", true, true, false, false, false, false).as_deref(),
            Some("lto%*")
        );
        assert_eq!(
            plain("lto", false, true, false, false, false, false).as_deref(),
            Some("-lto%")
        );
    }

    #[test]
    fn a_flag_dropped_from_iuse_shows_parenthesised_and_only_when_verbose() {
        assert_eq!(plain("gone", false, false, true, false, false, false), None);
        assert_eq!(
            plain("gone", false, false, true, false, false, true).as_deref(),
            Some("(-gone%)")
        );
        // ...and keeps the star when it had been enabled.
        assert_eq!(
            plain("gone", false, false, true, true, false, true).as_deref(),
            Some("(-gone%*)")
        );
    }

    // Nothing installed to compare against: every flag shows, none marked.
    #[test]
    fn a_new_package_shows_every_flag_unmarked() {
        assert_eq!(
            plain("doc", true, true, false, false, true, false).as_deref(),
            Some("doc")
        );
        assert_eq!(
            plain("doc", false, true, false, false, true, false).as_deref(),
            Some("-doc")
        );
    }

    fn ceded(cpn: &str, flag: &str) -> CededFlag {
        CededFlag {
            cpn: Cpn::parse(cpn).unwrap(),
            flag: Interned::intern(flag),
            value: true,
            flipped: false,
        }
    }

    fn slotted(cpn: &str, slot: &str) -> PortagePackage {
        PortagePackage::slotted(Cpn::parse(cpn).unwrap(), Interned::intern(slot))
    }

    #[test]
    fn shared_slot_decision_detected_for_multi_slot_cpn() {
        let ceded = vec![
            ceded("dev-qt/qtbase", "syslog"),
            ceded("dev-qt/qtbase", "journald"),
        ];
        let v = Version::parse("1.0").unwrap();
        let a = slotted("dev-qt/qtbase", "5");
        let b = slotted("dev-qt/qtbase", "6");
        let solution = [(&a, &v), (&b, &v)];
        let got = shared_slot_decisions(&ceded, solution);
        assert_eq!(got.len(), 1);
        let (cpn, slots, flags) = &got[0];
        assert_eq!(cpn.to_string(), "dev-qt/qtbase");
        assert_eq!(slots, &["5", "6"]);
        assert_eq!(flags, &["journald", "syslog"], "flags sorted");
    }

    #[test]
    fn single_slot_cpn_not_reported() {
        let ceded = vec![ceded("dev-qt/qtbase", "syslog")];
        let v = Version::parse("1.0").unwrap();
        let a = slotted("dev-qt/qtbase", "6");
        let other = slotted("dev-libs/foo", "0");
        let solution = [(&a, &v), (&other, &v)];
        assert!(shared_slot_decisions(&ceded, solution).is_empty());
    }

    #[test]
    fn multi_slot_cpn_without_decisions_not_reported() {
        let ceded = vec![ceded("dev-libs/foo", "bar")];
        let v = Version::parse("1.0").unwrap();
        let a = slotted("dev-qt/qtbase", "5");
        let b = slotted("dev-qt/qtbase", "6");
        let foo = slotted("dev-libs/foo", "0");
        let solution = [(&a, &v), (&b, &v), (&foo, &v)];
        assert!(shared_slot_decisions(&ceded, solution).is_empty());
    }
}
