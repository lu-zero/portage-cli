# Gentoo Stages

[![Crates.io](https://img.shields.io/crates/v/gentoo-stages.svg)](https://crates.io/crates/gentoo-stages)
[![Docs.rs](https://docs.rs/gentoo-stages/badge.svg)](https://docs.rs/gentoo-stages)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![CI](https://github.com/lu-zero/gentoo-stages/actions/workflows/ci.yml/badge.svg)](https://github.com/lu-zero/gentoo-stages/actions/workflows/ci.yml)
[![codecov](https://codecov.io/github/lu-zero/gentoo-stages/graph/badge.svg?token=U326K4DQ0I)](https://codecov.io/github/lu-zero/gentoo-stages)


A Rust crate for fetching and managing Gentoo Linux stage3 images.

## Overview

`gentoo-stages` provides functionality for working with Gentoo Linux stage3 tarballs, including:

- Listing available stage3 flavors for architectures
- Fetching latest stage3 images from Gentoo mirrors
- Downloading and caching stage3 images
- Extracting stage3 images to target directories

## Features

- **Async API** using Tokio runtime for non-blocking I/O operations
- **Streaming downloads** for memory-efficient large file transfers
- **Connection pooling** via reusable HTTP client
- **Comprehensive error handling** with thiserror
- **Logging support** via log crate

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
gentoo-stages = "0.6"
tokio = { version = "1.0", features = ["full"] }  # Required for async runtime
```

The `gentoo-core` dependency is re-exported, so you don't need to add it explicitly.

### Example: List Available Flavors (Async)

```rust
use gentoo_stages::{Client, Arch, KnownArch};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a client for riscv64 architecture
    let client = Client::builder()
        .arch(Arch::Known(KnownArch::Riscv64))
        .cache_dir("./cache")
        .build()?;

    // List all available stage3 images (async)
    let stage3_list = client.list().await?;

    println!("Available stage3 images:");
    for stage3 in stage3_list {
        println!("- {} ({} bytes)", stage3.variant, stage3.size);
    }

    Ok(())
}
```

### Example: Download Latest Stage3 (Async)

```rust
use gentoo_stages::{Client, Arch, KnownArch};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a client for riscv64 architecture
    let client = Client::builder()
        .arch(Arch::Known(KnownArch::Riscv64))
        .cache_dir("./cache")
        .build()?;

    // Download specific stage3 variant (async)
    let stage3 = client.get("rv64_lp64d-openrc").await?;

    println!("Downloaded: {}", stage3.name);
    println!("Size: {} bytes", stage3.size);
    println!("Cached at: {}", stage3.file_path().display());

    Ok(())
}
```

## Async API

As of version 0.5.0, `gentoo-stages` uses a fully asynchronous API for better performance during I/O-bound operations.

### Benefits of Async

- **Non-blocking I/O**: Better resource utilization during network operations
- **Streaming downloads**: Memory-efficient handling of large stage3 files
- **Connection pooling**: Reduced latency for multiple requests
- **Concurrent operations**: Ability to fetch multiple stage3 images in parallel

### Async Requirements

Your application must:

1. **Use Tokio runtime**: Add `tokio` to your dependencies
2. **Mark async entry points**: Use `#[tokio::main]` for async main functions
3. **Use `.await`**: All gentoo-stages methods require `.await`

## Examples

The crate includes working examples:

- `list.rs` - List available stage3 images for a given architecture (async)
- `download.rs` - Download a specific stage3 image (async)

Run examples with:

```bash
# List available images for current architecture
cargo run --example list

# List available images for a specific architecture
cargo run --example list -- amd64

# Download latest stage3 for riscv64
cargo run --example download
```

## Architecture Support

Supports all Gentoo architectures via `gentoo-core::Arch`:

- ARM (arm, arm64)
- x86 (x86, amd64)
- RISC-V (riscv32, riscv64)
- PowerPC (ppc, ppc64)

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Related Projects

- [gentoo-core](https://github.com/lu-zero/gentoo-core) - Core Gentoo types and utilities
