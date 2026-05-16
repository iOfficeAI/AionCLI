//! Process-level bootstrap helpers for the binary.
//!
//! These are *not* subcommands — they are layered initialization steps
//! (logging, work_dir resolution, builtin-skill materialization, database
//! init) that subcommands compose to start the application.

mod builtin_skills;
mod environment;
mod tracing_init;
mod work_dir;

pub use environment::{ServerEnvironment, init_data_layer, init_environment};
