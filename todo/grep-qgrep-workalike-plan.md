# `em grep` — qgrep workalike implementation plan

STATUS: **implemented 2026-09-17.** `em grep` now covers the full flag
surface below (`portage-cli/src/grep.rs` + the `GrepArgs` struct in
`cli.rs`), with unit tests in `grep.rs` and live parity tests against the
real `qgrep` in `portage-cli/tests/comparison.rs` (`cargo test -p
portage-cli --test comparison -- --ignored grep`) — all passing, including a
byte-exact stdout match for the default/`-N`/`-l`/`-c` cases. `-E` (eclass)
output is a set match, not byte-exact: `em grep` walks eclasses sorted by
name (deterministic); real `qgrep`'s own order is directory-iteration order.

**Real `qgrep`'s exit code is not a match/no-match signal once atom targets
are given — a genuine quirk, discovered while writing the parity tests.**
`qgrep -l EAPI=8 dev-python/setuptools` prints real hits and still exits 1;
the exact same query with no targets (searching the whole tree) exits 0.
Confirmed with `-q`/`-c`/`-l` and with multiple targets — exit is 1
whenever *any* atom target is given, independent of whether anything
matched. `em grep` deliberately does **not** replicate this: it exits 0
whenever at least one file matched and 1 (silently, via `crate::NoMatches`)
only when nothing matched at all, regardless of whether targets were given
— the standard, scriptable `grep(1)` convention, and arguably more correct
than what real `qgrep` does here.

`em grep` currently exists only as a CLI shape (`GrepArgs` in `cli.rs`,
`<PATTERN> [PATHS]...`) whose dispatch (`dispatch.rs`) does
`bail!("not implemented: grep")`. See
[`unimplemented-surface.md`](./unimplemented-surface.md)'s "Standalone
applets" section for the original "no known consumer" framing — this plan
supersedes that with a concrete scope decision and implementation path,
produced by surveying the real `qgrep` (portage-utils) installed on the dev
host, not from memory. (Kept below for reference/history now that it's
implemented — the design it describes is what actually landed.)

## 1. Survey results

**`qgrep` on this host: portage-utils 0.97.1.** Full flag set: `-I -i -N -c
-l -L -e -x -J -E -s -R -S<arg> -B<n> -A<n> --root<arg> -v -q -C --color -h
-V`.

Behaviours verified empirically (not from memory):

| Observation | Evidence |
|---|---|
| Searches **all `repos.conf` repos** by default | `qgrep -R EAPI=8` returned hits labelled `gentoo`, `guru`, `exp-llvm-libc` |
| Default label = **repo-relative path** | `dev-python/setuptools/setuptools-79.0.1.ebuild:PYTHON_COMPAT=(…` |
| `-N` label = atom w/ version | `dev-python/setuptools-79.0.1:…` |
| `-R` label = atom + repo, implies `-N` | `dev-python/setuptools-79.0.1::gentoo:…` (and warns `--with-name and --with-filename are incompatible… the former wins`) |
| `-vv` adds line numbers; plain `-v` changes nothing (filename already default) | `…setuptools-79.0.1.ebuild:11:PYTHON_COMPAT=…` |
| `-q` drops the label entirely | prints bare matching lines |
| `-c` = `label:count`, `-l` = labels only, `-L` = labels with no match | confirmed |
| Context lines use `-` as separator, match lines `:`; **no `--` group separator** | `-B1 -A1` output |
| Default pattern is a **literal substring**; `-e` switches to regex | `PYTHON_COM.AT` matched only with `-e` |
| Targets are **atoms**, not paths | `dev-lang/` (whole category), `>=dev-python/setuptools-83`, bare `setuptools` all worked |
| `-E` labels are `eclass/foo.eclass`; `-J` labels use tree-path shape over VDB copies | confirmed |
| Exit **1** on no match (grep convention) | `qgrep zzznomatchzzz …; echo $?` → 1 |

**What `em` already has (all the expensive parts):**

- Repo enumeration: `Cli::search_repos()` (`portage-cli/src/cli.rs:432`) —
  `--repo` wins, else every `repos.conf` entry, else host-tree fallback.
  Exactly `qgrep`'s default. `Cli::repo_path()` (single-repo) is the wrong
  one here.
- `RepoArg` mixin at `portage-cli/src/cli/context.rs:74`; `VerboseArg`/
  `QuietArg` in the same file; registration switches `Cli::repo_flag()`
  (`cli.rs:513`) and `Cli::verbose()` (`cli.rs:439`).
