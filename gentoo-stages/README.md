# Gentoo Stages

[![Crates.io](https://img.shields.io/crates/v/gentoo-stages.svg)](https://crates.io/crates/gentoo-stages)
[![Docs.rs](https://docs.rs/gentoo-stages/badge.svg)](https://docs.rs/gentoo-stages)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![CI](https://github.com/lu-zero/gentoo-stages/actions/workflows/ci.yml/badge.svg)](https://github.com/lu-zero/gentoo-stages/actions/workflows/ci.yml)
[![codecov](https://codecov.io/github/lu-zero/gentoo-stages/graph/badge.svg?token=U326K4DQ0I)](https://codecov.io/github/lu-zero/gentoo-stages)

Fetching and caching of Gentoo Linux stage3 tarballs: listing available
flavors per architecture, downloading, and extraction — an async API over
Tokio, with streaming downloads and a pooled HTTP client.

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
gentoo-stages = "0.6"
tokio = { version = "1.0", features = ["full"] }  # Required for async runtime
```

The `gentoo-core` dependency is re-exported, so you don't need to add it explicitly.

```rust
use gentoo_stages::{Client, Arch, KnownArch};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder()
        .arch(Arch::Known(KnownArch::Riscv64))
        .cache_dir("./cache")
        .build()?;

    let stage3_list = client.list().await?;
    for stage3 in stage3_list {
        println!("{} ({} bytes)", stage3.variant, stage3.size);
    }

    let stage3 = client.get("rv64_lp64d-openrc").await?;
    println!("Cached at: {}", stage3.file_path().display());

    Ok(())
}
```

## Examples

`cargo run --example list [arch]` / `cargo run --example download`.

## Architecture Support

Supports all Gentoo architectures via `gentoo-core::Arch`:

- ARM (arm, arm64)
- x86 (x86, amd64)
- RISC-V (riscv32, riscv64)
- PowerPC (ppc, ppc64)

## License

MIT
