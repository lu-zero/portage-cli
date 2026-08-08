# portage-metadata

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Build Status](https://github.com/lu-zero/portage-metadata/workflows/CI/badge.svg)](https://github.com/lu-zero/portage-metadata/actions?query=workflow:CI)
[![codecov](https://codecov.io/gh/lu-zero/portage-metadata/graph/badge.svg?token=2MECWX0IA7)](https://codecov.io/gh/lu-zero/portage-metadata)
[![Crates.io](https://img.shields.io/crates/v/portage-metadata.svg)](https://crates.io/crates/portage-metadata)
[![dependency status](https://deps.rs/repo/github/lu-zero/portage-metadata/status.svg)](https://deps.rs/repo/github/lu-zero/portage-metadata)
[![docs.rs](https://docs.rs/portage-metadata/badge.svg)](https://docs.rs/portage-metadata)

A Rust library for reading and writing Gentoo ebuild metadata cache files, based on the [Package Manager Specification (PMS) 9](https://projects.gentoo.org/pms/9/pms.html).

> **Warning**: This codebase was largely AI-generated (slop-coded) and has not
> yet been thoroughly audited. It may contain bugs, incomplete PMS coverage, or
> surprising edge-case behaviour. Use at your own risk and please report issues.

## Overview

`portage-metadata` provides types for representing ebuild metadata and a parser
for the `md5-cache` format used by Gentoo repositories. Ebuild files are bash
scripts that require a full shell interpreter to evaluate, but the metadata
cache (`metadata/md5-cache/`) stores pre-computed metadata in a simple
`KEY=VALUE` format — this is what tools actually consume day-to-day.

## Features

- Parse and serialize `md5-cache` metadata files (PMS 14.3)
- Full metadata types: EAPI, keywords, IUSE, SRC_URI, LICENSE, REQUIRED_USE, phases, etc.
- Dependency parsing via [portage-atom](https://crates.io/crates/portage-atom)
- [winnow](https://crates.io/crates/winnow) 1.0 parser combinators for expression types

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
portage-metadata = "0.10"
```

## Usage

### Parse a Cache Entry

```rust
use portage_metadata::CacheEntry;

let input = "\
EAPI=7
DESCRIPTION=Python bindings for sys-devel/clang
SLOT=0
KEYWORDS=~amd64 ~x86
IUSE=test python_targets_python3_6 python_targets_python3_7
LICENSE=Apache-2.0-with-LLVM-exceptions UoI-NCSA
DEFINED_PHASES=compile install
_md5_=4539d849d3cea8ac84debad9b3154143
";

let entry = CacheEntry::parse(input).unwrap();
let m = &entry.metadata;

assert_eq!(m.eapi.to_string(), "7");
assert_eq!(m.description, "Python bindings for sys-devel/clang");
assert_eq!(m.slot.slot, "0");
assert_eq!(m.keywords.len(), 2);
assert_eq!(entry.md5, Some("4539d849d3cea8ac84debad9b3154143".to_string()));
```

### Work with Individual Types

```rust
use portage_metadata::{Eapi, Keyword, Stability, IUse, Phase};

// EAPI
let eapi: Eapi = "8".parse().unwrap();
assert!(eapi.has_idepend());

// Keywords
let kw: Keyword = "~amd64".parse().unwrap();
assert_eq!(kw.stability, Stability::Testing);

// IUSE
let flag: IUse = "+ssl".parse().unwrap();
assert_eq!(flag.name(), "ssl");

// Phases
let phases = Phase::parse_line("compile configure install").unwrap();
assert_eq!(phases.len(), 3);
```

## Core Types

| Type | Description | PMS Section |
|------|-------------|-------------|
| `CacheEntry` | Full md5-cache file: metadata + MD5 + eclasses | 14.3 |
| `EbuildMetadata` | All ebuild-defined metadata variables | 7.2 |
| `Eapi` | EAPI version (0–9) with feature queries | 6 |
| `Keyword` / `Stability` | Architecture keywords | 7.2 |
| `IUse` / `IUseDefault` | USE flag declarations | 7.2 |
| `Phase` | Defined phase functions | 9 |
| `SrcUriEntry` | SRC_URI expression tree | 7.2, 8.2 |
| `LicenseExpr` | LICENSE expression tree | 7.2, 8.2 |
| `RequiredUseExpr` | REQUIRED_USE constraints | 7.2 |
| `RestrictExpr` | RESTRICT/PROPERTIES entries | 7.2 |

## PMS Compliance

This library implements **Package Manager Specification (PMS) 9** with support for:

- **Metadata Cache** (PMS Chapter 14) — md5-dict `KEY=VALUE` format
- **Ebuild Variables** (PMS Chapter 7) — EAPI, SLOT, DESCRIPTION, KEYWORDS, IUSE, SRC_URI, LICENSE, REQUIRED_USE, RESTRICT, PROPERTIES, DEPEND, RDEPEND, BDEPEND, PDEPEND, IDEPEND
- **Dependency Specification** (PMS Chapter 8) — via portage-atom
- **Phase Functions** (PMS Chapter 9) — DEFINED_PHASES parsing
- **EAPI Features** (PMS Chapter 6) — feature queries per EAPI level
- **Selective URI Restrictions** (PMS 7.3.2, EAPI 8+) — `fetch+`/`mirror+` prefixes in SRC_URI

## Related Projects

- [portage-atom](https://crates.io/crates/portage-atom) — Portage package atom parser
- [pkgcraft](https://crates.io/crates/pkgcraft) — Full-featured Gentoo package manager library
- [PMS 9](https://projects.gentoo.org/pms/9/pms.html) — Package Manager Specification
- [Portage](https://wiki.gentoo.org/wiki/Portage) — Reference Gentoo package manager

## License

[MIT](LICENSE-MIT)

## Contributing

See [AGENTS.md](AGENTS.md) for project conventions and contribution guidelines.

## Author

Luca Barbato <lu_zero@gentoo.org>
