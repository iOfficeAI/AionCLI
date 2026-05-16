//! Standalone static file serving with pluggable async access control.
//!
//! This crate provides a framework-agnostic `StaticFileService` that resolves
//! a (root, relative_path) pair into a validated file ready for streaming.
//! An optional async guard function can be injected to control access.
pub mod guard;
pub mod service;

pub use guard::{AccessDenied, AccessGuardFn, GuardResult, RequestContext, make_guard};
pub use service::{ByteRange, ServeError, ServedFile, StaticFileConfig, StaticFileService, parse_range};
