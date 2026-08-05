# portage-atom

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Build Status](https://github.com/lu-zero/portage-atom/workflows/CI/badge.svg)](https://github.com/lu-zero/portage-atom/actions?query=workflow:CI)
[![codecov](https://codecov.io/gh/lu-zero/portage-atom/graph/badge.svg)](https://codecov.io/gh/lu-zero/portage-atom)
[![Crates.io](https://img.shields.io/crates/v/portage-atom.svg)](https://crates.io/crates/portage-atom)
[![dependency status](https://deps.rs/repo/github/lu-zero/portage-atom/status.svg)](https://deps.rs/repo/github/lu-zero/portage-atom)
[![docs.rs](https://docs.rs/portage-atom/badge.svg)](https://docs.rs/portage-atom)

A Rust library for parsing Portage package atoms, based on the [Package Manager Specification (PMS) 9](https://projects.gentoo.org/pms/9/pms.html).

> **Warning**: This codebase was largely AI-generated (slop-coded) and has not
> yet been thoroughly audited. It may contain bugs, incomplete PMS coverage, or
> surprising edge-case behaviour. Use at your own risk and please report issues.

## Overview

`portage-atom` provides types and parsing for Gentoo/Portage package atoms
using the [winnow](https://crates.io/crates/winnow) parser combinator
library, with PMS version ordering (Algorithm 3.1).

Full atom syntax:

```
[!|!!][<|<=|=|~|>=|>]<category>/<package>[-<version>][:slot][::repository][use-deps]
```

```
dev-lang/rust                          # Simple unversioned atom
>=dev-lang/rust-1.75.0                 # Version constraint
dev-lang/rust:0/1.75                   # With slot/subslot
dev-lang/rust[llvm_targets_AMDGPU]     # With USE flag
!dev-lang/rust                         # Weak blocker
=dev-lang/rust-1.75.0*                 # Glob version match
~dev-lang/rust-1.75.0                  # Approximate version
dev-lang/rust::gentoo                  # From specific repository
```

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
portage-atom = "0.11"
```

## Usage

### Parse Unversioned Atoms (Cpn)

```rust
use portage_atom::Cpn;

let cpn = Cpn::parse("dev-lang/rust")?;
assert_eq!(cpn.category, "dev-lang");
assert_eq!(cpn.package, "rust");
println!("{}", cpn);  // dev-lang/rust
```

### Parse Versioned Atoms (Cpv)

```rust
use portage_atom::Cpv;

let cpv = Cpv::parse("dev-lang/rust-1.75.0")?;
assert_eq!(cpv.version.numbers[0], 1);
assert_eq!(cpv.version.numbers[1], 75);
assert_eq!(cpv.version.numbers[2], 0);
```

### Parse Full Dependencies (Dep)

```rust
use portage_atom::{Dep, Operator};

let dep = Dep::parse(">=dev-lang/rust-1.75.0:0[llvm_targets_AMDGPU]::gentoo")?;

// Access components
assert!(dep.version.is_some());
assert_eq!(dep.version.as_ref().unwrap().op, Some(Operator::GreaterOrEqual));
assert!(dep.slot_dep.is_some());
assert!(dep.use_deps.is_some());
assert_eq!(dep.repo, Some("gentoo".to_string()));

// Display
println!("{}", dep);
```

### Version Comparison

Versions implement `Ord` according to PMS rules:

```rust
use portage_atom::Version;

let v1 = Version::parse("1.75.0")?;
let v2 = Version::parse("1.75.0-r1")?;
assert!(v1 < v2);

let v3 = Version::parse("1.75.0_rc1")?;
assert!(v3 < v1);  // RC versions are less than release
```

## Core Types

- **`Cpn`**: Category/Package Name (e.g., `dev-lang/rust`)
- **`Cpv`**: Category/Package/Version (e.g., `dev-lang/rust-1.75.0`)
- **`Dep`**: Full dependency atom with all optional components
- **`Version`**: Version with operator, numbers, letter, suffixes, and revision
- **`Slot`** / **`SlotDep`**: Slot dependencies (`:0`, `:=`, `:*`, etc.)
- **`UseDep`**: USE flag dependencies (`[flag]`, `[-flag]`, `[flag?]`, etc.)
- **`Blocker`**: Weak (`!`) or strong (`!!`) blockers
- **`Operator`**: Version operators (`<`, `<=`, `=`, `~`, `>=`, `>`, `=*`)

## Examples

`cargo run --example parse_atoms`

## Related Projects

- [PMS](https://projects.gentoo.org/pms/) - Package Manager Specification
- [Portage](https://wiki.gentoo.org/wiki/Portage) - Reference Gentoo package manager
- [pkgcraft](https://crates.io/crates/pkgcraft) - Full-featured Gentoo package manager library

## License

[MIT](LICENSE-MIT)

## Contributing

See [AGENTS.md](AGENTS.md) for project conventions and contribution guidelines.

## Author

Luca Barbato <lu_zero@gentoo.org>
