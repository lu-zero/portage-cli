# gentoo-core

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Crates.io](https://img.shields.io/crates/v/gentoo-core.svg)](https://crates.io/crates/gentoo-core)
[![docs.rs](https://docs.rs/gentoo-core/badge.svg)](https://docs.rs/gentoo-core)
[![CI](https://github.com/lu-zero/gentoo-core/actions/workflows/ci.yml/badge.svg)](https://github.com/lu-zero/gentoo-core/actions/workflows/ci.yml)
[![codecov](https://codecov.io/github/lu-zero/gentoo-core/graph/badge.svg?token=fApuKCrcgU)](https://codecov.io/github/lu-zero/gentoo-core)

Gentoo architecture and release-variant types: parsing, keyword mapping, and
representation.

## Architecture Support

The crate supports the following Gentoo architectures:

- `arm`, `aarch64` (arm64)
- `x86`, `amd64` (x86_64)
- `riscv`, `riscv64`
- `powerpc`, `ppc64`
- And their Gentoo keyword equivalents

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
gentoo-core = "0.6"
```

## Usage

```rust
use gentoo_core::{Arch, Variant, KnownArch};

// Parse a known architecture
let known = KnownArch::parse("amd64").unwrap();
println!("Known: {} (keyword: {}, bitness: {})", known, known.as_keyword(), known.bitness());

// Intern an architecture (known or exotic)
let arch = Arch::intern("amd64");
println!("Arch: {} (keyword: {})", arch, arch.as_keyword());

// Exotic architectures work the same way
let exotic = Arch::intern("my-custom-board");
println!("Exotic: {} (keyword: {})", exotic, exotic.as_keyword());

// Parse a variant
let variant: Variant = "amd64-systemd".parse().unwrap();
println!("Variant: {} (arch: {}, flavor: {})", variant, variant.keyword(), variant.flavor());
```

## Examples

Run the included examples:

```bash
cargo run --example arch
```

## Contributing

See [AGENTS.md](AGENTS.md) for project conventions and contribution guidelines.

## License

MIT
