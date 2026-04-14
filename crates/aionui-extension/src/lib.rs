pub mod constants;
pub mod dependency;
pub mod error;
pub mod external_paths;
pub mod hub;
pub mod lifecycle;
pub mod loader;
pub mod manifest;
pub mod permission;
pub mod registry;
mod registry_helpers;
pub mod resolvers;
pub mod skill_service;
pub mod state;
pub mod template;
pub mod types;
pub mod watcher;

pub use constants::*;
pub use dependency::{
    topological_sort, validate_dependencies, DependencyIssue, DependencyValidationResult,
};
pub use error::ExtensionError;
pub use lifecycle::{execute_hook, needs_install_hook, resolve_hook_path, HookKind};
pub use loader::{filter_by_engine_compatibility, load_all, resolve_scan_paths, ScanPath};
pub use manifest::{parse_manifest, validate_manifest};
pub use permission::{build_permission_summary, calculate_risk_level};
pub use registry::{ExtensionRegistry, ExtensionSummary};
pub use resolvers::{resolve_all_contributions, resolve_extension_contributions, resolve_i18n_for_all};
pub use state::{load_states_from_file, save_states_to_file, ExtensionStateStore};
pub use template::{resolve_env_map, resolve_env_templates, resolve_file_reference};
pub use types::*;
pub use watcher::ExtensionWatcher;

pub use external_paths::ExternalPathsManager;
pub use hub::{HubIndexManager, HubInstaller};
pub use skill_service::{
    delete_skill, detect_and_count_external_skills, detect_common_skill_paths,
    export_skill_with_symlink, get_skill_paths, import_skill, import_skill_with_symlink,
    list_available_skills, read_builtin_rule, read_builtin_skill, read_skill_info,
    resolve_skill_paths, scan_for_skills, ExternalSkillSource, NamedPath, ScannedSkill,
    SkillListItem, SkillPaths,
};
pub use skill_service::{
    delete_assistant_rule, delete_assistant_skill, read_assistant_rule, read_assistant_skill,
    write_assistant_rule, write_assistant_skill,
};
