# Systematically Reviewed: `em` Subcommands and Options

> **Created**: 2026-08-09  
> **Purpose**: Central tracking file for CLI subcommand/option review findings.  
> **Scope**: All top-level applets, their subcommands, and global flags.

---

## Methodology

1. **Source**: Extracted from `portage-cli/src/cli.rs` (commit `53a0761`)
2. **Approach**: Enumerated every `Applet` variant + nested subcommand enums
3. **Validation**: Cross-checked against:
   - `README.md` applet status table
   - `docs/user/applets.md`
   - `test-scripts/regression-matrix.sh`
   - Known `todo/PENDING.md` gaps

---

## Global Flags (on `Cli` struct)

These apply across all applets unless shadowed by applet-specific variants.

| Flag | Short | Global | Default | Description |
|------|-------|--------|---------|-------------|
| `--pretend` | `-p` | ✅ | | Dry-run (no actions) |
| `--verbose` | `-v` | ✅ | | Verbosity level (count: 0-3) |
| `--quiet` | `-q` | ✅ | | Suppress non-error output |
| `--arch` | | | `Arch::current()` | Target architecture |
| `--repo` | | | | Pin search/query to single repository |
| `--prefix` | | ✅ | | Overlay root (install destination) |
| `--local` | | ✅ | `~/.gentoo` | Standalone prefix (own VDB/config) |
| `--privilege` | | | `Auto` | Fake root backend selection |
| `--search` | `-s` | | | Search package names |
| `--searchdesc` | `-S` | | | Search descriptions too |
| `--nodeps` | `-O` | | | Skip dependency resolution |
| `--unmerge` | `-C` | | | Remove packages completely |
| `--depclean` | `-c` | | | Remove unneeded packages |
| `--prune` | `-P` | | | Remove all but highest version |
| `--deselect` | `-W` | | | Remove from world file |
| `--resume` | `-r` | | | Resume last saved merge |
| `--root` | | ✅ | | Installation root offset |
| `--config-root` | | ✅ | | Config root (profile/make.conf source) |
| `--vdb` | | ✅ | | Override VDB path |
| `--target` | `-T` | ✅ | | Cross-build target tuple |
| `--color` | | | | Color output control (flattened) |
| `--activity-*` | | | | Activity bus flags (flattened) |
| `--depgraph-*` | | | | Depgraph flags (flattened) |
| `--merge-*` | | | | Merge flags (flattened) |

**Findings - Global Flags:**

