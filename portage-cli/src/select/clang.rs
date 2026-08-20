//! `em select clang` — LLVM/clang slot selection
//!
//! Manages which LLVM/clang version (slot) is active. Unlike gcc which uses
//! env.d/gcc/ profiles, clang is installed under /usr/lib/llvm/${SLOT}/ and
//! uses symlinks managed by the clang-toolchain-symlinks package.

use std::io::Write as _;

use anyhow::{Context, Result, bail};
use camino::Utf8PathBuf;

use super::{config_portage_dir, is_prefix_context, source_label};
use crate::cli::{ClangAction, Cli};
use crate::style::C_STAR;
use portage_atom::Version;

/// Base directory for LLVM installations
fn llvm_base_dir(globals: &Cli) -> Utf8PathBuf {
    // Check if we're in a prefix/local context
    if is_prefix_context(globals) {
        // For prefix, LLVM would be under EPREFIX/usr/lib/llvm
        // outer_roots(), not roots(): ClangAction has no --target of its
        // own, but select never wants roots()'s sysroot substitution
        // regardless (see env_d::run_list's doc comment).
        let roots = globals.outer_roots();
        if let Some(eprefix) = roots.eprefix() {
            return eprefix.join("usr/lib/llvm");
        }
    }
    // System location
    Utf8PathBuf::from("/usr/lib/llvm")
}

/// Path to the current clang slot config file
fn current_clang_slot_path(globals: &Cli) -> Utf8PathBuf {
    let config_dir = config_portage_dir(globals);
    config_dir.join("clang").join("current-slot")
}

/// Path to a slot's `gentoo-linker.cfg` (owned by `llvm-core/clang-linker-config`'s
/// `src_install`, keyed on its `default-lld` USE flag — not something `em
/// select` manages; this is read-only display, mirroring `llvm_base_dir`'s
/// own prefix-vs-system root resolution).
fn linker_cfg_path(globals: &Cli, slot: &str) -> Utf8PathBuf {
    if is_prefix_context(globals) {
        let roots = globals.outer_roots();
        if let Some(eprefix) = roots.eprefix() {
            return eprefix
                .join("etc/clang")
                .join(slot)
                .join("gentoo-linker.cfg");
        }
    }
    Utf8PathBuf::from(format!("/etc/clang/{slot}/gentoo-linker.cfg"))
}

/// The `-fuse-ld=` value from a slot's `gentoo-linker.cfg`, if present
fn linker_default(globals: &Cli, slot: &str) -> Option<String> {
    let content = std::fs::read_to_string(linker_cfg_path(globals, slot)).ok()?;
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .find_map(|line| line.strip_prefix("-fuse-ld=").map(str::to_string))
}

/// Which installation a slot was found in
///
/// Only `em` has to care: real `eselect` sees one root, while an overlay
/// prefix can hold the *same* slot as the host, so a selection is ambiguous
/// without this attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Prefix,
    Host,
}

impl Origin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prefix => "prefix",
            Self::Host => "host",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "prefix" => Some(Self::Prefix),
            "host" => Some(Self::Host),
            _ => None,
        }
    }
}

/// An LLVM/clang slot
#[derive(Debug, Clone)]
struct ClangSlot {
    name: String,
    /// List of architectures that have {arch}-clang symlinks
    targets: Vec<String>,
    /// Whether this slot is from the host system or the current config root
    is_host: bool,
    /// The slot directory itself (`<root>/usr/lib/llvm/<name>`), so a selection
    /// can point PATH at it without re-deriving which root it came from.
    dir: Utf8PathBuf,
}

impl ClangSlot {
    fn origin(&self) -> Origin {
        if self.is_host {
            Origin::Host
        } else {
            Origin::Prefix
        }
    }

    /// The unambiguous name for this slot, e.g. `22@prefix`
    fn qualified(&self) -> String {
        format!("{}@{}", self.name, self.origin().as_str())
    }
}