- Ebuild walking: `Repository::ebuilds()` (`portage-repo/src/repo/repository.rs:573`)
  is already a parallel `jwalk` walk, masters-aware, symlink-following,
  sorted by CPV, with a `.filter()` combinator. **Do not write a new walker.**
- `Repository::eclasses()` (`repository.rs:978`) gives eclass names →
  `<repo>/eclass/<n>.eclass`. So the ebuild/eclass split is cheap.
- `-J`: `crate::vdb::open_cli_vdb(cli)` (`portage-cli/src/vdb.rs:104`, honours
  `--root`/`--prefix`) + `portage_vdb::Package::path()` → `<path>/<pf>.ebuild`.
- Atom target matching: `portage_atom::Dep::matches_cpv` (`portage-atom/src/dep.rs:127`).
- **Parallel read+decode-in-chunks pattern to copy verbatim**:
  `read_and_decode` in `portage-repo/src/cache.rs:598` (chunk by
  `available_parallelism()`, one `tokio::task::spawn_blocking` per chunk,
  `fs::read_to_string` + closure, concat).
- **`regex` needs no new dependency**: `regex = "1"` is already a
  `[workspace.dependencies]` entry (`Cargo.toml:136`) and already a direct
  dep of `portage-repo`, so it is compiled for every `em` build today
  (`cargo tree -p portage-cli -i regex` confirms). `memchr` and
  `aho-corasick` are already *direct* deps of `portage-cli`.
- Silent-exit-1 convention: `error::NoValidAtoms` / `ConfigChangesNeeded`
  (`portage-cli/src/error.rs`) downcast in `main.rs:90`.

## 2. Scope decision: near-full `qgrep` surface, with three renames

Implement everything except `-V` (root `em --version`), `-C`/`--color` (root
`--color=<WHEN>` + `anstream` already does this), and `--root` (subsumed by
`RootedTopology`). Treat `-x` as an alias of `-e`.

Justification: the *cost* here is the walk + parallel read + matcher, which
already exist in the crate and are shared by every flag. Every remaining
`qgrep` flag is output-shaping — a variant on one `Emit` enum and one `Label`
enum, a few lines each. A "pragmatic subset" would save almost no code while
creating a permanent "which subset?" divergence for a workalike whose whole
value is being drop-in. Conversely the flags being dropped are the only ones
that would need *new* machinery (a second color system, a second root
model).

Three renames are forced by collisions with `em`'s existing global
vocabulary, and must be documented in the arg doc comments:

- `qgrep -R/--repo` collides with `em`'s `RepoArg --repo <PATH>`. →
  `-R/--show-repo` (short letter preserved).
- `qgrep -q/--quiet` means "drop the filename prefix", whereas `em`'s
  `QuietArg` means "suppress non-error output" and feeds `diag::init`. →
  `-q/--no-filename` (short letter preserved); **do not** flatten
  `QuietArg` onto `GrepArgs`.
- `qgrep` needs `-vv` for line numbers. `em` flattens `VerboseArg` (count,
  `-vv`/`-vvv` = debug logs) so `-v` ⇒ line numbers here; document the
  one-`v` difference.

## 3. Flag mapping

| qgrep | `em grep` | Behaviour |
|---|---|---|
| `-I --invert-match` | same | invert the per-line verdict |
| `-i --ignore-case` | same | literal→`Regex(escape+case_insensitive)`, regex→`RegexBuilder::case_insensitive` |
| `-N --atom-name` | same | label = `cat/pn-ver` from `Ebuild::cpv()` |
| `-c --count` | same | `label:N`, one line per file with ≥1 match |
| `-l --list` | same | labels of matching files only |
| `-L --invert-list` | same | labels of non-matching files only |
| `-e --regexp` | same | compile with `regex` |
| `-x --extended` | same | alias of `-e`; Rust `regex` has no BRE/ERE split — say so in the doc comment |
| `-J --installed` | same | walk `open_cli_vdb(cli)` instead of the tree |
| `-E --eclass` | same | walk `<repo>/eclass/*.eclass` via `Repository::eclasses()` |
| `-s --skip-comments` | same | skip lines whose first non-blank char is `#` |
| `-R --repo` | **`-R --show-repo`** | label = `cat/pn-ver::reponame`; implies `-N` |
| `-S --skip <arg>` | same | skip lines matching `<arg>` using the same matcher mode as the pattern |
| `-B --before <n>` | same | `n` leading context lines, `-` separator |
| `-A --after <n>` | same | `n` trailing context lines, `-` separator |
| `-v --verbose` | `VerboseArg` | `>=1` ⇒ prepend `:<lineno>` after the label |
| `-q --quiet` | **`-q --no-filename`** | print bare matching lines |
| `--root` | `RootedTopology` mixin | already the `em` model; drives `-J`'s VDB and `repos.conf` discovery |
| `-C`/`--color` | root `em --color=<WHEN>` | `anstream` + `ColorChoice::write_global()` in `main.rs` |
| `-h`, `-V` | `--help` (usage-generated), root `--version` | — |
| `[pkg …]` | `[TARGETS]…` | **rename `paths` → `targets`**; atom semantics, not paths |

