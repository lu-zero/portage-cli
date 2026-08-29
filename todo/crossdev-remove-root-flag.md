# Remove `--root` from `em crossdev` at the CLI level

Status: 🔴 not started. Runtime rejection landed 2026-08-29 (`8207e0f`,
tightened per review to a one-line `bail!`): `crossdev --setup`/
`--init-target` now errors if `globals.root.is_some()`. That's a
stopgap — `--root` is `global = true` on `Cli` (`cli.rs`), so it's
still accepted by clap and only caught at runtime, inside the
`crossdev` applet body.

## The actual task

Make `--root` structurally unavailable to `em crossdev`, not just
rejected after parsing — ideally a clap-level error (`unexpected
argument '--root'`), not `em`'s own `bail!`.

`--root` is `#[arg(..., global = true)]` on the top-level `Cli` struct,
so every subcommand inherits it today. Making it subcommand-scoped
means un-globalizing it and flattening it into each applet's own args
struct instead — touches every subcommand that legitimately uses
`--root` (`stages`, plain merges, `toolchain --setup`, etc.), not just
`crossdev`. Survey call sites of `globals.root` before starting.
