# Remove `--root` from `em crossdev` at the CLI level

Status: ✅ done 2026-08-30 — per-applet `Topology`/`RootArg` mixins, a real
`Applet::Emerge`, and `parse_cli_from` (bare `em` ≡ `em emerge`).

## Current shape

`Cli`'s own root carries `pretend`/`verbose`/`quiet`/`info`/`json`/`arch`/
`repo`/`color`. `--arch`/`--repo` are `global = true`. Topology flags live
in two mixins (`cli/topology.rs`):

- `Topology` — `--prefix`/`--local`/`--config-root`/`--vdb`/`--target`
- `RootArg` — `--root` alone, so it can be excluded per applet

Flattened onto every applet that resolves a `Roots`. `CrossdevArgs`
flattens `Topology` and never `RootArg`, so `--root` + `crossdev` is a
clap parse error in any position. The old runtime `bail!` (`8207e0f`)
is gone.

The word `emerge` is optional: `parse_cli_from` retries a failed parse
with `emerge` after argv0 when no sibling applet is in argv. Help/version
are not retried. Topology flags belong *after* a non-emerge applet
(`em stages --target T --stage1`, not `em --target T stages --stage1`).
Bare `em --root R cat/pkg` still works (retried as `em emerge …`).

`--json` is output format for both `--info` and merge-plan `-p`; it does
not require `--info`. `Cli.json` folds into `merge_flags()`.

Verified: `em crossdev --setup --root R` and `em --root R crossdev
--setup` fail to parse; `em crossdev --help` does not list `--root`;
`em toolchain`/`em stages`/`em setup`/`em emerge` and bare `em <atoms>`
still take `--root`.

See `docs/user/root-model.md`.
