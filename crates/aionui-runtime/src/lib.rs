//! Bundled runtime (bun) resolver for aionui-backend.
//!
//! Embeds the bun runtime at build time (zstd-compressed) and extracts it
//! to the user's OS cache directory on first call. Callers use
//! [`resolve_bun`] to obtain a usable executable path and [`bun_bin_dir`]
//! to prepend the runtime directory to child-process `PATH`.

mod cache;
mod embed;
mod extract;
mod resolver;

pub use resolver::{ResolveError, bun_bin_dir, resolve_bun};

#[cfg(test)]
#[path = "../build_support.rs"]
mod build_support_tests;
