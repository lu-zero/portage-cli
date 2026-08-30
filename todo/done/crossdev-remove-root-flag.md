# Remove `--root` from `em crossdev` at the CLI level

Status: ✅ done — landed as part of a broader `Cli` rationalization
(per-applet `Topology`/`RootArg` mixins + a real `Applet::Emerge`), not
as a standalone fix.

## What changed

`--root` (and `--prefix`/`--local`/`--config-root`/`--vdb`/`--target`)
are no longer `global = true` on `Cli` at all — `Cli`'s own root now
carries only `pretend`/`verbose`/`quiet`/`info`/`arch`/`repo`/`color`.
Each now lives in one of two small mixins (`cli/topology.rs`):
`Topology` (`--prefix`/`--local`/`--config-root`/`--vdb`/`--target`) and
`RootArg` (`--root` alone, kept separate specifically so it can be
excluded per-applet). Both are flattened individually into every applet
that resolves a `Roots` — `CrossdevArgs` flattens `Topology` but
deliberately **never** `RootArg`.

Since `RootArg` is now mounted nowhere in `Cli`'s tree that is an
ancestor of `crossdev` (not even `Cli`'s own root, which no longer
exists as a mount point for it at all), `--root` combined with
`crossdev` is a genuine clap parse error — `unexpected argument
'--root'` — in any position, before or after the subcommand name. This
is stronger than the original ask ("ideally a clap-level error"): the
old runtime `bail!` in `crossdev::run()` (from `8207e0f`) is now
unreachable and has been deleted outright, since the situation it
guarded against can no longer be typed at all.

Verified: `em crossdev --setup --root R` and `em --root R crossdev
--setup` both fail to parse; `em crossdev --help` no longer lists
`--root`; `em toolchain`/`em stages`/`em setup`/`em emerge` (and the bare
`em <atoms>` path, which now parses into the same `Applet::Emerge`) are
unaffected.

See `docs/user/root-model.md` for the updated per-applet flag model
(replacing the old "all four are global" line).