## 4. Types and files

**`portage-cli/src/cli.rs:2680`** — replace the placeholder `GrepArgs`:

```rust
/// `em grep` — search inside ebuilds and eclasses (qgrep workalike)
#[derive(usage::Args, Debug, Clone)]
#[usage(effect = "read", example = "em grep PYTHON_COMPAT dev-python/setuptools")]
pub struct GrepArgs {
    pub pattern: String,
    // -i -I -e -x -s -S -N -R -c -l -L -E -J -q -B -A per the table, all `#[usage(short = .., long)]`
    // -l/-L/-c mutually exclusive via `conflicts(..)` (see cli.rs:2588 for the spelling)
    /// Restrict the search to these atoms (`cat/pn`, `cat/`, bare `pn`, `>=cat/pn-1.2`)
    #[usage(double_dash = "automatic")]
    pub targets: Vec<String>,
    #[usage(flatten)] pub topology: RootedTopology,
    #[usage(flatten)] pub repo_arg: RepoArg,
    #[usage(flatten)] pub verbose_arg: VerboseArg,
}
```

Register `Some(Applet::Grep(a)) => a.repo_arg.repo.clone()` in
`Cli::repo_flag()` and `a.verbose_arg.verbose` in `Cli::verbose()`. Add
`#[usage(help = …)]` unchanged on the `Applet::Grep` variant.

**New `portage-cli/src/grep.rs`** (~350 lines), `pub(crate) mod grep;` in
`lib.rs` (alphabetically next to `glsa`):

- `pub struct Options` — flat, `Copy`-ish config built in `dispatch.rs` from
  `GrepArgs`.
- `enum Matcher { Literal(memchr::memmem::Finder<'static>), Regex(regex::Regex) }`
  with `fn find(&self, line: &str) -> bool`. One constructor
  `Matcher::new(pat, regex: bool, ignore_case: bool)`: `-i` on a literal
  routes through `RegexBuilder::new(&regex::escape(pat)).case_insensitive(true)`
  so there is exactly one case-folding code path and no new dependency.
- `enum Mode { Ebuilds, Eclasses, Installed }`, `enum Emit { Lines, Count,
  List, InvertList }`, `enum Label { Path, Atom, AtomRepo }`.
- `struct Target` — `Cat(String)` / `Pn(String)` / `Dep(portage_atom::Dep)`,
  parsed once from `targets`; `fn matches(&self, cpv: &Cpv) -> bool`.
- `struct Job { label: String, path: camino::Utf8PathBuf }` — label
  pre-rendered during discovery so workers never touch `Repository`.
- `pub async fn run(cli: &cli::Cli, opts: &Options) -> Result<()>`.

**`portage-cli/src/error.rs`** — add `NoMatches` alongside `NoValidAtoms`,
re-export in `lib.rs:63`, and downcast it in `main.rs:90` so `em grep` exits
1 silently on no match, like `grep(1)` and `qgrep`.

**`portage-cli/src/dispatch.rs:306`** — replace `bail!("not implemented:
grep")` with an `Options` build + `grep::run(cli, &opts).await`, same shape
as the `SearchArgs` impl directly below it.

## 5. Search implementation

Phase 1 — discovery, serial, cheap:
- `Ebuilds`: for each `p` in `cli.search_repos()`, `repo_open::open(p)`
  (skip-with-warning on failure, exactly as `search.rs::open_repos` does),
  then `repo.ebuilds()?` — already a parallel `jwalk` walk. Apply
  `Target::matches` as a `.filter()` closure so non-matching ebuilds never
  allocate. Label rendered per `Label`. Iterate repos in `search_repos()`
  order and `collect_vec()` (sorted by CPV) per repo ⇒ deterministic output.
