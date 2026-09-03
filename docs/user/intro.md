# Introduction to `em`

`em` is a **Rust reimplementation** of the Gentoo Portage package manager, designed to be
faster, more reliable, and easier to maintain while preserving the behavior and
conventions of the traditional `emerge`, `equery`, `euse`, and related tools.

---

## What is `em`?

`em` is the **unified command-line front-end** for Gentoo's package management system. It replaces the traditional collection of separate tools:

| Traditional Tool | `em` Equivalent | Purpose |
|------------------|-----------------|---------|
| `emerge` | `em` (default) | Install, upgrade, and manage packages |
| `equery` | `em query` | Query the package database |
| `euse` | `em use` | Manage USE flags |
| `emaint` | `em maint` | Repository maintenance |
| `dispatch-conf` | `em dispatch` | Configuration file updates |
| `eclean` | `em clean` | Clean distfiles and packages |
| `qlist`/`qfile` | `em query list` / `em query belongs` | List installed packages / find file owners |
| `genlop` | `em log` | View merge history and activity |

`em` is built on a family of **purpose-built Rust crates** that handle parsing of package
atoms, metadata, repository layouts, and the installed package database. The project is
pre-release (development source from git) and is being actively developed and tested
against real Gentoo systems.

> **Note**: For a more mature Rust-based alternative, see [Pkgcraft](https://pkgcraft.github.io/).

---

## Quick Start

### Installation

```bash
# From a Git checkout of this repository:
cargo install --path portage-cli
```

Not published to crates.io, and not expected to be: `portage-cli` sets
`publish = false` because it depends (via `portage-repo`/`portage-distfiles`)
on a forked `brush` pulled straight from git.

This installs the `em` binary to your `~/.cargo/bin` directory. Ensure this directory is
on your `PATH`.

### Basic Usage

Documented form is `em [applet] [options] [args]`. True globals (`-p`/`-v`/`-q`,
`--arch`, `--repo`, `--color`) and Topology (`--prefix`/`--local`/`--config-root`/
`--vdb`/`--target`) may also appear before a named applet
(`em --prefix P toolchain`). Prefix emerge-mixins before a non-merge applet
(`em -a search`, `em -uD query …`) are rejected.

```bash
# Search for a package
em -s firefox
em --search firefox

# Install a package (with dependencies)
em firefox

# Pretend (dry-run) to see what would be installed
em -p firefox
em --pretend firefox

# Upgrade all installed packages
em -uD @world
em --update --deep @world

# Query installed packages
em query list -I

# Find which package owns a file
em query belongs /usr/bin/python

# View USE flags
em use

# Enable a USE flag
em use -a png
em use --add png
```

---

## How `em` Works

### The Resolution Pipeline

When you run `em <package>`, the following happens:

1. **Atom Parsing**: Package names (atoms) are parsed into structured form
   (e.g., `>=dev-lang/python-3.10`)
2. **Repository Loading**: The ebuild repository (typically `::gentoo`) is loaded
   from `repos.conf` or `/var/db/repos/gentoo`
3. **Profile Resolution**: Your system profile is resolved from `make.profile`,
   incorporating overrides from `make.conf` and environment variables
4. **Dependency Resolution**: The solver computes the full set of packages needed
   to satisfy your request, respecting USE flags, masks, and keywords
5. **Merge Planning**: Packages are ordered for installation, respecting
   dependency constraints and build order
6. **Build Execution**: Each package is fetched (if needed), unpacked, configured,
   compiled, and installed (merged) into your system

### Key Features

- **Fast Dependency Resolution**: Uses the [PubGrub](https://github.com/pubgrub-rs/pubgrub)
  algorithm for efficient dependency solving
- **Parallel Builds**: Supports `-j`/`--jobs` for parallel package building
- **Accurate Change Detection**: Only rebuilds packages when truly needed
- **USE Flag Stacking**: Correctly handles incremental USE flag stacking from
  profiles and `make.conf`
- **Slot Handling**: Properly manages package slots and sub-slots
- **Preserve Libs**: Tracks shared libraries to avoid breaking dependencies

---

## Topologies: Where Does `em` Install Packages?

`em` supports multiple **root topologies**, which determine where packages are
installed and how dependencies are resolved. This is one of `em`'s most powerful
features, enabling both system management and unprivileged builds.

### 1. Host System (Default)

The default mode installs packages directly to your running system:

```bash
# Install to the host system (default)
em firefox

# All roots point to /:
# - config root: /etc/portage
# - base root: / (VDB at /var/db/pkg)
# - target root: / (install destination)
# - BROOT: / (build tools run on host)
```

This is equivalent to running `emerge` as root on a Gentoo system.

### 2. Offset Root (`--root`)

Install to a separate directory tree, useful for building stage tarballs or
chroots:

```bash
# Create a minimal stage1 into /var/tmp/stage1
em --root /var/tmp/stage1 toolchain --setup
em --root /var/tmp/stage1 stages --stage1

# All packages install under /var/tmp/stage1
# - config root: / (still reads the host's profile/make.conf — matches real
#   portage's ROOT=; pass --config-root separately to read from the offset)
# - base root: /var/tmp/stage1
# - target root: /var/tmp/stage1
# - BROOT: / (build tools from host)
```

### 3. Prefix Overlay (`--prefix`)

Install packages as an **overlay** on top of your host system, useful for
unprivileged installs that borrow the host's libraries:

```bash
# Create a prefix at ~/.gentoo-overlay
em setup --prefix ~/.gentoo-overlay
em --prefix ~/.gentoo-overlay firefox

# Packages install under ~/.gentoo-overlay
# - config root: / (host profile)
# - base root: / (host VDB counts as installed)
# - target root: ~/.gentoo-overlay
# - BROOT: / (build tools from host)
# - EPREFIX: ~/.gentoo-overlay (scripts get relocatable paths)
```

This is useful on a Gentoo host when you want to install a few extra packages
without affecting the system.

### 4. Standalone Prefix (`--local`)

Create a **completely self-contained** Gentoo prefix that owns its own VDB,
config, and toolchain. This is the most powerful topology for unprivileged use:

```bash
# Bootstrap a standalone prefix
em setup --local ~/.gentoo
em --config-root ~/.gentoo select profile set default/linux/amd64/23.0/no-multilib
# TODO: package.provided seed (not yet automated)
em --local ~/.gentoo toolchain --setup
em --local ~/.gentoo firefox

# Everything lives under ~/.gentoo
# - config root: ~/.gentoo/etc/portage
# - base root: ~/.gentoo
# - target root: ~/.gentoo
# - BROOT: ~/.gentoo (build tools from prefix itself)
# - EPREFIX: ~/.gentoo
```

This mode is ideal for:
- Running Gentoo on non-Gentoo systems (Ubuntu, Debian, macOS, etc.)
- Creating isolated development environments
- Cross-compilation toolchains (as a prerequisite)

> **Note**: The `--local` bootstrap is still being polished. See
> [`docs/user/stages-and-testing.md`](./stages-and-testing.md) for current
> limitations and the manual bootstrap procedure.

---

## Common Workflows

### Package Management

```bash
# Install a package
em <package>

# Install multiple packages
em <package1> <package2> <package3>

# Reinstall a package (naming it explicitly reinstalls even if the same
# version is already installed; -n/--noreplace is what skips that)
em <package>

# Build and install without recording it in the world file
em -1 <package>
em --oneshot <package>

# Uninstall a package
em -C <package>
em --unmerge <package>

# Clean packages not in @world
em -c
em --depclean
```

### Querying

```bash
# Search for packages
em -s <pattern>
em --search <pattern>

# Search descriptions too
em -S <pattern>
em --searchdesc <pattern>

# List installed packages
em query list -I

# Show package metadata
em query meta <package>

# Show which package owns a file
em query belongs /path/to/file

# Show reverse dependencies
em query depends <package>
```

### USE Flags

```bash
# Show current USE flags
em use

# Add a USE flag
em use -a <flag>
em use --add <flag>

# Remove a USE flag
em use -r <flag>
em use --remove <flag>

# Temporarily enable USE flags for a single merge
em --use <flag> <package>
```

### Repository Management

```bash
# Sync repositories (update ebuilds)
em sync

# Add a new repository overlay
# Edit /etc/portage/repos.conf (or repos.conf under an offset's config_root)
# Then sync:
em sync
```

### Bootstrap and Setup

```bash
# Bootstrap a prefix layout for --local or --prefix
em setup --local ~/.gentoo
em setup --prefix ~/.gentoo-overlay

# Bootstrap a toolchain into an offset root
em --root /var/tmp/stage1 toolchain --setup

# Build a stage1 (packages.build)
em --root /var/tmp/stage1 stages --stage1

# Build a stage3 (@system)
em --root /var/tmp/stage3 stages --stage3
```

---

## Configuration

`em` reads configuration from several sources, in this order of precedence:

1. **Command-line flags** (highest priority) — including the few that also
   read env vars (`ROOT`, `EM_PRIVILEGE`, `EM_EMERGELOG`)
2. **`/etc/portage/make.conf`** (system-wide; legacy `/etc/make.conf` also
   read) — resolved from `config_root`, which defaults to `--config-root || /`.
   Neither `--root` nor `--prefix` moves it (matches real portage's `ROOT=`,
   which never touches `PORTAGE_CONFIGROOT`); `--local` is the one exception,
   preferring an already-bootstrapped prefix's own profile over the host
3. **Profile defaults** (from `make.profile` and parent profiles)

There's no per-user `~/.config/portage/make.conf` — the only recognized
`make.conf` lives under `config_root`. See
[`root-model.md`](./root-model.md) for the full config/base/target-root
breakdown, including the `--local` exception.

### Key Configuration Files

| File | Purpose |
|------|---------|
| `make.conf` | USE flags, CFLAGS, FEATURES, and other settings |
| `repos.conf` | Repository locations and sync settings |
| `make.profile` | Active profile (symlink to actual profile directory) |
| `package.use` | Per-package USE flag overrides |
| `package.keywords` | Per-package keyword overrides |
| `package.mask` | Per-package masks |
| `package.provided` | Packages provided by the system (for bootstrapping) |

### Topology Configuration

Use these flags to control where `em` operates:

| Flag | Purpose |
|------|---------|
| `--root DIR` | Install packages under DIR (self-contained offset) |
| `--prefix DIR` | Install packages as overlay under DIR (shares host) |
| `--local [DIR]` | Standalone prefix at DIR (or `~/.gentoo` if DIR not given) |
| `--config-root DIR` | Read config from DIR instead of target root |
| `--target TUPLE` | Cross-compile for target TUPLE |

---

## Cross-Compilation

`em` supports cross-compilation through the `crossdev` applet:

```bash
# Set up a cross-compilation target
em --target riscv64-unknown-linux-gnu crossdev --init-target
em --target riscv64-unknown-linux-gnu crossdev --setup

# Build packages for the target
em --target riscv64-unknown-linux-gnu <package>
```

For a complete guide to cross-compilation, see [`docs/user/crossdev.md`](./crossdev.md).

---

## Comparison with `emerge`

`em` aims for **behavioral parity** with `emerge`, with these differences:

| Feature | `em` | `emerge` |
|---------|------|----------|
| Resolution speed | Faster (Rust + PubGrub) | Good |
| Memory usage | Lower | Higher |
| Parallel builds | Supported (`-j`) | Supported |
| Dependency resolution | PubGrub algorithm | Portage depgraph |
| Python requirement | None (native Rust) | Required |
| Ebuild compatibility | High | Native |
| Host platform | Linux, partial macOS (`pseudoroot` privilege backend only) | Linux-focused |

### Known Gaps

`em` is still pre-release. Some features are not yet complete:

- Some applets are stubs (see applet status in README)
- `--local` bootstrap is not fully automated
- Certain edge cases in dependency resolution
- Some Portage-specific features (e.g., `FEATURES` parity)

See [`docs/user/applets.md`](./applets.md) for detailed status of each applet.

---

## Getting Help

### Reporting Issues

1. Check if your issue is already known in the [pending work](https://github.com/lu-zero/portage-cli/blob/master/todo/PENDING.md)
2. Search existing issues on GitHub
3. File a new issue with:
   - Your command and its output
   - Your system information (`em --version`, OS, architecture)
   - Your configuration (relevant parts of `make.conf`, profile, etc.)

### Debugging

```bash
# Increase verbosity
em -v <package>    # Show build phases
em -vv <package>   # Show debug info
em -vvv <package>  # Show trace info

# Pretend mode (dry-run)
em -p <package>

# Show the dependency tree
em query depgraph <package>
```

---

## Architecture Overview

For those interested in the internals:

- **`portage-cli`**: The `em` binary crate (this repository)
- **`portage-atom`**: Package atom parsing (Cpn, Cpv, Dep, etc.)
- **`portage-metadata`**: Metadata cache parsing and USE flag handling
- **`portage-repo`**: Repository layout, profile stack, ebuild execution
- **`portage-solver`**: Solver-agnostic dependency resolution trait
- **`portage-atom-pubgrub`**: PubGrub solver implementation
- **`portage-resolve`**: Resolution policy, USE stacking, post-solve validation
- **`portage-vdb`**: Installed package database access
- And more...

For details, see [`docs/design/architecture.md`](../design/architecture.md).

---

## Next Steps

Once you're familiar with the basics:

1. Read [`root-model.md`](./root-model.md) to understand the root topology system
2. Explore [`applets.md`](./applets.md) for detailed applet documentation
3. Try [`stages-and-testing.md`](./stages-and-testing.md) for stage building
4. For cross-compilation: [`crossdev.md`](./crossdev.md)
5. For binary packages: [`binhost.md`](./binhost.md)