/// List all available LLVM/clang slots, grouped by... (no target grouping for clang)
fn list_all_clang_slots(globals: &Cli) -> Result<Vec<ClangSlot>> {
    let mut slots: Vec<ClangSlot> = Vec::new();

    // Check if we're in a prefix/local context
    let is_prefix_context = is_prefix_context(globals);

    // Collect slots from the current config root (prefix/local)
    let prefix_llvm_dir = llvm_base_dir(globals);
    if prefix_llvm_dir.is_dir() {
        collect_clang_slots(&prefix_llvm_dir, &mut slots, false)?;
    }

    // If in prefix context, also check system location
    if is_prefix_context {
        let system_dir = Utf8PathBuf::from("/usr/lib/llvm");
        if system_dir.is_dir() {
            collect_clang_slots(&system_dir, &mut slots, true)?;
        }
    }

    // Sort by slot name as a real Gentoo version, not lexicographically --
    // plain `str::cmp` would put "17.0" ahead of "9" (byte-wise, '1' < '9').
    slots.sort_by(
        |a, b| match (Version::parse(&a.name), Version::parse(&b.name)) {
            (Ok(va), Ok(vb)) => va.cmp(&vb),
            _ => a.name.cmp(&b.name),
        },
    );

    Ok(slots)
}

/// Helper to collect clang slots from a directory
fn collect_clang_slots(
    llvm_dir: &Utf8PathBuf,
    slots: &mut Vec<ClangSlot>,
    is_host: bool,
) -> Result<()> {
    if !llvm_dir.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(llvm_dir)? {
        let entry = entry?;
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        let name = path.file_name().unwrap_or_default().to_string();

        // Skip non-directory entries
        if !path.is_dir() {
            continue;
        }

        // LLVM slots are numeric (e.g., "15", "16", "17", "22") or major.minor (e.g., "17.0")
        // We use a simple heuristic: if it starts with a digit, it's likely a slot
        if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            // Find available targets for this slot
            let targets = find_slot_targets(&path);
            slots.push(ClangSlot {
                name,
                targets,
                is_host,
                dir: path.clone(),
            });
        }
    }

    Ok(())
}

/// Find which targets have {arch}-clang symlinks in a slot's bin directory
fn find_slot_targets(slot_dir: &Utf8PathBuf) -> Vec<String> {
    let bin_dir = slot_dir.join("bin");
    let mut targets: Vec<String> = Vec::new();

    if !bin_dir.is_dir() {
        return targets;
    }

    // List of clang binaries that might have target prefixes
    let clang_binaries = ["clang", "clang++", "clang-cpp"];

    if let Ok(entries) = std::fs::read_dir(&bin_dir) {
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
                continue;
            };
            let name = path.file_name().unwrap_or_default().to_string();

            // Check if this file name starts with a target prefix followed by a clang binary
            for binary in &clang_binaries {
                if let Some(prefix) = name.strip_suffix(binary) {
                    // Strip trailing dash if present (e.g., "aarch64-unknown-linux-gnu-" -> "aarch64-unknown-linux-gnu")
                    let target = prefix.strip_suffix('-').unwrap_or(prefix);
                    // Check it's not just the binary itself (which would give empty prefix)
                    // and not just a dash
                    if !target.is_empty() && target != "-" {
                        targets.push(target.to_string());
                        break; // Only add once per file (clang, clang++, clang-cpp all have same prefix)
                    }
                }
            }
        }
    }

    // Deduplicate and sort
    targets.sort();
    targets.dedup();
    targets
}

/// Get the current clang slot
fn get_current_clang_slot(globals: &Cli) -> Option<String> {
    let config_path = current_clang_slot_path(globals);
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        for line in content.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                return Some(line.to_string());
            }
        }
    }
    None
}