- `Eclasses`: `repo.eclasses()?`, label `eclass/<n>.eclass`. Targets are
  meaningless here — warn and ignore if both given (mirrors `qgrep`'s own
  incompatible-option warning style).
- `Installed`: `open_cli_vdb(cli)?.packages()` →
  `pkg.path().join(format!("{}.ebuild", pf))`, filtered by
  `Target::matches`, sorted by CPV.

Phase 2 — match, parallel: copy `read_and_decode` (`portage-repo/src/cache.rs:598`)
shape. Chunk `Vec<Job>` into `available_parallelism()` slices; one
`spawn_blocking` per chunk does `fs::read_to_string` + line scan + **formats
into a per-chunk `String`**; await handles in order and write the
concatenated buffers through `anstream::stdout()`. Per-chunk buffering keeps
ordering deterministic and avoids the shared-mutex contention the `cache.rs`
comment calls out. Return the total match count for the `NoMatches`
decision.

Line scan per file: `for (idx, line) in text.lines().enumerate()`, skip on
`-s` / `-S`, verdict = `matcher.find(line) ^ invert`. Context (`-B`/`-A`)
needs a small ring buffer for leading lines and an "after" countdown; emit
match lines with `:`, context lines with `-`, **no `--` group separator**
(matches observed output).

Output styling: label in `C_PKG` (`Label::Path`) or
`C_CAT`/`C_PKGNAME`/`C_VERSION` (`Label::Atom`, matching `query/list.rs`),
matched substring in `C_BOLD`. These are `anstyle::Style` consts that
`Display` into ANSI unconditionally and get stripped by `anstream::stdout()`,
so worker threads can format them without knowing the color choice.

## 6. Tests

- `#[cfg(test)] mod tests` in `grep.rs`: `Matcher` literal vs regex vs `-i`;
  `-s` comment predicate; `Target` parsing/matching for `dev-lang/`, bare
  `setuptools`, `>=dev-python/setuptools-83`; `Label` rendering for all
  three variants; the context-window emitter (`:` vs `-`, no `--`);
  `Emit::{Count, List, InvertList}`; plus one end-to-end `run()` over a
  `tempfile` repo fixture (build `profiles/categories` + a couple of
  `.ebuild` files the way `clean.rs`/`pkg.rs` tests already do —
  `test_support.rs` only carries env-var locks, no repo builder).
- `portage-cli/tests/comparison.rs`: add `#[ignore]` parity cases `em grep`
  vs `qgrep` for default / `-l` / `-c` / `-N` / `-E`, using the existing
  `em()`/`q()` helpers. This is the established home for q-tools parity
  (`cargo test -p portage-cli -- --ignored`).
- Docs: regenerate with `UPDATE_CLI_DOCS=1 cargo test -p portage-cli --test
  cli_docs` (rewrites `docs/user/cli/grep.md`). Also add a `qgrep → em
  grep` row to `docs/user/intro.md:21`'s mapping table, and update
  `unimplemented-surface.md` section 4 + its "suggested order" item 4,
  which currently says grep is unimplemented and not worth it.

## 7. Implementation order

1. `Matcher` + the per-file line scanner + all emitters, in `grep.rs`, with
   unit tests — pure functions, no I/O, no CLI.
2. `Target` parsing/matching + unit tests.
3. Discovery for `Mode::Ebuilds` over `cli.search_repos()`; parallel phase
   2; `run()` wired with default label/emit only.
4. `cli.rs` arg surface + `repo_flag()`/`verbose()` registration +
   `dispatch.rs` wiring; regenerate CLI docs. **First usable commit** —
   `em grep PAT [atoms]` works end to end.
5. Labels (`-N`, `-R`), emitters (`-c`, `-l`, `-L`), `-q`, `-v` line
   numbers.
6. `-E` eclasses and `-J` installed modes.
7. `-B`/`-A` context, `-s`, `-S`.
8. `error::NoMatches` + `main.rs` downcast (exit 1 on no match).
9. Colorized match highlighting; `comparison.rs` parity tests; doc/todo
   updates.

### Critical files
- `portage-cli/src/cli.rs`
- `portage-cli/src/dispatch.rs`
- `portage-cli/src/search.rs` (closest sibling applet — repo-tree-scanning
  query, `open_repos` pattern)
- `portage-repo/src/cache.rs` (parallel read+decode pattern to copy)
- `portage-repo/src/repo/repository.rs` (`ebuilds()`/`eclasses()` walkers)
