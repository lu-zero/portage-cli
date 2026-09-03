<!-- @generated from em's usage spec; do not edit -->
# `em read`

- **Usage:** `em read [FLAGS] [PACKAGE]`

Display Portage elog files

## Arguments
- **`[PACKAGE]`** — Only show packages whose `<category>/<pf>` contains this text

## Flags
- **`-l --list`** — List what is filed instead of printing the messages
- **`-n --limit <LIMIT>`** — Show only this many of the most recent packages; 0 for all

  **Default:** `10`
- **`--delete`** — Remove each file once it has been shown
- **`-h --help`** — Print help

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
