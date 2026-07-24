//! `em select clang` — LLVM/clang slot selection.
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

/// Base directory for LLVM installations.
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

/// Path to the current clang slot config file.
fn current_clang_slot_path(globals: &Cli) -> Utf8PathBuf {
    let config_dir = config_portage_dir(globals);
    config_dir.join("clang").join("current-slot")
}

/// An LLVM/clang slot.
#[derive(Debug, Clone)]
struct ClangSlot {
    name: String,
    /// List of architectures that have {arch}-clang symlinks
    targets: Vec<String>,
    /// Whether this slot is from the host system or the current config root
    is_host: bool,
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

/// Get the current clang slot.
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

/// Set the current clang slot.
fn set_clang_slot(globals: &Cli, slot: &str) -> Result<()> {
    let config_path = current_clang_slot_path(globals);

    // Verify the slot exists
    let llvm_dir = llvm_base_dir(globals);
    let slot_dir = llvm_dir.join(slot);
    if !slot_dir.is_dir() {
        // Also check system location
        let system_slot_dir = Utf8PathBuf::from(format!("/usr/lib/llvm/{}", slot));
        if !system_slot_dir.is_dir() {
            bail!("LLVM slot '{}' not found", slot);
        }
    }

    // Ensure the config directory exists
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent))?;
    }

    std::fs::write(&config_path, format!("{}\n", slot))
        .with_context(|| format!("writing {}", config_path))?;

    Ok(())
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
        let is_current = current.as_deref() == Some(&slot.name);
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
    set_clang_slot(globals, slot)?;
    println!(">>> LLVM/clang slot set: {}", slot);
    Ok(())
}
