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

`--no-tarball` emits `CRATES=` + `GIT_CRATES=` (before `inherit cargo`, which
reads them) and one URI per crate instead. The tarball is the default: one
distfile, git deps included, no thousand-line `CRATES=`.

## Input

`cargo eb [PATH]` (`PATH` defaults to `.`). Package metadata comes from
`cargo metadata`, so `field.workspace = true` inheritance is resolved; that
call also writes `Cargo.lock` if there is none. `CARGO` from the environment
(Cargo sets this when invoking an applet).

A workspace member is built as an isolated workspace of its path-dep closure
so the vendor tarball is not the whole repo (em, benches, …):

- members are symlinked at their original relative paths, so `path =` deps
  and nested layouts still resolve;
- the upstream `Cargo.lock` is copied in and trimmed, not re-resolved;
- relative `[patch]` paths are made absolute, and patches the lock lists as
  unused are dropped (an offline build would still try to fetch them);
- cargo runs from the real workspace root, so its `.cargo/config.toml`
  applies.

## Versions

The ebuild uses `${P}` = crate name + the semver version mapped to PMS:
`-alpha.N`/`-beta.N`/`-rc.N` → `_alphaN`/`_betaN`/`_rcN`, any other
pre-release tag → `_pre[N]`, build metadata dropped. The result is validated
as a Gentoo CPV.

## Outputs

```
cargo eb [PATH]
    --update FILE.ebuild     rewrite generated CRATES/GIT_CRATES/SRC_URI/S/LICENSE/IUSE
    -o, --output FILE        new ebuild (default ${P}.ebuild)
    --tarball FILE           vendor tarball (default DISTDIR/${P}-crates.tar.xz)
    --no-tarball             CRATES=/GIT_CRATES= instead
    --snapshot               pack DISTDIR/${P}.tar.xz of the package
    --no-snapshot            never pack a source tarball
    -d, --distdir DIR
    -l, --license-mapping FILE
    -f, --force
```

`--update` and `-o` together: read `--update`, write `-o`. Distfiles land in
DISTDIR so `ebuild … manifest` finds them. Both tarballs are reproducible
(sorted entries, fixed metadata).

A source snapshot is packed automatically when the crate is not a release
(`publish = false` or a semver pre-release). For a workspace member it is the
isolated tree, and the ebuild sets `S="${WORKDIR}/${P}/<member>"` so
`cargo_src_install` installs the member rather than the virtual root.

## Git and alternate-registry dependencies

`cargo vendor` puts them in the same directory as crates.io ones, but
`cargo_gen_config` only redirects `crates-io`. The tarball therefore also
carries `cargo_home/sources.toml`, the non-crates.io `[source.*]` entries
`cargo vendor` printed with `replace-with = "gentoo"`, and the ebuild appends
it to `${ECARGO_HOME}/config.toml` after `cargo_src_unpack`. It must go into
`CARGO_HOME`: `cargo install` ignores project-level config.

## IUSE from Cargo features

Every feature with a valid USE flag name becomes a flag, `+foo` if it is in
`default`. `src_configure` passes each through `$(usev foo)` together with
`--no-default-features`, so disabling a default flag actually disables it.
`default` entries that are not flags (`dep/feat`) are passed unconditionally.
A `dep:` entry in `default` cannot be expressed with `--features`, so then
default features stay on.

```
IUSE="debug"                 # maintainer; never rewritten
# Cargo features
IUSE+=" json +serde"

src_configure() {
	local myfeatures=(
		$(usev json)
		$(usev serde)
	)
	cargo_src_configure --no-default-features
}
```

`--update` replaces only the `# Cargo features` / `IUSE+=` line, the
`myfeatures=(...)` body and the `cargo_src_configure` call. A missing marker
is an error on update (do not guess which `IUSE=` is ours).

## Licenses

SPDX (parsed leniently, as crates.io accepts `GPL-3.0+`) → Gentoo via the
main repository's `metadata/license-mapping.conf`, keeping AND/OR grouping,
validated with `portage_metadata::LicenseExpr`. An unmapped license is an
error, not a pass-through. A dependency with only `license-file` is reported
for manual review.

## Not this round

`pycargoebuild.toml` license-overrides, umask, `pkgdev manifest`, a
`SRC_URI` for released crates (the maintainer adds the upstream source).
