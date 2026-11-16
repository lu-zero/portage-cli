# Default options from the config, and sets in `em -s`

Status: plan, nothing implemented. Written 2026-11-16.

## Why

- `em` never reads `EMERGE_DEFAULT_OPTS`; only `em info` prints it. A host that carries `-j80 -l80` there runs `em` sequentially.
- `emerge -s` lists package sets (`world`, `system`, ...) after the packages. `em -s` lists none.

## Part 1: `EMERGE_DEFAULT_OPTS` and `EM_DEFAULT_OPTS`

### Behaviour

The effective command line of an `emerge` run is, left to right:

1. `EMERGE_DEFAULT_OPTS`: portage's variable; flags `em` does not support are dropped.
2. `EM_DEFAULT_OPTS`: `em`'s own variable; an unknown flag is an error, since the user wrote it for `em`.
3. The command line. For a scalar flag the last one wins, so the command line overrides both.

- `--ignore-default-opts` skips 1 and 2.
- Both variables are read from the environment first, then `make.conf` (and `make.conf.d`) under the config root. `make.conf` is read as portage reads it, not as bash: `MakeConf::get`, no sourcing.
- Scope: the `emerge` applet only (`em -ua @world`, `em emerge ...`, the mode-flag retry). Other applets have their own flags, and an emerge flag would be an error there.
- A bad value for a supported flag (`--jobs=abc`) is an error that names the variable it came from.

### Mechanics

`parse_cli_or_exit` parses once, and the config root is only known afterwards, as emerge also parses twice.

1. Parse the arguments as they are. If the applet is not `Emerge`, or `--ignore-default-opts` is given, stop here.
2. Read the variables for `cli.roots().config()`. Split them like a shell would (no expansion). Add `shlex` as a dependency, or write 30 lines; the choice is open.
3. Build the argv of the second parse. The grammar already says every word after a leading flag is an emerge argument, so the rule is exact:
   - the first word is `emerge`: insert after it;
   - otherwise (a leading flag, `-r`, or a bare atom): insert at the front.
4. Drop the flags `em` does not support, in group form (`--flag`, `--flag=value`, `--flag value`).
   - Probe each group with `Cli::parse_from(["emerge", group])`. An `ErrorKind::UnknownFlag` error drops the group; any other error is reported.
   - An unknown flag without `=` takes the next word as its value when that word does not start with `-`. Atoms in a default-options variable are not a thing, so the worst case is a dropped stray word. Say this in the docs.
   - Known cost: N small parses, N being 3 to 6.
5. Parse `[em, <EMERGE kept>, <EM>, <user args>]`.

### Visibility

- Dropped flags are a `debug!` line plus a list in `em info` ("EMERGE_DEFAULT_OPTS: -j80 -l80 (ignored: --alert)"). No warning on every run; the flag is the user's own, not an error.
- `em info` also shows the effective list and which source each flag came from.

### Risks to settle before landing

- Applying `-j80 -l80` changes what existing runs do. `--jobs` here is packages in parallel, and MAKEOPTS is separate. This is the point of the feature, but the first run after the change should be a `-p`.
- `--keep-going` has no em equivalent by design (AGENTS.md), so it falls out through the "unsupported" path. Test that it is dropped and not accepted.
- `--ask` in the default options makes a script prompt. The same holds in emerge; no special case.
- `--color`, `--quiet`/`--verbose` and `--pretend` in the options must go through the same re-parse, so `Cli::color.write_global()` and `diag::init` see the final values: build `Cli` once, from the second parse.
- `privilege::maybe_supervise` re-execs `em` under a fake root. The re-exec must not apply the defaults twice. Pass the final argv, or set the ignore marker in the child's environment.

### Tests

- Unit: the splitter; group dropping (unknown with `=`, unknown with a word, unknown at the end, known with a bad value); ordering and override; the insert position for `emerge`, a leading flag and a bare atom.
- Unit: `--ignore-default-opts`; the environment over `make.conf`; `make.conf.d`.
- Integration: `em -p` with a `-j` in `EMERGE_DEFAULT_OPTS`, against a scratch root.
- Live: `EMERGE_DEFAULT_OPTS="--alert --jobs=4"` plan equals the plan with `--jobs=4` on the command line.

### Commits

1. `feat(cli): read EMERGE_DEFAULT_OPTS and EM_DEFAULT_OPTS for emerge runs` (reader, splitter, two-pass parse, drop, tests).
2. `feat(info): show the default options and the flags dropped from them`.
3. `docs(user): default options` plus `docs/design` for the two-pass parse.

## Part 2: sets in `em -s`

### Behaviour

- After the packages, list each set whose name matches the key: `*  name`, then `Description:   ...`. The result count includes the sets, as emerge's does.
- The name matched is the part after the last `/` unless the key is a category match (`@`, `/`), as for packages. `-S` also matches the description. Similarity matching applies to set names like it does to package names.
- Descriptions are em's own wording; none is copied from portage. Proposed:

| Set | Description |
|-----|-------------|
| `system`, `profile` | `System packages of profile <path>`, `Profile packages of profile <path>` |
| `world` | `Packages installed by name, and their sets` (a world file) |
| `selected`, `selected-packages`, `selected-sets` | one line each |
| a file under `/etc/portage/sets` | `Package set from <path>` |
| another section of `sets.conf` | `Package set <class>` using the class, else `Package set` |

- Names come from `KnownSets`, extended to keep the order the files declare them in (a `Vec` beside the `HashSet`). The order is not sorted, to stay close to emerge's.
- The text does not match emerge's descriptions, so a `-S` key that hits a set by its description can differ. Accepted and recorded in the docs.

### Code

- `portage-cli/src/maint/sets.rs`: `KnownSets::ordered()`.
- `portage-cli/src/search.rs`: collect the sets after the package pass; a small `describe_set(name, ...)`.
- No resolver or policy code involved, so no go-ahead is needed beyond this plan.

### Tests

- Matching of names with and without a category key; `-S` over descriptions; the count; the order; a user set file.
- Live: names and order against `emerge -s` for `world`, `system`, `^selected`.

### Commit

`feat(search): list the sets that match a key`.

## Order

Part 2 first (small, self-contained, a day), then Part 1 (the parse path is shared by every run, so it gets an Opus review before landing).

## Open questions

- Should `EM_DEFAULT_OPTS` apply to the other applets (a per-applet variable like `EM_QUERY_DEFAULT_OPTS`)? The plan says no, until a flag is wanted there.
- `shlex` as a dependency, or our own splitter?
- Should the flags dropped from `EMERGE_DEFAULT_OPTS` be silent, as planned, or one warning per run?
