# `gentoo-stages` should use the workspace's own domain types

Status: 🔴 not started, postponed 2026-09-02. Deliberately deferred — the
crate has an external consumer, so this is a semver-visible change, not a
free refactor.

## Not an orphan

`gentoo-stages` is a workspace member that nothing else in this workspace
depends on, which reads like dead weight from inside the repo. It is not:
**`crossdev-stages` consumes it** (Luca, 2026-09-02). It is a published
library that happens to live here, so the missing reverse edge is expected.

Worth stating plainly somewhere, because the next person to run a dependency
audit will draw the same wrong conclusion. (`portage-atom-resolvo` is a
genuinely different case — benchmarks-only, already recorded in
[[PENDING]]'s solver section.)

## The actual gap

It already depends on `gentoo-core` and uses `gentoo_core::Arch` for the
architecture — but keeps the variant as a bare `String`:

```rust
// gentoo-stages/src/stage3.rs
pub arch: Arch,
/// Stage3 variant (e.g. `openrc`, `systemd`, `musl-hardened`)
pub variant: String,
```

while `gentoo_core::variant::Variant` exists in that same crate, with
`parse(arch, flavor)`, `flavor()` and `keyword()`. `client.rs` then
hand-parses variants out of stage3 filenames
(`extract_variant_from_filename`) straight into those strings.

This is the "never downgrade a domain type" rule from AGENTS.md, with the
type sitting one import away rather than needing to be written.

## Why it is not free

`Stage3`'s fields are public and `Arch`/`Stage3` are re-exported from
`lib.rs`, so changing `variant`'s type breaks `crossdev-stages` at compile
time. Needs coordinating with that repo — check what it actually does with
`variant` before choosing between a typed field, an accessor returning
`Variant`, or a parallel typed accessor alongside the string.

Survey the consumer first; do not just change the field and fix the fallout.
