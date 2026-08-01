# brush: `declare -a`/`-A` on a Dynamic variable permanently destroys it (PIPESTATUS case)

STATUS: OPEN. Root-caused and reproduced in 3 lines of pure shell, no
em/eclass involved. Worked around on the `em` side (see below) so this is
not blocking anything in portage-cli anymore, but it's a real brush
correctness bug worth fixing/upstreaming — repo: `~/Sources/brush`.

## Symptom

Once a script explicitly `declare`s `PIPESTATUS` (e.g. via
`declare -a PIPESTATUS=([0]="1")`), brush **never updates it again** on any
later pipeline in that shell — it stays frozen at whatever was last
assigned. Real bash always replaces the *entire* array on every new
pipeline, unconditionally, regardless of any prior explicit `declare`.

Minimal repro (no em involved):

```bash
declare -a PIPESTATUS=([0]="1")
true | true | true
echo "${PIPESTATUS[@]}"
```

- **bash**: `0 0 0`
- **brush** (`~/Sources/brush/target/release/brush`): `1`

## Root cause

`PIPESTATUS` is implemented as a dynamic well-known variable:

`brush-core/src/wellknownvars.rs:409-422`
```rust
// PIPESTATUS
shell.env_mut().set_global(
    "PIPESTATUS",
    ShellVariable::new(ShellValue::Dynamic {
        getter: |shell| {
            ShellValue::indexed_array_from_strings(
                shell.last_pipeline_statuses().iter().map(|s| s.to_string()),
            )
        },
        setter: |_| (),
    }),
)?;
```

Every read is supposed to go through `getter` (live `last_pipeline_statuses()`),
and every write is supposed to be a no-op via `setter`. But
`declare -a NAME=(...)` doesn't go through that setter at all — the
`declare` builtin (`brush-builtins/src/declare.rs`, `process_declaration`,
~line 433) calls `var.convert_to_indexed_array()` *before* `var.assign(...)`
whenever `-a` was given. And `ShellVariable::convert_to_indexed_array`
(`brush-core/src/variables.rs:190-207`):

```rust
pub fn convert_to_indexed_array(&mut self) -> Result<(), error::Error> {
    match self.value() {
        ShellValue::IndexedArray(_) => Ok(()),
        ShellValue::AssociativeArray(_) => {
            Err(error::ErrorKind::ConvertingAssociativeArrayToIndexedArray.into())
        }
        _ => {
            let mut new_values = BTreeMap::new();
            new_values.insert(
                0,
                self.value.to_cow_str_without_dynamic_support().to_string(),
            );
            self.value = ShellValue::IndexedArray(new_values);   // <-- here
            Ok(())
        }
    }
}
```

falls into the `_` arm for a `ShellValue::Dynamic` value and **unconditionally
overwrites `self.value` with a plain, static `ShellValue::IndexedArray`** —
permanently discarding the `Dynamic { getter, setter }` binding. From that
point on "PIPESTATUS" is just an ordinary frozen array; nothing in the
pipeline-execution path ever calls `last_pipeline_statuses()` again for it,
because the dynamic getter that would have done so is gone.

`convert_to_associative_array` (same file, ~line 210-226) has the identical
pattern and almost certainly the same bug for any dynamic var converted via
`declare -A`.

## Why this matters beyond a toy repro

Any script that computes `PIPESTATUS` into a `local status=(...)` array
(which is precisely what the standard eclass `pipestatus()` helper —
`eapi9-pipestatus.eclass` — and countless other bash idioms do, though note:
a plain `local x=( "${PIPESTATUS[@]}" )` does *not* trigger this, since it
never calls `declare -a` with an *explicit* array-conversion path the same
way; the trigger specifically needs a `declare -a`/`-A` **conversion** call,
which is why this surfaced via `em`'s own `worker-env` dump/restore
mechanism restoring `declare -a PIPESTATUS=([0]="1")` line-for-line) will
silently get a stale/wrong `PIPESTATUS` for the rest of that shell's life.
This broke a real `distutils-r1.eclass` build (`dev-python/jinja2`, under
`portage-cli`'s Compile→Install worker split): its `pipestatus || die`
check fired incorrectly, misreported as "listing .../usr/bin failed" even
though the directory was fine — the pipe's actual two-stage
`(cd dir && find .) | sort > file` had genuinely succeeded, but the
stale/truncated PIPESTATUS array from the restored `declare` made
`pipestatus()` think it hadn't.

## Portage-cli side workaround (already landed, not blocking)

`portage-cli/src/ebuild.rs`, `capture_variables`/`filter_declare_dump`
(commit `5902b73`) now excludes `PIPESTATUS` and brush/bash's other
dynamic/special vars (`FUNCNAME`, `BASH_LINENO`, `BASH_SOURCE`,
`BASH_ARGV`/`BASH_ARGC`/`BASH_ARGV0`, `BASH_CMDS`, `BASH_COMMAND`,
`BASH_SUBSHELL`, `BASH_ALIASES`) from the Compile→Install worker-env dump,
so `em` itself no longer triggers this. This todo is purely about the
underlying brush bug, which could still bite any other script that
explicitly `declare -a`/`-A`s one of these.

## Suggested fix directions (not yet implemented)

- `ShellVariable::convert_to_indexed_array`/`convert_to_associative_array`:
  when `self.value()` is currently `ShellValue::Dynamic`, either (a) route
  through the existing `setter` instead of blindly replacing `self.value`
  (matching bash's real behavior where assigning to `PIPESTATUS` is
  accepted syntactically but has no lasting effect — see the `setter: |_|
  ()` already present), or (b) reject the conversion outright for
  known-non-convertible dynamic vars. Given the `setter` field already
  exists and is presumably meant for exactly this, (a) looks like the
  intended design that just isn't wired up in the conversion path.
- Same class of issue likely affects any other well-known `Dynamic`
  variable in `wellknownvars.rs` (worth grepping for `ShellValue::Dynamic`
  and checking each one against `declare -a`/`-A`/plain `assign`).
- A regression test mirroring the 3-line repro above (`declare -a
  PIPESTATUS=(...)` then a real multi-stage pipe, assert the array is
  fully live again) would be the cheapest guard against regressing this.
