<!-- @generated from em's usage spec; do not edit -->
# `em query`

- **Usage:** `em query [--root <PATH>] <SUBCOMMAND>`
- **Effect:** read-only

Query package information

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Flags
- **`-h --help`** — Print help

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

```
em query belongs /usr/bin/python
```

```
em query list -I
```

## Subcommands

- [`em query belongs <FILE>…`](query/belongs.md)
- [`em query check <ATOM>…`](query/check.md)
- [`em query depends <ATOM>…`](query/depends.md)
- [`em query depgraph [FLAGS] <ATOM>…`](query/depgraph.md)
- [`em query files <ATOM>…`](query/files.md)
- [`em query has <FIELD> [VALUE]`](query/has.md)
- [`em query hasuse <FLAG>…`](query/hasuse.md)
- [`em query keywords <ATOM>…`](query/keywords.md)
- [`em query list [-I --installed] [PATTERN]…`](query/list.md)
- [`em query meta <ATOM>…`](query/meta.md)
- [`em query size <ATOM>…`](query/size.md)
- [`em query uses <ATOM>…`](query/uses.md)
- [`em query which <ATOM>…`](query/which.md)
