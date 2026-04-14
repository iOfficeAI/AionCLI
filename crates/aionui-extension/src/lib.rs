pub mod constants;
pub mod dependency;
pub mod error;
pub mod loader;
pub mod manifest;
pub mod permission;
pub mod state;
pub mod template;
pub mod types;

pub use constants::*;
pub use dependency::{
    topological_sort, validate_dependencies, DependencyIssue, DependencyValidationResult,
};
pub use error::ExtensionError;
pub use loader::{filter_by_engine_compatibility, load_all, resolve_scan_paths, ScanPath};
pub use manifest::{parse_manifest, validate_manifest};
pub use permission::{build_permission_summary, calculate_risk_level};
pub use state::{load_states_from_file, save_states_to_file, ExtensionStateStore};
pub use template::{resolve_env_map, resolve_env_templates, resolve_file_reference};
pub use types::*;