/// Resolve a `set` argument against the available slots
///
/// Accepted, in this order: an exact slot name (`22`), a slot qualified by
/// origin (`22@host`), or a 1-based number from `list`. Name is tried before
/// index deliberately — LLVM slots *are* numbers, `22` has always meant the
/// slot, and a list position that happens to collide must not silently steal
/// it.
///
/// A bare slot found in both roots resolves to the **prefix's**, matching the
/// shadowing the planner already applies to `VDB(R) ∪ VDB(P)`
/// (`docs/user/root-model.md`). Pass `@host` to override.
fn resolve_slot<'a>(slots: &'a [ClangSlot], arg: &str) -> Result<&'a ClangSlot> {
    let (name, origin) = match arg.split_once('@') {
        Some((name, origin)) => {
            let origin = Origin::parse(origin)
                .with_context(|| format!("unknown origin '{origin}' (expected host or prefix)"))?;
            (name, Some(origin))
        }
        None => (arg, None),
    };

    let mut matched = slots
        .iter()
        .filter(|s| s.name == name && origin.is_none_or(|o| s.origin() == o));
    if let Some(first) = matched.next() {
        // Prefix shadows host when the caller did not say which.
        return Ok(matched
            .find(|s| s.origin() == Origin::Prefix)
            .unwrap_or(first));
    }

    if origin.is_none()
        && let Ok(index) = arg.parse::<usize>()
        && let Some(slot) = index.checked_sub(1).and_then(|i| slots.get(i))
    {
        return Ok(slot);
    }

    bail!(
        "LLVM slot '{arg}' not found (available: {})",
        slots
            .iter()
            .map(ClangSlot::qualified)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Record the selection and make it take effect
fn set_clang_slot(globals: &Cli, arg: &str) -> Result<String> {
    let slots = list_all_clang_slots(globals)?;
    let slot = resolve_slot(&slots, arg)?;
    let selection = slot.qualified();

    let config_path = current_clang_slot_path(globals);
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent))?;
    }
    std::fs::write(&config_path, format!("{selection}\n"))
        .with_context(|| format!("writing {}", config_path))?;

    write_llvm_env_d(globals, slot)?;
    Ok(selection)
}

/// Put the selected slot's `bin` ahead of every other on `PATH`, through the
/// mechanism LLVM already uses.
///
/// Each `llvm-core/llvm` slot ships its own `env.d/60llvm-NNNN`, where the
/// number encodes priority (`9977` = slot 22, `9978` = 21, …) so that the
/// newest installed slot wins by sort order alone — Gentoo has no selection
/// step here at all. Writing `59llvm-selected` sorts ahead of all of them, so
/// an explicit choice beats the newest-wins default without editing files that
/// belong to packages.
///
/// This is also the only way a choice reaches anything: `current-slot` on its
/// own is inert state that nothing outside this module reads.
fn write_llvm_env_d(globals: &Cli, slot: &ClangSlot) -> Result<()> {
    let Some(eprefix) = globals.outer_roots().eprefix().map(Utf8PathBuf::from) else {
        // No prefix: the host's own env.d belongs to the system, and rewriting
        // it from an unprivileged tool is not this command's business.
        return Ok(());
    };
    let bin = slot.dir.join("bin");
    let body = format!(
        "# Autogenerated by 'em select clang'.\n\
         # Sorts before the llvm-supplied 60llvm-* so this choice wins on PATH.\n\
         PATH=\"{bin}\"\n\
         ROOTPATH=\"{bin}\"\n\
         MANPATH=\"{man}\"\n\
         LDPATH=\"{lib}\"\n",
        bin = bin,
        man = slot.dir.join("share/man"),
        lib = slot.dir.join("lib64"),
    );
    let path = eprefix.join("etc/env.d/59llvm-selected");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
    }
    crate::util::write_atomic(&path, &body)
}

pub fn run(action: &ClangAction, globals: &Cli) -> Result<()> {
    match action {
        ClangAction::List => list(globals),
        ClangAction::Show => {
            show(globals);
            Ok(())
        }
        ClangAction::Set { slot } => set(globals, slot),
    }
}

