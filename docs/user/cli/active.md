<!-- @generated from em's usage spec; do not edit -->
# `em active`

- **Usage:** `em active [SUBCOMMAND]`

Register a default `--prefix` / `--local` so bare `em <pkg>` picks it up (dogfooding)

Explicit `--prefix`/`--local`/`--root` still win. State is stored under `$XDG_STATE_HOME/em/active`.

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Examples

**Register ~/.gentoo**

```
em active set --local=
```

**Flags may precede set**

```
em active --local= set
```

## Subcommands

- [`em active add [NAME]`](active/add.md)
- [`em active clear [--all]`](active/clear.md)
- [`em active env`](active/env.md)
- [`em active list`](active/list.md)
- [`em active remove <REF>`](active/remove.md)
- [`em active set [REF]`](active/set.md)
- [`em active show`](active/show.md)
