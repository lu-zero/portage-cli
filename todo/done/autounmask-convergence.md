# `--autounmask-write` must converge in a single invocation

STATUS: **NOT A BUG — resolved 2026-06-19** (the `-p` artifact the caveat below
suspected). Verified in a clean crossdev-stages stage3 sandbox
([[crossdev-stages-sandbox]], profile `default/linux/arm64/23.0`) from an empty
autounmask state, target `www-client/firefox`:

- `em -p --autounmask` reports the **complete** USE-change set in ONE call
  (`freetype harfbuzz`, `libglvnd X`, `libvpx postproc`, `cairo X`) and a plan
  that fully reaches `firefox-140.11.0` — the cosolve fixpoint applies the
  changes in-memory and re-solves, so there is no per-run single layer.
- That set is **identical to emerge**'s (`emerge -p --autounmask --autounmask-use=y`).
- Applying the 4 changes and re-running → `rc=0`, zero "necessary to proceed"
  advisories: converged.
- `em -p --autounmask-write` does **not** write (`package.use` hash unchanged),
  exactly like `emerge -p` — pretend suppresses the write, so a `-p` loop never
  converges for *either* tool. That was the whole of the original observation.

No code change needed. (A non-`-p` write pass is a separate "does em merge after
writing" question, not a convergence bug.)

## Bug (2026-06-18)

`em --autounmask --autounmask-write` only discovers **one layer** of required
config changes per run. On a minimal profile (e.g. a fresh stage3) resolving
`firefox` needs several rounds: pass 1 writes `media-libs/freetype harfbuzz`,
`x11-libs/cairo X`, `media-video/ffmpeg` license, …; a *second* `em` invocation
then surfaces the next set unlocked by those, and so on. We had to loop
`em --autounmask-write` until `rc=0` to get a full plan — that external loop is a
workaround, not the intended behaviour.

emerge does this internally: one `emerge --autounmask-write --autounmask-backtrack=y`
call backtracks until the autounmask set is closed, writing all changes at once
(or reporting the full set in-memory for `-p`).

> Caveat (2026-06-18): the original observation may be partly a `-p` artifact —
> `emerge -p --autounmask-write` does **not** write (pretend suppresses the write),
> so a `-p` loop never converges. emerge *without* `-p` converged in a single
> write-pass on a fresh stage3. Whether `em` shares this `-p`-suppresses-write
> behaviour — i.e. whether em's looping was the same artifact rather than a real
> single-layer limit — is **not yet re-verified**; confirm before treating this as
> a confirmed em bug.

## Expected

A single `em --autounmask[-write]` should iterate its own solve→collect-changes
loop to a fixpoint and emit/write the **complete** set of USE / license /
keyword changes (and the resulting full plan), with `rc=0` when the closure is
satisfiable. No caller-side looping.

## Where

`portage-cli/src/query/depgraph/autounmask.rs` (+ the solve loop in
`query/depgraph/mod.rs`). Today autounmask changes are collected from a single
solved graph; they need to be fed back into config and re-solved until no new
change appears (bounded, like the existing upgrade fixpoint in `resolve_targets`).

## Repro

```bash
# in a fresh stage3 chroot (minimal profile), as root:
em -e --autounmask --autounmask-write -p www-client/firefox   # rc=1, partial plan
em -e --autounmask --autounmask-write -p www-client/firefox   # rc=1, more config
# ...repeats until rc=0 — should have been one call
```

Surfaced while validating emptytree toolchain behaviour in a stage3 chroot
(see [[em-emptytree]] — the `dev-lang/rust` slot-22 investigation).
