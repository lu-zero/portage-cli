<!-- @generated from em's usage spec; do not edit -->
# `em active set`

- **Usage:** `em active set [REF]`

Register the invocation's `--prefix` or `--local` as the active context

Without arguments, reads from `--prefix`/`--local` flags:
  `em active set --prefix /home/me/prefix`
  `em active set --local=`           (default `~/.gentoo`)
  `em active set --local /other`

With a reference argument, activates an existing entry:
  `em active set my-name`     # by name
  `em active set 0`           # by index
  `em active set /path/to/dir` # by exact path

Note: `em active --local set` is wrong — `--local` takes `set` as the `--local` path. Put `set` first, or use `em active set --local=`.

## Arguments
- **`[REF]`** — Reference to an existing entry (name, index, or path) to activate

  If not provided, creates a new entry from --prefix/--local flags.

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
