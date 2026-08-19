//! Gentoo stage3 image support
//!
//! This crate provides functionality for fetching, parsing, and managing
//! Gentoo Linux stage3 images.
#![warn(missing_docs)]

mod cache;
mod client;
mod error;
mod stage3;

pub use cache::Cache;
pub use client::{Client, ClientBuilder};
pub use error::Error;
pub use gentoo_core::Arch;
pub use stage3::Stage3;