1. **✅ Good**: Comprehensive topology control (`--root`, `--prefix`, `--local`, `--config-root`, `--target`)
2. **✅ Good**: Standard Portage parity flags (`-p`, `-s`, `-S`, `-O`, `-C`, `-c`, `-P`, `-W`, `-r`)
3. **✅ Resolved (2026-08-09)**: `--search`/`--searchdesc` on `Cli` and the `Search` applet are deliberately separate — global `-s`/`-S` drive `search::run_emerge_style` (emerge's own built-in search), the applet drives `search::run` (equery-style, `--all`/`--desc`/`--name-only`/`--homepage`). Same split as real Portage's `emerge -s` vs `equery`. Documented inline on `Cli::search`/`Cli::searchdesc` in cli.rs.
4. **✅ Resolved (2026-08-09)**: `em sync` and `em maint sync` both dispatch to `crate::maint::sync::run` — identical implementation, confirmed in `dispatch.rs`. `em sync` exists as a shorthand, matching real Portage having both `emerge --sync` and `emaint sync`. Documented inline in cli.rs.
5. Not a global flag today — `em sync` is its own applet, not `em --sync`. No indication this needs to change.
6. **⚠️ Issue**: `--local` uses `num_args = 0..=1` with `default_missing_value = ""` — non-intuitive that bare `--local` means `--local ~/.gentoo`

---

## Applet Catalog

### 1. Internal/Hidden Applets

| Applet | Hidden | Purpose | Status |
|--------|--------|---------|--------|
| `Helper` | ✅ (`name = "__helper"`) | Run do*/new* install helpers | Internal |
| `Worker` | ✅ (`name = "__worker"`) | Privilege-wrapped install worker | Internal |

**Findings:** Both are correctly hidden from help output. Used internally for build workflows.

---

### 2. Core Package Management (Default Path)

When no applet is specified, `em` behaves as the default emerge-like path.

| Flag | Behavior |
|------|----------|
| `-p` / `--pretend` | Show plan without merging |
| `-s` / `--search` | Search for packages |
| `-S` / `--searchdesc` | Search descriptions |
| `-O` / `--nodeps` | Skip dependency resolution |
| `-C` / `--unmerge` | Remove packages |
| `-c` / `--depclean` | Clean unneeded packages |
| `-P` / `--prune` | Remove old versions |
| `-W` / `--deselect` | Remove from world |
| `-r` / `--resume` | Resume saved merge |
| `-u` / `--update` | Update packages |
| `-D` / `--deep` | Deep update |
| `-1` / `--reinstall` | Reinstall |
| `--jobs` / `-j` | Parallel builds (from MergeFlags) |

**Findings:**

1. **✅ Good**: Covers all traditional emerge short flags
2. **⚠️ Missing**: `-n` / `--noreplace` not visible in Cli — may be in DepgraphFlags or MergeFlags
3. **⚠️ Missing**: `-N` / `--newuse` not visible in Cli — may be in DepgraphFlags
4. **⚠️ Missing**: `-U` / `--changed-use` not visible in Cli — may be in DepgraphFlags
5. **❓ Question**: Where are the `-u`, `-D`, `-1` flags defined? Not in Cli struct directly

---

### 3. Top-Level Applets (Leaf Commands)

#### 3.1 `Ebuild` — Execute ebuild phases

```
em ebuild <ebuild_path> <phase> [phase...]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--work-dir` | `-w` | Override build work directory |

**Options:**
- `ebuild_path`: Required, String
- `phase`: Required, Vec<String>

**Findings:**
- ✅ Simple and focused
- ✅ Matches traditional `ebuild` behavior
- ⚠️ Consider adding `--pretend` support for phase dry-run

---

#### 3.2 `Sync` — Sync repositories

```
em sync [repo...]
```

| Flag | Description |
|------|-------------|
| (none - takes repo names as positional args) |

**Options:**
- `repos`: Vec<String> — Repo names from repos.conf

**Findings:**
- ✅ Clean and simple
- ✅ Matches `emaint sync` behavior
- ✅ Resolved (2026-08-09): also exists as `MaintCommand::Sync`, confirmed identical (`crate::maint::sync::run` in both) — see Cross-Cutting Concerns §A

---

#### 3.3 `Depclean` — Remove orphaned packages

```
em depclean [atom...]
```

**Options:**
- `atoms`: Vec<String> — Optional list of atoms to clean

**Findings:**
- ✅ Simple
- ✅ Resolved (2026-08-09): `em -c` and `em depclean [atoms]` are the same implementation — `depclean::run(cli)` just forwards to `depclean::run_with_targets(cli, &cli.atoms)`. The applet form exists so scripts can pass atoms without also setting the global `-c` flag semantics. Documented inline on `Cli::depclean` in cli.rs.

---

#### 3.4 `Portageq` — Query Portage internal variables

```
em portageq <command> [args...]
```

**Options:**
- `command`: Required, String
- `args`: Vec<String> (trailing_var_arg)

**Findings:**
- ✅ Matches portageq behavior
- ⚠️ No subcommands defined — free-form command passing
- ❓ Question: Is this the right approach or should it have explicit subcommands?

---

#### 3.5 `Regen` — Regenerate metadata cache

```
em regen [repo...]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--output` | `-o` | Write cache to directory instead of metadata/md5-cache |
| `--repos-dir` | | Directory containing master repositories |
| `--jobs` | `-j` | Number of parallel workers |
| `--dedup` | | Deduplicate top-level dep tokens |

**Options:**
- `repos`: Vec<String>
- `activity`: ActivityArgs (flattened)

**Findings:**
- ✅ Good feature set
- ✅ Parallel processing support
- ⚠️ `--repos-dir` might conflict with global `--repo` — needs clarification

---

#### 3.6 `Quickpkg` — Create binary packages

```
em quickpkg <atom> [atom...]
```

| Flag | Description |
|------|-------------|
| `--include-config` | Include CONFIG_PROTECT files (bool flag, default off) |
| `--include-unmodified-config` | Include unmodified CONFIG_PROTECT files (bool flag, default off) |

**Options:**
- `atoms`: Required, Vec<String>

**Findings:**
- ✅ Matches quickpkg behavior
- ✅ Fixed (2026-08-09): `--include-config`/`--include-unmodified-config` are now plain clap bool flags (`--include-config` / no flag = off), not `String` "y|n". `quickpkg.rs` internals were already `bool` — only the CLI layer's `yn()` string parser in `dispatch.rs` needed removing.

---

#### 3.7 `MirrorDist` (alias: `emirrordist`) — Build distfiles mirror

```
em mirrordist [--repos-dir DIR] --distfiles DIR [options]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--repos-dir` | | Directory containing master repositories |
| `--distfiles` | | **Required** — Distfiles directory to populate |
| `--jobs` | `-j` | Concurrent downloads |
| `--delete` | | Delete distfiles no longer referenced |
| `--deletion-delay` | | Grace period before deleting (default: 7d) |
| `--deletion-db` | | Deletion-grace state file |
| `--success-log` | | Log of fetched files |
| `--failure-log` | | Log of fetch failures |
| `--scheduled-deletion-log` | | Report of files scheduled for deletion |
| `--whitelist-from` | | Files listing distfiles to never remove |
| `--verify-existing-digest` | | Re-hash existing files |
| `--gentoo-mirrors-fallback` | | Also try GENTOO_MIRRORS |
| `--delete-allow-incomplete` | | Allow --delete even with incomplete cache |

**Options:**
- `repo`: Option<String> — repos.conf name or path

**Findings:**
- ✅ Comprehensive mirror management
- ✅ Good logging support
- ⚠️ `--distfiles` is required but has no short flag
- ⚠️ Many logging options — consider if they should be unified

---

#### 3.8 `Setup` — Bootstrap a prefix layout

```
em setup [--prefix DIR | --local [DIR] | --root DIR]
```

**Options:** None (relies on global topology flags)

**Findings:**
- ✅ Simple — uses global `--prefix`/`--local`/`--root` flags
- ⚠️ No applet-specific options
- ⚠️ From code review: writes layout but no repos.conf/make.profile for --local
- 🔴 **Issue**: Needs `--profile`, `--repo-location`, `--sync` options for full bootstrap

---

### 4. Query Applet (`em query <subcommand>`)

#### 4.1 Query Subcommands

| Subcommand | Alias | Required Args | Description |
|------------|-------|---------------|-------------|
| `belongs` | `b` | file(s) | Find which package owns a file |
| `check` | `k` | atom(s) | Verify checksums |
| `depends` | `d` | atom(s) | List reverse dependencies |
| `depgraph` | `g` | atom(s) | Display full dependency tree |
| `files` | `f` | atom(s) | List files installed by package |
| `has` | `a` | atom(s) | List packages matching env data |
| `hasuse` | `h` | flag(s) | List packages with USE flag in IUSE |
| `keywords` | `y` | atom(s) | Display keyword status |
| `list` | | pattern(s) | List installed/available packages |
| `meta` | `m` | atom(s) | Display package metadata |
| `size` | `s` | atom(s) | Display total file size |
| `uses` | `u` | atom(s) | Display USE flags for package |
| `which` | `w` | atom(s) | Print full path to ebuild |

**Depgraph-specific flags:**
- `--format` (-F): pretty, json, tree (default: pretty)
- `--autosolve-use`: Let solver choose USE flags for REQUIRED_USE
- `--emptytree` (-e): Treat all as not installed
- `--onlydeps` (-o): Only dependencies
- `--with-bdeps`: Include BDEPEND
- `--root-deps`: Only require RDEPEND satisfiable

**List-specific flags:**
- `--installed` (-I): Only installed packages

**Findings:**
- ✅ Excellent equery parity
- ✅ All single-letter aliases match equery
- ✅ Comprehensive depgraph options
- ⚠️ `--format` short flag `-F` might conflict with other uses
- ❓ Question: Does `em query list` without args list ALL packages? (pattern is `Vec<String>` with no `required`)

---

### 5. Clean Applet (`em clean <target>`)

| Subcommand | Description |
|------------|-------------|
| `dist` | Clean outdated distfiles |
| `pkg` | Clean outdated binary packages |

**Findings:**
- ✅ Simple and clear
- ⚠️ Only two targets — matches eclean behavior
- ❓ Question: Are there more eclean targets we should support?

---

### 6. Use Applet (`em use`)

```
em use [-a FLAG | -r FLAG] [--make-conf PATH]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--add` | `-a` | Add (enable) flags |
| `--remove` | `-r` | Remove (disable) flags |
| `--make-conf` | | Path to make.conf (default: auto-resolved) |

**Options:**
- `add`: Vec<String>
- `remove`: Vec<String>
- `make_conf`: Option<camino::Utf8PathBuf>

**Findings:**
- ✅ Matches euse behavior
- ✅ Supports both add and remove in one call
- ⚠️ No `--list` or `--query` mode like euse has
- ❓ Question: Should we add `em use` (no flags) to list current USE? (Code shows it does)

---

### 7. Pkg Applet (`em pkg <subcommand>`)

| Subcommand | Description | Flags |
|------------|-------------|-------|
| `use` | Edit package.use | `-a`, `-s`, `-d`, `--path` |
| `keyword` | Edit package.accept_keywords | `-a`, `-s`, `-d`, `--path` |
| `mask` | Add/remove from package.mask | `-a`, `-d`, `--path` |
| `env` | Edit package.env | `-a`, `-d`, `--path` |

**Use/Keyword/Env flags:**
- `-a` / `--add`: Add entry
- `-s` / `--subtract`: Subtract entry
- `-d` / `--drop`: Drop entirely
- `--path`: Target file (default: auto-derived from atom)

**Mask flags:**
- `-a` / `--add`: Add to mask
- `-d` / `--drop`: Remove from mask
- `--path`: Target file

**Findings:**
- ✅ Comprehensive package.* file management
- ✅ Consistent flag patterns across subcommands
- ⚠️ `--subtract` on mask doesn't make sense (mask is yes/no, not additive)
- ❓ Question: Why does mask use `--add` bool instead of Vec<String> like use/keyword?

---

### 8. Read Applet (`em read`)

```
em read [--package PATTERN] [--list] [--limit N] [--delete]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--package` | | Filter by package pattern |
| `--list` | `-l` | List filed packages instead of messages |
| `--limit` | `-n` | Max recent packages (default: 10) |
| `--delete` | | Remove each file after showing |

**Findings:**
- ✅ Matches elogv behavior
- ✅ Good filtering options
- ⚠️ `--limit` default of 10 seems arbitrary — consider making it 0 for "all"

---

### 9. Revdep Applet (`em revdep`)

```
em revdep [-L NAME]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--library` | `-L` | Only consider consumers of library |

**Findings:**
- ✅ Simple and focused
- ✅ Matches revdep-rebuild behavior
- ⚠️ Only one flag — limited functionality compared to revdep-rebuild

---

### 10. News Applet (`em news <subcommand>`)

| Subcommand | Description |
|------------|-------------|
| `count` | Count unread news items |
| `list` | List news items |
| `read` | Read a news item |
| `purge` | Purge read news items |

**Findings:**
- ✅ Matches eselect news behavior
- ✅ Simple and clear
- ⚠️ No `--all` or `--unread-only` filters

---

### 11. Glsa Applet (`em glsa <subcommand>`)

| Subcommand | Description |
|------------|-------------|
| `list` | List all GLSAs |
| `check` | Check for affected GLSAs |
| `fix` | Apply a GLSA fix |

**Check/Fix options:**
- `ids`: Vec<String> — GLSA IDs to operate on

**Findings:**
- ✅ Matches glsa-check behavior
- ⚠️ No `--all` or `--affected` filters
- ⚠️ No `--pretend` support for fix

---

### 12. Log Applet (`em log <subcommand>`)

| Subcommand | Description | Flags |
|------------|-------------|-------|
| `current` | Show currently running merges | None |
| `list` | Show recent merge history | `--limit` |
| `time` | Show merge times | `--atom` |
| `predict` | ETA for live session | None |

**List flags:**
- `--limit`: Max rows (default: 20)

**Time flags:**
- `--atom`: Package atom to filter by

**Findings:**
- ✅ Good activity tracking
- ✅ Matches genlop behavior
- ⚠️ `--limit` default of 20 seems arbitrary
- ⚠️ No `--since` or `--until` time range filters

---

### 13. Grep Applet (`em grep`)

```
em grep <pattern> [path...]
```

**Options:**
- `pattern`: Required, String
- `paths`: Vec<String> (trailing_var_arg)

**Findings:**
- ✅ Simple and focused
- ✅ Matches egreplite behavior
- ⚠️ No regex vs fixed-string mode option
- ⚠️ No case-insensitive option

---

### 14. Search Applet (`em search`)

```
em search [--all] [--desc] [--name-only] [--homepage] [pattern]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--all` | `-a` | List all packages (no pattern required) |
| `--desc` | `-S` | Search descriptions instead of names |
| `--name-only` | `-N` | Show only package name |
| `--homepage` | `-H` | Show homepage instead of description |
| `pattern` | | Pattern to search (required unless --all) |

**Findings:**
- ✅ Good search flexibility
- ✅ Matches emerge --search behavior
- ✅ Resolved (2026-08-09): `--searchdesc` on both `Cli` and `Search` is intentional (emerge-style vs equery-style, see Cross-Cutting Concerns §A) — not a duplication bug.
- Correction: global `-s`/`-S` do **not** dispatch to the `Search` applet — they're a separate code path (`search::run_emerge_style` in `emerge.rs`) taken from the default (no-applet) path, without ever constructing an `Applet::Search`.

---

### 15. Atom Applet (`em atom`)

```
em atom <atom> [atom...]
```

**Findings:**
- ✅ Simple atom parsing/validation
- ⚠️ No explicit subcommands — just parses input atoms
- ❓ Question: What does this actually output? Parse results? Validation?

---

### 16. Select Applet (`em select <module> <action>`)

#### 16.1 Select Modules

| Module | Alias | Description |
|--------|-------|-------------|
| `profile` | `repos` | System/sysroot profile (cross-aware) |
| `repository` | | Manage local repositories (overlays) |
| `compiler` | `gcc` | Active compiler profile (gcc-config) |
| `binutils` | | Active binutils profile |
| `linker` | | Active linker profile |
| `clang` | | Active LLVM/clang slot |
| `pkgconf` | | pkg-config backend and wrappers |
| `mirrors` | `mirror` | Gentoo distfile mirrors |

#### 16.2 Profile Actions

| Action | Description |
|--------|-------------|
| `list` | List available profiles |
| `show` | Show current profile |
| `set` | Set active profile |

#### 16.3 Repository Actions

| Action | Description | Options |
|--------|-------------|---------|
| `list` | List repositories | None |
| `add` | Add repository | `name` (required), `location` (required) |
| `remove` | Remove repository | `name` (required) |
| `create` | Create new local overlay | `name` (required), `location` (optional) |
| `sync` | Sync repository | None |

#### 16.4 Compiler/Binutils/Linker/Clang Actions

| Action | Description | Options |
|--------|-------------|---------|
| `list` | List available compiler/binutils/linker profiles | `--target` (optional) |
| `show` | Show current compiler/binutils/linker profile | `--target` (optional) |
| `set` | Set active compiler/binutils/linker profile | `profile` (required), `--target` (optional) |

#### 16.5 Clang Actions

| Action | Description | Options |
|--------|-------------|---------|
| `list` | List available LLVM/clang slots | None |
| `show` | Show current LLVM/clang slot | None |
| `set` | Set active LLVM/clang slot | `slot` (required) |

#### 16.6 Pkgconf Actions

| Action | Description | Options |
|--------|-------------|---------|
| `list` | List available pkg-config backends | `--target` (optional) |
| `show` | Show current backend | `--target` (optional) |
| `set` | Create/update wrapper | `backend` (required), `--target` (optional) |

#### 16.7 Mirrors Actions (MirrorAction)

| Action | Description | Options |
|--------|-------------|---------|
| `list` | List available mirrors | `--country`, `--region` |
| `show` | Show GENTOO_MIRRORS value | None |
| `set` | Set GENTOO_MIRRORS | `urls` (Vec), `--country`, `--region` |

#### 16.5 Mirrors Actions

| Action | Description |
|--------|-------------|
| `list` | List mirrors |
| `add` | Add mirror |
| `remove` | Remove mirror |
| `select` | Select active mirror |

**Findings:**
- ✅ Comprehensive eselect-like functionality
- ✅ Cross-aware (compiler, binutils, etc. work with crossdev)
- ✅ Good module organization
- ⚠️ `--config-root` not honored — must use explicit `--config-root` on select
- 🔴 **Known Issue**: `em --local DIR select profile set ...` tries to modify host /etc/portage (from root-topology.md)

---

### 17. Active Applet (`em active <subcommand>`)

| Subcommand | Description | Flags |
|------------|-------------|-------|
| `show` | Show registered active context | None |
| `set` | Register --prefix/--local as active | `--reference` |
| `clear` | Clear active context | `--all` |
| `env` | Print shell exports | None |
| `list` | List all registered entries | None |
| `add` | Add new entry | `--name` |
| `remove` | Remove entry | None |

**Set flags:**
- `--reference`: Reference to existing entry (name, index, or path)

**Clear flags:**
- `--all`: Clear all entries

**Add flags:**
- `--name`: Optional name for entry

**Remove options:**
- `reference`: Required — name, index, or path

**Findings:**
- ✅ Excellent prefix/local management
- ✅ Good examples in doc comments
- ⚠️ `--local` flag interaction is tricky (see doc comment: `em --local=` or explicit path)
- ⚠️ `em --local active set` is wrong due to clap parsing — documented in code

---

### 18. Crossdev Applet (`em crossdev [options]`)

```
em [--target TUPLE] crossdev [--init-target] [--setup] [options]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--llvm` | `-L` | Use LLVM/Clang model (rejects glibc) |
| `--init-target` | | Initialize target (write alias repos.conf + sysroot config) |
| `--setup` | | Bootstrap cross toolchain (implies --init-target) |
| `--show-target-cfg` | | Print derived target config and exit |
| `--ex-pkg` | | Build extra package onto cross target |
| `--ex-gdb` | | Build cross gdb (shorthand) |

**Inherited from flattened structs:**
- `depgraph_flags`: DepgraphFlags
- `merge_flags`: MergeFlags
- `activity`: ActivityArgs
- `privilege`: Option<Privilege>

**Findings:**
- ✅ Comprehensive crossdev support
- ✅ `--ex-pkg` matches real crossdev behavior
- ⚠️ `--init-target` and `--setup` relationship could be clearer
- ⚠️ Uses global `--target` flag for tuple
- ❓ Question: Should `--target` be required for crossdev?

---

### 19. Toolchain Applet (`em toolchain`)

```
em [--root DIR] toolchain --setup [options]
```

| Flag | Description |
|------|-------------|
| `--setup` | **Required** — Bootstrap toolchain into --root |

**Inherited from flattened structs:**
- `depgraph_flags`: DepgraphFlags
- `merge_flags`: MergeFlags
- `activity`: ActivityArgs
- `privilege`: Option<Privilege>

**Findings:**
- ✅ Simple — only one action (`--setup`)
- ⚠️ Requires `--root` or other topology flag
- ⚠️ No `--stage1`, `--stage2` etc. — toolchain is separate from stages
- 🔴 **Known Issue**: `--local` bootstrap blocked on missing package.provided

---

### 20. Stages Applet (`em stages [options]`)

```
em [--root DIR] stages --stage1 | --stage3 [options]
```

| Flag | Description |
|------|-------------|
| `--stage1` | Build stage1 (packages.build) |
| `--stage3` | Build stage3 (@system emptytree) |

**Inherited from flattened structs:**
- `depgraph_flags`: DepgraphFlags
- `merge_flags`: MergeFlags
- `activity`: ActivityArgs

**Findings:**
- ✅ Stage1 and stage3 support
- ⚠️ No stage2 or stage4 (stage2 doesn't exist in catalyst model)
- ⚠️ Requires `--root` or other topology flag
- ⚠️ `--stage1` and `--stage3` are mutually exclusive?
- 🔴 **Known Issue**: Stage builds into `/` are rejected

---

### 21. Maint Applet (`em maint <subcommand>`)

| Subcommand | Description |
|------------|-------------|
| `all` | Run all maintenance tasks |
| `binhost` | Generate binary package metadata index |
| `binpkg` | Inspect/verify/prune local binary packages |
| `cleanconfmem` | Discard stale config tracker entries |
| `cleanresume` | Discard saved resume lists |
| `logs` | Clean old Portage build logs |
| `merges` | Scan for and fix failed merges |
| `movebin` | Apply package moves to binary packages |
| `moveinst` | Apply package moves to installed packages |
| `regen-use` | Regenerate profiles/use.local.desc |
| `revisions` | Purge repo revision history |
| `sync` | Sync repositories (same as `em sync`) |
| `world` | Check/fix problems in world file |

**Binpkg subcommands:**
| Action | Description |
|--------|-------------|
| `verify` | Check size/MD5/SHA1 against disk |
| `list` | List indexed binary packages |
| `prune` | Keep only newest BUILD_ID |
| `fingerprint` | Print build-env key |
| `gpg-import` | Import OpenPGP public key |

**Verify flags:**
- `--fix`: Quarantine corrupt, drop missing from index
- `--require-signature`: Reject unsigned containers

**Prune flags:**
- `--dry-run`: Report without deleting

**Fingerprint flags:**
- `--full`: Print full key instead of slug
- `--host`: Fingerprint BROOT config instead of target

**Cleanresume flags:**
- `--fix`: Actually delete saved lists

**Regen-use flags:**
- `--output`: Write to file instead of use.local.desc

**World flags:**
- `--fix`: Remove orphaned entries

**Findings:**
- ✅ Comprehensive maintenance coverage
- ✅ Good parity with emaint
- ⚠️ `sync` appears in both MaintCommand and as top-level applet
- ⚠️ Many subcommands have no short flags
- ❓ Question: Should `em sync` be an alias for `em maint sync`?

---

### 22. Dispatch Applet (`em dispatch`)

No options — safe configuration file updates (dispatch-conf)

**Findings:**
- ✅ Simple
- ⚠️ No documentation of what it does
- ❓ Question: What config files does it handle?

---

### 23. Etc Applet (`em etc`)

No options — interactive configuration file updates (etc-update)

**Findings:**
- ✅ Simple
- ⚠️ No documentation
- ❓ Question: Same as dispatch but interactive?

---

## Cross-Cutting Concerns

### A. Global vs Applet Flags

**Issues Found:**

1. **`--search` and `--searchdesc`** exist on both `Cli` and `Search` applet
   - Global flags do **not** trigger the `Search` applet — they take a
     separate code path (`search::run_emerge_style` in `emerge.rs`'s
     default no-applet handling), mirroring real Portage's `emerge -s` vs
     `equery`
   - `Search` applet uses `search::run` with its own richer flag set
     (`--all`/`--desc`/`--name-only`/`--homepage`)
   - ✅ Resolved (2026-08-09): intentional split, documented inline on
     `Cli::search`/`Cli::searchdesc` in cli.rs — not a bug

2. **`--sync`** exists as both:
   - Top-level `Sync` applet (`em sync`)
   - `MaintCommand::Sync` (`em maint sync`)
   - ✅ Resolved (2026-08-09): confirmed both call `crate::maint::sync::run`
     with identical arguments (`dispatch.rs:121` and `:309`) — genuinely the
     same implementation, `em sync` is a shorthand. Documented inline on
     both variants in cli.rs.

3. **`--pretend` / `-p`** is global but should work with all applets
   - ✅ Works with emerge path
   - ⚠️ Verify it works with all applets that should support it

### B. Short Flag Conflicts

**Issue**: Multiple applets use the same short flag characters.

| Short Flag | Used In | Conflict? |
|-----------|---------|-----------|
| `-a` | MergeFlags (`--ask`) | clap subcommand parsing prevents conflict |
| `-a` | Use applet (`--add`) | ✅ No conflict (different subcommand) |
| `-a` | Pkg applet (`--add`) | ✅ No conflict (different subcommand) |
| `-F` | MergeFlags (`--fetch-all-uri`) | clap subcommand parsing prevents conflict |
| `-F` | Query depgraph (`--format`) | ✅ No conflict (different subcommand) |
| `-t` | MergeFlags (`--tree`) | Currently no other `-t` found |

**Findings:**
- ✅ Clap's subcommand parsing handles conflicts correctly
- ✅ Flags with same short form in different subcommands don't collide
- ⚠️ Could be confusing for users but works correctly
- ⚠️ `-a` is heavily overloaded (MergeFlags::ask, Use::add, Pkg::add)

### C. DepgraphFlags and MergeFlags

These are flattened into many applets. Now reviewed from source files.

#### B.1 DepgraphFlags (`cli/depgraph_flags.rs`, 27 lines)

| Flag | Short | Description |
|------|-------|-------------|
| `--deep` | `-D` | Re-examine transitive deps; with `-u` upgrades installed packages |
| `--newuse` | `-N` | Rebuild when planned USE differs from VDB |
| `--changed-use` | `-U` | Rebuild only when enabled USE flag changed (subset of newuse) |

**Findings:**
- ✅ Found the missing emerge flags: `-D`, `-N`, `-U` are in DepgraphFlags
- ✅ All have short flags matching emerge
- ✅ Flattened into Cli and staged build applets (crossdev, toolchain, stages)
- ✅ Also flattened into Query::Depgraph subcommand with same meanings

#### B.2 MergeFlags (`cli/merge_flags.rs`, 185 lines)

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--ask` | `-a` | | Ask for confirmation before merging |
| `--update` | `-u` | | Update installed packages to newest |
| `--autounmask-write` | | | Write required USE changes to package.use |
| `--oneshot` | `-1` | | Build but don't add to world file |
| `--fetchonly` | `-f` | | Only fetch distfiles |
| `--fetch-all-uri` | `-F` | | Fetch every SRC_URI regardless of USE |
| `--buildpkg` | `-b` | | Build binary packages for all merged |
| `--buildpkgonly` | `-B` | | Build but don't merge |
| `--usepkg` | `-k` | | Use binary packages if available |
| `--usepkgonly` | `-K` | | Only use binary packages |
| `--getbinpkg` | `-g` | | Fetch binary packages |
| `--getbinpkgonly` | `-G` | | Only fetch binary packages |
| `--emptytree` | `-e` | | Treat all as not installed |
| `--tree` | `-t` | | Show dependency tree |
| `--json` | | | Output as JSON |
| `--onlydeps` | `-o` | | Only merge dependencies |
| `--noreplace` | `-n` | | Don't replace already installed same version |
| `--jobs` | `-j` | 1 | Parallel builds (default: sequential) |
| `--load-average` | `-l` | | Max load average for parallel builds |
| `--keep-going` | | | Continue even if some packages fail |
| `--autounmask` | | | Auto add USE flags and unmask entries |
| `--autosolve-use` | | | Let solver choose USE flags for REQUIRED_USE |
| `--eta` | | | Show estimated time of completion |
| `--complete-graph` | | | Pull retained packages into plan during -uD |
| `--with-bdeps` | | | Include BDEPEND in resolution |
| `--root-deps` | | | Only require RDEPEND satisfiable in target |
| `--exclude` | `-X` | | Exclude atom from being merged |

**Findings:**
- ✅ Found `-n` / `--noreplace` in MergeFlags (line 114-115)
- ✅ All standard emerge flags present: `-u`, `-1`, `-f`, `-F`, `-b`, `-B`, `-k`, `-K`, `-g`, `-G`, `-e`, `-t`, `-o`, `-j`, `-l`
- ✅ Additional useful flags: `--json`, `--keep-going`, `--autounmask`, `--autosolve-use`, `--eta`
- ✅ `--complete-graph` for pulling retained packages during -uD
- ✅ `--with-bdeps` and `--root-deps` for cross-compilation support
- ✅ `--exclude` (-X) for excluding packages from plan
- ⚠️ Short flags: `-a` for `--ask` — but `em use` also has `-a` for `--add` (potential conflict resolved by clap's subcommand parsing)
- ⚠️ Short flags: `-F` for `--fetch-all-uri` — Query depgraph also has `-F` for `--format` (potential conflict)
- ⚠️ Short flags: `-t` for `--tree` — might conflict with other uses
- ⚠️ Default for `--jobs` is 1 (sequential) — different from emerge which defaults to 1 but can be configured
- ❓ Question: Should `--ask` be global? Code comment says it was moved OFF global to avoid conflicts

#### B.3 ActivityArgs (`cli/activity.rs`, 55 lines)

| Flag | Long Form | Description |
|------|-----------|-------------|
| | `--activity-fd N` | Write activity events as JSONL to file descriptor N |
| | `--activity-jsonl PATH` | Append activity events as JSONL to PATH |
| | `--emergelog` | Dual-write Portage-compatible emerge.log lines |

**Findings:**
- ✅ Clean separation of activity output concerns
- ✅ Flattened into Cli and staged build applets
- ✅ Not global (only applies to commands that drive activity bus)
- ✅ Code comment explains why not global (avoids meaningless flags on non-activity commands)

**Findings:**
- ✅ All flags now accounted for
- ✅ No missing emerge flags (all found in DepgraphFlags or MergeFlags)
- ⚠️ Potential short flag conflicts between applets (handled by clap's subcommand parsing)
- ⚠️ Some flags have different defaults than emerge (e.g., `--jobs` default is 1)

### C. Privilege Handling

**Current state:**
- Global `--privilege` flag (default: Auto)
- Applet-specific overrides for: `Crossdev`, `Toolchain`, `Stages`
- Override merged via `Cli::effective_privilege()`

**Findings:**
- ✅ Consistent pattern
- ⚠️ Not all staged-build applets have override (Setup doesn't)

---

## Summary: Action Items

### 🔴 High Priority

1. **Fix `--local` bootstrap** (todo/local-bootstrap-provided.md)
   - `em setup --local` needs to write: repos.conf, make.profile, package.provided
   - Currently only writes layout + bashrc + make.conf

2. ~~**Config-root resolution inconsistency**~~ — **STALE, already fixed.**
   `em select` (all modules, via `config_portage_dir_for()`) resolves
   `--config-root` first, else the `--local`/`--prefix` overlay, else host
   `/`. Fixed in `7a8c5bc` (2026-06-23); root-topology.md's "Known gap"
   section described pre-fix behavior and has been corrected
   (2026-08-09). Only a bare `--root` (no `--config-root`/`--local`/
   `--prefix`) still doesn't count, matching real `eselect` on purpose.

3. ~~**`--sync` duplication**~~ — **Resolved (2026-08-09), verified no
   behavioral difference.** Both call `crate::maint::sync::run` with the
   same args; `em sync` is the intentional shorthand, documented inline in
   cli.rs. No code change needed.

### 🟡 Medium Priority

4. ~~**Global flag conflicts**~~ — **Resolved (2026-08-09).** `--search`/
   `--searchdesc` on `Cli` and `Search`'s own `--desc` are separate,
   intentional code paths (emerge-style vs equery-style, see §A above), not
   a precedence conflict — nothing dispatches through both. Documented
   inline in cli.rs.

5. **Missing emerge short flags**
   - `-n` / `--noreplace`
   - `-N` / `--newuse`
   - `-U` / `--changed-use`
   - Find where these are defined and verify parity

6. **DepgraphFlags / MergeFlags review**
   - Review all flags in these structs
   - Check for conflicts with global flags
   - Document all options

7. **Applet option consistency**
   - Some applets have `--pretend`, some don't
   - Some support `--jobs`, some don't
   - Audit which should support which

### 🟢 Low Priority / Polish

8. **Short flag standardization**
   - Many applets lack short flags
   - Consider adding consistent short flags for common actions

9. **Default values**
   - Some `--limit` defaults are arbitrary (20 for log list)
   - Consider making 0 mean "all" consistently

10. ~~**Type consistency**~~ — **Fixed (2026-08-09).** `quickpkg
    --include-config`/`--include-unmodified-config` are now plain bool
    flags, not `String` "y|n".

11. **Documentation gaps**
    - `em dispatch` — what does it do?
    - `em etc` — how is it different from dispatch?
    - `em atom` — what output format?

### 📋 Questions for Maintainer

1. Still open: should `em --sync` (global flag) be supported or only `em sync` (applet)?
2. Still open: should `--search`/`--searchdesc` be only on the `Search` applet, not global? (Current split mirrors `emerge -s` vs `equery` on purpose — see §A — but whether that's the right UX for `em` specifically is still a call for the maintainer.)
3. Answered below (§ Resolved) — `-u`/`-D`/`-1`/`-n`/`-N`/`-U` are in `MergeFlags`/`DepgraphFlags`.
4. Answered (2026-08-09): No — `em depclean` and `em -c` are the identical implementation, verified in code.
5. Answered (2026-08-09): Functionally, yes — both already call the same `maint::sync::run`. No code change needed; neither is literally a clap `alias` of the other, but behavior is identical.
6. Answered (2026-08-09): No good reason found — fixed to `bool`.
7. Still open: should all staged-build applets (`crossdev`, `toolchain`, `stages`) support `--pretend`?
8. Still open: should `--local` without arguments default to `~/.gentoo` or require explicit path?

---

## Appendices

### A. Complete Command Tree

```
em [global-flags] [atoms...]
em [global-flags] <applet> [applet-flags] [applet-args...]

Global flags:
  -p/--pretend, -v/--verbose, -q/--quiet, --arch, --repo, --prefix, --local, 
  --privilege, -s/--search, -S/--searchdesc, -O/--nodeps, -C/--unmerge, 
  -c/--depclean, -P/--prune, -W/--deselect, -r/--resume, --root, --config-root, 
  --vdb, -T/--target, --color, --activity-*, --depgraph-*, --merge-*

Applets:
  ebuild <path> <phase>...
  sync [repo...]
  depclean [atom...]
  portageq <command> [args...]
  regen [repo...] [-o DIR] [--repos-dir DIR] [-j N] [--dedup]
  quickpkg <atom>... [--include-config] [--include-unmodified-config]
  mirrordist [--repo REPO] --distfiles DIR [--repos-dir DIR] [-j N] [--delete] 
    [--deletion-delay DUR] [--deletion-db FILE] [--success-log FILE] 
    [--failure-log FILE] [--scheduled-deletion-log FILE] [--whitelist-from FILE]...
    [--verify-existing-digest] [--gentoo-mirrors-fallback] [--delete-allow-incomplete]
  setup [--prefix DIR | --local [DIR] | --root DIR]
  query <subcommand> [subcommand-flags] [args...]
  clean <dist|pkg>
  use [-a FLAG | -r FLAG] [--make-conf PATH]
  pkg <use|keyword|mask|env> [subcommand-flags] <atom> [flags...]
  revdep [-L NAME]
  read [--package PATTERN] [-l/--list] [-n/--limit N] [--delete]
  news <count|list|read|purge>
  glsa <list|check|fix> [ids...]
  log <current|list|time|predict> [subcommand-flags]
  grep <pattern> [path...]
  search [--all] [-S/--desc] [-N/--name-only] [-H/--homepage] [pattern]
  atom <atom>...
  select <module> <action> [action-flags]
  active <show|set|clear|env|list|add|remove> [subcommand-flags]
  crossdev [--init-target] [--setup] [--show-target-cfg] [--ex-pkg PKG]... [--ex-gdb] [flattened-flags]
  toolchain --setup [flattened-flags]
  stages [--stage1 | --stage3] [flattened-flags]
  maint <subcommand> [subcommand-flags]
  dispatch
  etc
```

### B. Comparison with Portage

| Portage Command | `em` Equivalent | Status |
|----------------|-----------------|--------|
| `emerge` | `em` (default) | ✅ |
| `equery` | `em query` | ✅ Partial |
| `euse` | `em use` | ✅ Partial |
| `emaint` | `em maint` | ✅ Partial |
| `dispatch-conf` | `em dispatch` | ✅ |
| `etc-update` | `em etc` | ✅ |
| `eclean` | `em clean` | ✅ Partial |
| `qlist` | `em query list` | ✅ |
| `qfile` | `em query belongs` | ✅ |
| `genlop` | `em log` | ✅ |
| `qlop` | ? | ❌ Missing |
| `emirrordist` | `em mirrordist` | ✅ |
| `quickpkg` | `em quickpkg` | ✅ |
| `revdep-rebuild` | `em revdep` | ✅ |
| `eselect news` | `em news` | ✅ |
| `glsa-check` | `em glsa` | ✅ Partial |
| `portageq` | `em portageq` | ✅ |
| `egreplite` | `em grep` | ✅ |
| `elogv` | `em read` | ✅ |
| `crossdev` | `em crossdev` | ✅ |
| `gcc-config` | `em select compiler` | ✅ |
| `binutils-config` | `em select binutils` | ✅ |

### C. File Locations

- **Main CLI**: `portage-cli/src/cli.rs`
- **Depgraph flags**: `portage-cli/src/depgraph_flags.rs`
- **Merge flags**: `portage-cli/src/merge_flags.rs`
- **Activity args**: `portage-cli/src/activity.rs`
- **Dispatch**: `portage-cli/src/dispatch.rs`

---

## Metadata

- **Created**: 2026-08-09
- **Last updated**: 2026-08-09
- **Source commit**: 53a0761
- **Status**: DepgraphFlags, MergeFlags, ActivityArgs reviewed ✅
- **Next review**: Select subcommands depth review, short flag conflict analysis

### 📋 Questions for Maintainer - UPDATE

#### Resolved ✅
- **Where are `-u`, `-D`, `-1`, `-n`, `-N`, `-U` flags?** → Found in MergeFlags (`-u`, `-1`, `-n`) and DepgraphFlags (`-D`, `-N`, `-U`)

#### New Questions (from MergeFlags review) ❓

9. Short flag `-a` is used for both `--ask` (MergeFlags) and `--add` (Use applet) — how is conflict resolved?
10. Short flag `-F` is used for both `--fetch-all-uri` (MergeFlags) and `--format` (Query depgraph) — how is conflict resolved?
11. Short flag `-t` is used for `--tree` (MergeFlags) — any conflicts?
12. Should `--ask` be global? Code comment says it was intentionally moved OFF global to avoid conflicts with `em use -a`
13. `--jobs` default is 1 (sequential) — should this match emerge's behavior more closely?
14. MergeFlags has `--update` (-u) and DepgraphFlags has `--deep` (-D) — is the -uD interaction correct?

