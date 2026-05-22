//! Bundled node runtime resolver for aioncore.
//!
//! Embeds the node runtime at build time (zstd+tar compressed) and extracts
//! it to the user's OS cache directory on first call. Callers use
//! [`resolve_node`] to obtain a usable executable path, [`node_bin_dir`] to
//! prepend the runtime directory to child-process `PATH`, and
//! [`resolve_npm_cli_js`] to locate the bundled npm CLI script.

mod cache;
mod embed;
mod extract;
mod resolver;
mod shell_env;

pub use cache::init;
pub use resolver::{
    ResolveError, node_bin_dir, resolve_command_in, resolve_command_path,
    resolve_node, resolve_npm_cli_js,
};
pub use shell_env::enhance_process_path;
mod spawn;
pub use spawn::Builder;

#[cfg(test)]
#[path = "../build_support.rs"]
mod build_support_tests;
