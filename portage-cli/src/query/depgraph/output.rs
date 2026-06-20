use std::collections::HashMap;
use std::io::Write as _;

use anstyle::{AnsiColor, Effects, Style};
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    CededFlag, DepClass, DroppedDep, PortagePackage, UseConfig, UseFlagRequirement, UseFlagState,
    UseOverride, apply_package_use,
};
use portage_metadata::CacheEntry;

pub(super) use crate::cli::C_PKG;

// emerge color scheme: bold green for keywords/atoms/tags, bold red/blue for flags
// Package names use plain green (not bold) to match portage's PKG_MERGE style
// (C_PKG now centralized in cli.rs to prevent duplication/stomping between agents).
pub(super) const C_ON: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)))
    .effects(Effects::BOLD);
pub(super) const C_OFF: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Blue)))
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
use super::repo::{RepoData, find_cache};

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
pub(super) fn report_solver_violations(violations: &[portage_solver::Violation]) {
    use portage_solver::Violation;
    let mut out = anstream::stderr();

    let blockers: Vec<&Violation> = violations
        .iter()
        .filter(|e| matches!(e, Violation::Blocker { .. }))
        .collect();
    if !blockers.is_empty() {
        writeln!(out, "\n{C_OFF}!!!{C_OFF:#} Blocker conflict(s) detected:\n").ok();
        for e in blockers {
            if let Violation::Blocker {
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

    let repos: Vec<&Violation> = violations
        .iter()
        .filter(|e| matches!(e, Violation::Repo(..)))
        .collect();
    if !repos.is_empty() {
        writeln!(
            out,
            "\n{C_OFF}!!!{C_OFF:#} Repository constraint conflict(s) detected:\n"
        )
        .ok();
        for e in repos {
            if let Violation::Repo(pkg, msg) = e {
                writeln!(out, "  {C_PKG}{pkg}{C_PKG:#}: {msg}").ok();
            }
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

/// Format USE flags for display.
///
/// For upgrades/downgrades, if `installed_active_use` is Some, only show flags that differ
/// from the installed version's active USE (emerge -p behavior).
fn format_flags(
    cache: &CacheEntry,
    use_config: &UseConfig,
    use_expand: &[String],
    use_expand_hidden: &[String],
    is_reinstall: bool,
    req: Option<&UseFlagRequirement>,
    installed_active_use: Option<&[Interned<DefaultInterner>]>,
) -> String {
    // Each entry: (enabled_tokens, disabled_tokens).  BTreeMap keeps groups sorted.
    let mut base_flags: (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut expand_groups: std::collections::BTreeMap<&str, (Vec<String>, Vec<String>)> =
        std::collections::BTreeMap::new();

    let mut flags: Vec<_> = cache.metadata.iuse.iter().collect();
    flags.sort_by_key(|f| f.name());
    flags.dedup_by_key(|f| f.name());

    for f in flags {
        let name = f.name();
        let iuse_default_enabled =
            matches!(f.default, Some(portage_metadata::IUseDefault::Enabled));
        let interned = Interned::intern(name);
        let mut enabled = match use_config.get_opt(interned) {
            Some(UseFlagState::Enabled) => true,
            Some(_) => false,
            None => iuse_default_enabled,
        };

        // In diff mode (upgrade/downgrade), skip flags that haven't changed
        if let Some(installed_use) = installed_active_use {
            let installed_enabled = installed_use.contains(&interned);
            // Skip if the enabled state is the same
            if enabled == installed_enabled {
                continue;
            }
        }

        // For reinstall packages: show current state with a change marker.
        // For new/upgrade packages: apply required state directly (it will be
        // enforced at build time, so show the intended post-install state).
        let suffix = if is_reinstall {
            if let Some(r) = req {
                if r.required_enabled.contains(&interned) {
                    "*"
                } else if r.required_disabled.contains(&interned) {
                    "%"
                } else {
                    ""
                }
            } else {
                ""
            }
        } else {
            if let Some(r) = req {
                if r.required_enabled.contains(&interned) {
                    enabled = true;
                } else if r.required_disabled.contains(&interned) {
                    enabled = false;
                }
            }
            ""
        };
        let expand_match = use_expand.iter().find(|key| {
            let prefix = format!("{}_", key.to_lowercase());
            name.starts_with(prefix.as_str())
        });

        if let Some(key) = expand_match {
            let prefix = format!("{}_", key.to_lowercase());
            let short = &name[prefix.len()..];
            let bucket = expand_groups.entry(key.as_str()).or_default();
            if enabled {
                bucket.0.push(format!("{C_ON}{short}{suffix}{C_ON:#}"));
            } else {
                // Wrap disabled USE_EXPAND flags in parentheses
                bucket.1.push(format!("{C_OFF}(-{short}{suffix}){C_OFF:#}"));
            }
        } else if enabled {
            base_flags.0.push(format!("{C_ON}{name}{suffix}{C_ON:#}"));
        } else {
            base_flags
                .1
                .push(format!("{C_OFF}-{name}{suffix}{C_OFF:#}"));
        }
    }

    let join_bucket = |(on, off): &(Vec<String>, Vec<String>)| -> String {
        // Sort enabled and disabled flags separately for portage-compatible ordering
        let mut on_sorted = on.clone();
        let mut off_sorted = off.clone();
        on_sorted.sort();
        off_sorted.sort();
        on_sorted
            .into_iter()
            .chain(off_sorted)
            .collect::<Vec<_>>()
            .join(" ")
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
    installed: &HashMap<Cpn, HashMap<String, Version>>,
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
    String::from_utf8(f.to_vec()).unwrap()
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

/// Like [`print_pretty`], but honours per-entry [`MergeRoot`](portage_atom_pubgrub::MergeRoot).
#[allow(clippy::too_many_arguments)]
pub(super) fn print_pretty_rooted(
    data: &RepoData,
    plan: &[super::root_aware::PlanEntry],
    installed: &HashMap<Cpn, HashMap<String, Version>>,
    installed_entries: &[super::installed::VdbEntry],
    use_config: &UseConfig,
    package_use: &[(Dep, Vec<UseOverride>)],
    use_expand: &[String],
    use_expand_hidden: &[String],
    flag_reqs: &HashMap<&PortagePackage, &UseFlagRequirement>,
    sizes: &HashMap<Cpv, u64>,
    slot_op_cpns: &std::collections::HashSet<Cpn>,
    verbose: u8,
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
    print_pretty_with_roots(
        data,
        &order,
        &merge_roots,
        installed,
        installed_entries,
        use_config,
        package_use,
        use_expand,
        use_expand_hidden,
        flag_reqs,
        sizes,
        slot_op_cpns,
        verbose,
        cross,
    );
}

#[allow(clippy::too_many_arguments, dead_code)]
pub(super) fn print_pretty(
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    installed: &HashMap<Cpn, HashMap<String, Version>>,
    installed_entries: &[super::installed::VdbEntry],
    use_config: &UseConfig,
    package_use: &[(Dep, Vec<UseOverride>)],
    use_expand: &[String],
    use_expand_hidden: &[String],
    flag_reqs: &HashMap<&PortagePackage, &UseFlagRequirement>,
    sizes: &HashMap<Cpv, u64>,
    slot_op_cpns: &std::collections::HashSet<Cpn>,
    verbose: u8,
    target_root: Option<&camino::Utf8Path>,
) {
    let merge_roots: Vec<_> = order
        .iter()
        .map(|_| portage_atom_pubgrub::MergeRoot::Target)
        .collect();
    let cross = super::root_aware::CrossContext {
        active: false,
        sysroot: camino::Utf8PathBuf::from("/"),
        target: target_root
            .map(|p| p.to_owned())
            .unwrap_or_else(|| camino::Utf8PathBuf::from("/")),
        chost: None,
        cbuild: None,
    };
    print_pretty_with_roots(
        data,
        order,
        &merge_roots,
        installed,
        installed_entries,
        use_config,
        package_use,
        use_expand,
        use_expand_hidden,
        flag_reqs,
        sizes,
        slot_op_cpns,
        verbose,
        &cross,
    );
}

#[allow(clippy::too_many_arguments)]
fn print_pretty_with_roots(
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    merge_roots: &[portage_atom_pubgrub::MergeRoot],
    installed: &HashMap<Cpn, HashMap<String, Version>>,
    installed_entries: &[super::installed::VdbEntry],
    use_config: &UseConfig,
    package_use: &[(Dep, Vec<UseOverride>)],
    use_expand: &[String],
    use_expand_hidden: &[String],
    flag_reqs: &HashMap<&PortagePackage, &UseFlagRequirement>,
    sizes: &HashMap<Cpv, u64>,
    slot_op_cpns: &std::collections::HashSet<Cpn>,
    verbose: u8,
    cross: &super::root_aware::CrossContext,
) {
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
        let is_reinstall = tag == "R";
        let cache = find_cache(data, pkg, ver);

        // emerge -p always shows USE flags; -v additionally shows the
        // :slot/subslot::repo suffix and download size.
        let cpv = Cpv::new(*cpn, ver.clone());
        let effective_use = apply_package_use(use_config, &cpv, pkg.slot(), package_use);

        // For upgrades/downgrades, find the installed entry to compare USE flags
        let installed_active_use = if tag == "U" || tag == "D" {
            let slot_key = pkg
                .slot()
                .map(|s| s.as_str().to_string())
                .unwrap_or_default();
            installed_entries
                .iter()
                .find(|e| e.cpn == *cpn && e.slot.as_deref() == Some(slot_key.as_str()))
                .map(|e| e.active_use.as_slice())
        } else {
            None
        };

        let flag_str = cache
            .map(|c| {
                format_flags(
                    c,
                    &effective_use,
                    use_expand,
                    use_expand_hidden,
                    is_reinstall,
                    req,
                    installed_active_use,
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
        writeln!(
            out,
            "[{C_BRACKET}ebuild {colored_field}{C_BRACKET:#}] {C_PKG}{cpn}-{ver}{slot_repo}{C_PKG:#}{old}{flag_str}{size_str}{dest_suffix}",
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
    installed: &HashMap<Cpn, HashMap<String, Version>>,
    flag_reqs: &HashMap<&PortagePackage, &UseFlagRequirement>,
) {
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

    println!("{}", serde_json::to_string_pretty(&out).unwrap());
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