fn list(globals: &Cli) -> Result<()> {
    let slots = list_all_clang_slots(globals)?;
    let mut out = anstream::stdout();

    if slots.is_empty() {
        println!("No LLVM/clang slots found");
        return Ok(());
    }

    let total_count = slots.len();
    let num_width = total_count.to_string().len();

    let current = get_current_clang_slot(globals);

    for (idx, slot) in slots.iter().enumerate() {
        let n = idx + 1;
        // A selection written before origins existed is a bare slot name;
        // still mark it, rather than showing nothing as current.
        let is_current = current
            .as_deref()
            .is_some_and(|c| c == slot.qualified() || c == slot.name);
        let num = format!("[{:>width$}]", n, width = num_width);

        // Format: clang-{version} [arch1, arch2, ...] [* if current]
        let mut slot_display = format!("clang-{}", slot.name);

        if !slot.targets.is_empty() {
            slot_display.push_str(" [");
            for (i, target) in slot.targets.iter().enumerate() {
                if i > 0 {
                    slot_display.push_str(", ");
                }
                slot_display.push_str(target);
            }
            slot_display.push(']');
        }

        if let Some(linker) = linker_default(globals, &slot.name) {
            slot_display.push_str(&format!(" (default linker: {linker})"));
        }

        if is_current {
            slot_display = format!("{}{C_STAR} *{C_STAR:#}", slot_display);
        }

        // Add source label if in prefix context
        if is_prefix_context(globals) {
            let label = source_label(slot.is_host);
            slot_display.push_str(&label);
        }

        writeln!(out, "  {num} {}", slot_display).ok();
    }

    Ok(())
}

fn show(globals: &Cli) {
    match get_current_clang_slot(globals) {
        Some(slot) => println!("{}", slot),
        None => println!("(no LLVM/clang slot set)"),
    }
}

fn set(globals: &Cli, slot: &str) -> Result<()> {
    let selection = set_clang_slot(globals, slot)?;
    println!(">>> LLVM/clang slot set: {selection}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(name: &str, is_host: bool) -> ClangSlot {
        ClangSlot {
            name: name.to_string(),
            targets: Vec::new(),
            is_host,
            dir: Utf8PathBuf::from(if is_host {
                format!("/usr/lib/llvm/{name}")
            } else {
                format!("/p/usr/lib/llvm/{name}")
            }),
        }
    }

    // The overlay case real eselect never faces: the same slot in both roots
    #[test]
    fn a_bare_slot_in_both_roots_resolves_to_the_prefix() {
        let slots = [slot("20", true), slot("22", false), slot("22", true)];
        assert_eq!(resolve_slot(&slots, "22").unwrap().qualified(), "22@prefix");
        assert_eq!(
            resolve_slot(&slots, "22@host").unwrap().qualified(),
            "22@host"
        );
        assert_eq!(
            resolve_slot(&slots, "22@prefix").unwrap().qualified(),
            "22@prefix"
        );
        // Only one copy of 20 exists, and it is the host's.
        assert_eq!(resolve_slot(&slots, "20").unwrap().qualified(), "20@host");
    }

    // Slot names are numbers, so a name must beat a list position — `22` has always meant the
    // slot
    //
    // Indices still work where they do not collide, which is what `list` advertises.
    #[test]
    fn a_slot_name_wins_over_a_list_index() {
        let slots = [slot("1", true), slot("22", true)];
        // "1" is both a slot name and the first list position; the name wins.
        assert_eq!(resolve_slot(&slots, "1").unwrap().qualified(), "1@host");
        // "2" is not a slot name here, so it is the second entry.
        assert_eq!(resolve_slot(&slots, "2").unwrap().qualified(), "22@host");
    }

    #[test]
    fn unknown_selections_say_what_is_available() {
        let slots = [slot("22", false)];
        let err = resolve_slot(&slots, "19").unwrap_err().to_string();
        assert!(err.contains("22@prefix"), "{err}");
        // Out-of-range index, and a bad origin.
        assert!(resolve_slot(&slots, "7").is_err());
        assert!(resolve_slot(&slots, "22@elsewhere").is_err());
        assert!(resolve_slot(&[], "22").is_err());
    }
}
