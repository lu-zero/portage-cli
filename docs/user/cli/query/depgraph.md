<!-- @generated from em's usage spec; do not edit -->
# `em query depgraph`

- **Usage:** `em query depgraph [FLAGS] <ATOM>…`

Display full dependency tree

## Arguments
- **`<ATOM>…`** — Atom(s) to resolve and display the dependency tree for

## Flags
- **`-f --format <FORMAT>`** — Output format

  **Choices:** `pretty`, `json`, `tree`

  **Default:** `pretty`
- **`--autosolve-use`** — Let the solver choose USE flags to satisfy REQUIRED_USE (Level C)
- **`-e --emptytree`** — Treat every atom as not-yet-installed (emerge's `-e`/`--emptytree`)
- **`-o --onlydeps`** — Only show dependencies, excluding the given atoms themselves from the tree
- **`--with-bdeps`** — Include build-time dependencies (BDEPEND) in the resolution
- **`--root-deps`** — emerge's `--root-deps[=rdeps]`: only require RDEPEND (not DEPEND) to be satisfiable in the merge target.
- **`-h --help`** — Print help

## Depgraph

How far to re-examine installed dependencies.
- **`-D --deep`** — Re-examine transitive dependencies

  With `--update` (`-uD`), upgrades installed packages in the depgraph to the newest accepted in-slot version (emerge `-uD`). Alone, still bumps `:*` any-slot deps to the newest slot rather than keeping a satisfying installed slot.
- **`-N --newuse`** — Reinstall installed packages when their planned USE or IUSE differs from the VDB (emerge `--newuse`)

  Applies to packages that appear in the depgraph; pairs with `--deep` for a full-tree USE recheck.
- **`-U --changed-use`** — Like `--newuse`, but only rebuild when an *enabled* USE flag changed among flags present in both installed and current IUSE (ignore pure IUSE add/drop). Emerge's `--changed-use` / `-U`.

## Output Formats

- **`json`** — Machine-parsable JSON

  **Framing:** `json`
- **`pretty`** (default) — emerge -p style pretend output

  **Framing:** `text`
- **`tree`** — cargo tree style dependency tree

  **Framing:** `text`

## Examples

```
em query depgraph zlib
```
