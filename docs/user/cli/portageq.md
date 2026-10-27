<!-- @generated from em's command line; do not edit -->
# em portageq

`em portageq` — a subset of Portage's own query interface (byte-compatible where implemented: the installed-package queries and `envvar`)

**Effect:** read-only.

## Usage

```
em portageq [OPTIONS] <COMMAND>
```

## Commands

* [`has_version`](portageq/has_version.md) — Exit 0 if ATOM is installed under EROOT, 1 otherwise
* [`best_version`](portageq/best_version.md) — Print the highest installed version matching ATOM under EROOT
* [`match`](portageq/match.md) — List every installed CPV matching ATOM under EROOT, one per line
* [`envvar`](portageq/envvar.md) — Print configuration variables as a build would see them
* [`vdb_path`](portageq/vdb_path.md) — Print the installed-package database path
* [`pkgdir`](portageq/pkgdir.md) — Print the binary-package directory
* [`distdir`](portageq/distdir.md) — Print the distfiles directory
* [`gentoo_mirrors`](portageq/gentoo_mirrors.md) — Print GENTOO\_MIRRORS
* [`config_protect`](portageq/config_protect.md) — Print CONFIG\_PROTECT
* [`config_protect_mask`](portageq/config_protect_mask.md) — Print CONFIG\_PROTECT\_MASK
* [`portdir`](portageq/portdir.md) — Print the main repository path (deprecated)
* [`portdir_overlay`](portageq/portdir_overlay.md) — Print overlay paths (deprecated)
* [`get_repos`](portageq/get_repos.md) — List repository names, overlays first
* [`get_repo_path`](portageq/get_repo_path.md) — Print each repository's location
* [`mass_best_version`](portageq/mass_best_version.md) — Print `atom:cpv` for each of several atoms' best installed version

## Options

* `-h, --help` — Print help (see a summary with '-h')

## Global options

* `--color <WHEN>` — \[default: auto\]
  * `auto`: Colour when stdout is a terminal.
  * `always`: Always colour.
  * `never`: Never colour.

## Exit status

* `0` — query succeeded
* `1` — no match / unset variable
* `2` — invalid atom, or no variable named
* `64` — \<EROOT\> is not a directory

## Examples

```
$ em portageq has_version / sys-apps/portage
```
