# Mask consolidation / cleanup maint tool + override warning

Status: 🔴 not started. Agreed 2026-08-25 while landing widened-autounmask
persistence (Luca: "we can have a maint tool to consolidate and clean up
masks … and possibly detect and warn early").

## Background

Widened solves + `--autounmask-write` persist keyword/unmask entries into
`/etc/portage/package.{accept_keywords,unmask}` automatically (crossdev
setup implies the write). Nothing ever removes entries, and nothing tells
the user when an auto-unmask overrides a *deliberate* local mask.

Concrete instance found live: the `pilot-i586-em` sandbox's
`package.mask/llvm-unused-slot` masks `>=llvm-core/llvmgold-22` — written
by crossdev-stages as a **workaround** to keep `dev-lang/rust`'s own
`llvm_slot_21` pin from dragging in the llvm:22 chain. em's first widened
resolve wrote `=llvm-core/llvmgold-24:0` into `package.unmask`,
silently cancelling that workaround. Not wrong (the mask is a workaround,
not settled policy), but the override was invisible.

## What to build

1. **`em maint masks`** (name TBD) — consolidate and clean up:
   - list mask/unmask/accept_keywords entries em itself wrote vs hand edits
     (needs provenance, see below)
   - drop entries that no longer have an effect (version gone from tree,
     slot empty, superseded by ACCEPT_KEYWORDS changes)
   - merge near-duplicate lines (`=pkg-1 **` + `=pkg-2 **` → `pkg:SLOT **`)
2. **Override warning at write time**: when `autounmask::write` is about to
   write a package.unmask entry whose cpv matches an *existing*
   `package.mask` atom from a non-em-owned file, print a named warning
   ("this cancels your local mask `<atom>` from `<file>`"). Cheap,
   config-local, no provenance format needed.
3. **Provenance for em-written entries**: a marker comment block (or an
   em-owned subfile) so cleanup can distinguish generated grants from user
   policy. Decide format when building 1; the writer currently appends bare
   lines via `merge_content`.

## How to attack

1. Start with 2 (warning) — smallest, catches the llvmgold class today.
2. Prototype 1 against this host's accumulated `/etc/portage` state.
3. Keep `--autounmask-keep-masks` out of scope unless the warning proves
   insufficient — the maint tool supersedes it.
