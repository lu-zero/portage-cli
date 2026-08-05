# portage-repo

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Build Status](https://github.com/lu-zero/portage-repo/workflows/CI/badge.svg)](https://github.com/lu-zero/portage-repo/actions?query=workflow:CI)
<!-- Historical crates.io publish; workspace crate is `publish = false` until brush is on crates.io. -->
[![dependency status](https://deps.rs/repo/github/lu-zero/portage-repo/status.svg)](https://deps.rs/repo/github/lu-zero/portage-repo)
[![docs.rs](https://docs.rs/portage-repo/badge.svg)](https://docs.rs/portage-repo)

A Rust library for reading Gentoo ebuild repository layouts, based on the [Package Manager Specification (PMS)](https://projects.gentoo.org/pms/9/pms.html).

> **Warning**: This codebase was largely AI-generated (slop-coded) and has not
> yet been thoroughly audited. It may contain bugs, incomplete PMS coverage, or
> surprising edge-case behaviour. Use at your own risk and please report issues.

## Overview

`portage-repo` provides types and readers for Gentoo ebuild repository
structure: `metadata/layout.conf`, category and package directory enumeration,
profiles, and metadata cache access. It builds on
[portage-atom](https://crates.io/crates/portage-atom) for atom parsing and
[portage-metadata](https://crates.io/crates/portage-metadata) for cache entry types.

## Features

- Read `metadata/layout.conf` (masters, cache-formats, profile-formats)
- Enumerate categories, packages, and ebuild versions
- Read metadata cache entries (`metadata/md5-cache/`)
- Profile directory reading (PMS Chapter 5)
- Repository discovery and validation

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
portage-repo = "0.1"
```

## PMS Compliance

This library implements:

- **Tree Layout** (PMS Chapter 4) — repository directory structure
- **Profiles** (PMS Chapter 5) — profile directory and inheritance
- **Metadata Cache** (PMS Chapter 14) — via [portage-metadata](https://crates.io/crates/portage-metadata)

## Related Projects

- [portage-atom](https://crates.io/crates/portage-atom) — Portage package atom parser
- [portage-metadata](https://crates.io/crates/portage-metadata) — Ebuild metadata cache types and parser
- [pkgcraft](https://crates.io/crates/pkgcraft) — Full-featured Gentoo package manager library
- [PMS](https://projects.gentoo.org/pms/) — Package Manager Specification
- [Portage](https://wiki.gentoo.org/wiki/Portage) — Reference Gentoo package manager

## License

[MIT](LICENSE-MIT)

## Contributing

See [AGENTS.md](AGENTS.md) for project conventions and contribution guidelines.

## Author

Luca Barbato <lu_zero@gentoo.org>
