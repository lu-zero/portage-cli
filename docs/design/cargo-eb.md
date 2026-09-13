# cargo-eb

Standalone workspace crate. Binary name `cargo-eb` so Cargo's applet lookup
makes `cargo eb` work. Not an `em` applet. CLI is usage-rs, same as `em`.
MIT; no GPL source.

## What it produces

Two artifacts `cargo.eclass` already knows how to consume:

1. A vendor tarball whose members unpack as `cargo_home/gentoo/…`
   (`ECARGO_VENDOR=${WORKDIR}/cargo_home/gentoo`).
2. An ebuild that puts that tarball in `SRC_URI` (and the package sources),
   with `LICENSE` / `DESCRIPTION` / `HOMEPAGE` / Cargo-feature `IUSE` filled
   from the crate.

`--update PATH.ebuild` rewrites those generated bits in an existing ebuild
and leaves the rest of the file alone.

The old default (`CRATES=` + `GIT_CRATES=` + one URI per crate) is the
fallback when a tarball is not wanted. The tarball path is the one this
tool is for: one distfile, git deps included, no thousand-line `CRATES=`.

## Input

`cargo eb [PATH]` (`PATH` defaults to `.`):

- `Cargo.lock` present → use it.
- only `Cargo.toml` → `cargo generate-lockfile` in that tree, then use the
  lock. No home-grown resolver.

`CARGO` from the environment (Cargo sets this when invoking an applet).

A workspace member does not inherit the root `Cargo.lock`. `cargo eb` builds
an isolated workspace of that package's path-dep closure so the vendor
tarball is not the whole repo (em, benches, …).

## CLI

```
cargo eb [PATH]
    --update FILE.ebuild     rewrite generated CRATES/GIT_CRATES/SRC_URI/LICENSE/IUSE
    -o, --output FILE        new ebuild (default {name}-{version}.ebuild)
    --tarball FILE           vendor tarball (default {name}-{version}-crates.tar.xz)
    --no-tarball             CRATES=/GIT_CRATES= instead
    -d, --distdir DIR
    -l, --license-mapping FILE
    -f, --force
```

`--update` and `-o` together: read `--update`, write `-o`.

## IUSE from Cargo features

`cargo_toml` already gives `[features]` plus which names sit in `default`.
Skip the synthetic `default` group itself. Remaining names become IUSE:
`+foo` if it is a default feature, `foo` otherwise. `src_configure` maps
each to `$(usev foo)` for `cargo_src_configure`.

Same split as `LICENSE=` / `LICENSE+=`:

```
IUSE="debug"                 # maintainer; never rewritten
# Cargo features
IUSE+=" +serde json"

src_configure() {
	local myfeatures=(
		$(usev serde)
		$(usev json)
	)
	cargo_src_configure
}
```

`--update` replaces only the `# Cargo features` / `IUSE+=` line and the
`myfeatures=(...)` body. A missing marker is an error on update (do not
guess which `IUSE=` is ours). Fresh generate always writes the marker.

## Keep from the current crate

- SPDX → Gentoo `LICENSE=` with nested AND/OR grouping, then
  `portage_metadata::LicenseExpr` to validate the combined expression.
- `cargo vendor --versioned-dirs` for the tarball (git deps included).
- `package_directory_in_archive` when emitting `GIT_CRATES` in `--no-tarball`
  mode.
- DISTDIR / license-mapping defaults via `MakeConf` / `ReposConf`.
- Empty `DESCRIPTION` fallback (`"{name} Rust crate"`).
- Independently structured `templates/ebuild.j2` (not pycargoebuild's GPL
  template).
- Cargo `[features]` → IUSE (`+` for defaults), already parsed.

## Drop

- Binary names `pycargoebuild-rs` and `cargo-ebuild` (`cargo-ebuild` is
  already a crates.io crate).
- Fetching every crate into DISTDIR when the tarball is the product
  (`cargo vendor` already fetches).
- Flag soup copied from pycargoebuild (`-c`, `--no-write-crate-tarball`,
  `--crate-tarball-prefix`, `-e`, `-M`) unless a second caller needs it.

## Not this round

`pycargoebuild.toml` license-overrides, umask, `pkgdev manifest`.
