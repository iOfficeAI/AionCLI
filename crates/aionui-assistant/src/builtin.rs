//! Built-in assistant registry — loads `assistants.json` manifest + resolves
//! locale-templated rule/skill/avatar paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{error, warn};

/// Single built-in assistant entry, loaded from `assistants.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinAssistant {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub name_i18n: HashMap<String, String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub description_i18n: HashMap<String, String>,
    #[serde(default)]
    pub avatar: Option<String>,
    pub preset_agent_type: String,
    #[serde(default)]
    pub enabled_skills: Vec<String>,
    #[serde(default)]
    pub custom_skill_names: Vec<String>,
    #[serde(default)]
    pub disabled_builtin_skills: Vec<String>,
    /// Relative to assets_dir, may contain `{locale}`.
    #[serde(default)]
    pub rule_file: Option<String>,
    /// Relative to assets_dir, may contain `{locale}`.
    #[serde(default)]
    pub skill_file: Option<String>,
    #[serde(default)]
    pub prompts: Vec<String>,
    #[serde(default)]
    pub prompts_i18n: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BuiltinManifest {
    #[serde(default)]
    #[allow(dead_code)]
    version: String,
    #[serde(default)]
    assistants: Vec<BuiltinAssistant>,
}

/// In-memory registry of built-in assistants.
pub struct BuiltinAssistantRegistry {
    assistants: HashMap<String, BuiltinAssistant>,
    assets_dir: PathBuf,
}

impl BuiltinAssistantRegistry {
    /// Load the registry by resolving the assets directory.
    ///
    /// Graceful degradation:
    /// - Directory unresolvable → empty registry (warn).
    /// - Manifest missing → empty registry (warn).
    /// - Manifest malformed → empty registry (error).
    pub fn load() -> Self {
        let Some(assets_dir) = resolve_builtin_assets_dir() else {
            warn!("Built-in assistants directory not resolvable; using empty registry");
            return Self::empty();
        };
        Self::load_from_dir(assets_dir)
    }

    /// Load from an explicit directory. Used by tests and by [`load`] via the
    /// default resolver.
    pub fn load_from_dir(assets_dir: PathBuf) -> Self {
        let manifest_path = assets_dir.join("assistants.json");
        let content = match std::fs::read_to_string(&manifest_path) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Built-in manifest missing at {}: {}",
                    manifest_path.display(),
                    e
                );
                return Self {
                    assistants: HashMap::new(),
                    assets_dir,
                };
            }
        };
        let manifest: BuiltinManifest = match serde_json::from_str(&content) {
            Ok(m) => m,
            Err(e) => {
                error!("Built-in manifest parse failed: {}", e);
                return Self {
                    assistants: HashMap::new(),
                    assets_dir,
                };
            }
        };
        let assistants = manifest
            .assistants
            .into_iter()
            .map(|a| (a.id.clone(), a))
            .collect();
        Self {
            assistants,
            assets_dir,
        }
    }

    /// Construct an empty registry (safe fallback + test helper).
    pub fn empty() -> Self {
        Self {
            assistants: HashMap::new(),
            assets_dir: PathBuf::new(),
        }
    }

    pub fn has(&self, id: &str) -> bool {
        self.assistants.contains_key(id)
    }

    pub fn get(&self, id: &str) -> Option<&BuiltinAssistant> {
        self.assistants.get(id)
    }

    pub fn all(&self) -> impl Iterator<Item = &BuiltinAssistant> {
        self.assistants.values()
    }

    pub fn assets_dir(&self) -> &Path {
        &self.assets_dir
    }

    pub fn is_empty(&self) -> bool {
        self.assistants.is_empty()
    }

    pub fn len(&self) -> usize {
        self.assistants.len()
    }

    /// Resolve the on-disk path for a built-in assistant's rule file.
    /// Substitutes `{locale}` in `rule_file`.
    pub fn rule_path(&self, id: &str, locale: &str) -> Option<PathBuf> {
        let a = self.assistants.get(id)?;
        let rel = a.rule_file.as_ref()?;
        let resolved = rel.replace("{locale}", locale);
        Some(self.assets_dir.join(resolved))
    }

    /// Resolve the on-disk path for a built-in assistant's skill file.
    pub fn skill_path(&self, id: &str, locale: &str) -> Option<PathBuf> {
        let a = self.assistants.get(id)?;
        let rel = a.skill_file.as_ref()?;
        let resolved = rel.replace("{locale}", locale);
        Some(self.assets_dir.join(resolved))
    }

    /// Resolve the on-disk path for a built-in assistant's avatar asset.
    pub fn avatar_path(&self, id: &str) -> Option<PathBuf> {
        let a = self.assistants.get(id)?;
        let rel = a.avatar.as_ref()?;
        Some(self.assets_dir.join(rel))
    }
}

impl Default for BuiltinAssistantRegistry {
    fn default() -> Self {
        Self::empty()
    }
}

fn resolve_builtin_assets_dir() -> Option<PathBuf> {
    if let Ok(env) = std::env::var("AIONUI_BUILTIN_ASSISTANTS_PATH") {
        let p = PathBuf::from(env);
        if p.exists() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("assets").join("builtin-assistants");
    if dir.exists() {
        return Some(dir);
    }
    // Dev fallback: cargo run from workspace root
    let cargo_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let dev = PathBuf::from(cargo_dir)
        .parent()?
        .join("aionui-app")
        .join("assets")
        .join("builtin-assistants");
    if dev.exists() {
        return Some(dev);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_manifest(dir: &Path, body: &str) {
        std::fs::write(dir.join("assistants.json"), body).unwrap();
    }

    #[test]
    fn load_missing_dir_returns_empty_with_warn() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nope");
        let reg = BuiltinAssistantRegistry::load_from_dir(missing);
        assert!(reg.is_empty());
    }

    #[test]
    fn load_missing_manifest_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        assert!(reg.is_empty());
    }

    #[test]
    fn load_malformed_manifest_returns_empty() {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), "{not valid json");
        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        assert!(reg.is_empty());
    }

    #[test]
    fn load_empty_list_is_ok() {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), r#"{"version":"1.0.0","assistants":[]}"#);
        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        assert!(reg.is_empty());
        assert!(!reg.has("anything"));
    }

    #[test]
    fn load_happy_path_resolves_ids_and_paths() {
        let tmp = TempDir::new().unwrap();
        write_manifest(
            tmp.path(),
            r#"{
                "version": "1.0.0",
                "assistants": [
                    {
                        "id": "builtin-office",
                        "name": "Office",
                        "presetAgentType": "gemini",
                        "ruleFile": "rules/office.{locale}.md",
                        "skillFile": "skills/office.{locale}.md",
                        "avatar": "assets/office.svg"
                    }
                ]
            }"#,
        );
        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        assert_eq!(reg.len(), 1);
        assert!(reg.has("builtin-office"));
        assert!(!reg.has("missing"));

        let rule = reg.rule_path("builtin-office", "en-US").unwrap();
        assert!(rule.ends_with("rules/office.en-US.md"));

        let skill = reg.skill_path("builtin-office", "zh-CN").unwrap();
        assert!(skill.ends_with("skills/office.zh-CN.md"));

        let avatar = reg.avatar_path("builtin-office").unwrap();
        assert!(avatar.ends_with("assets/office.svg"));
    }

    #[test]
    fn rule_path_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        write_manifest(
            tmp.path(),
            r#"{
                "assistants": [
                    { "id": "no-rule", "name": "x", "presetAgentType": "gemini" }
                ]
            }"#,
        );
        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        assert!(reg.rule_path("no-rule", "en-US").is_none());
        assert!(reg.skill_path("no-rule", "en-US").is_none());
        assert!(reg.avatar_path("no-rule").is_none());
    }

    #[test]
    fn all_and_get_expose_entries() {
        let tmp = TempDir::new().unwrap();
        write_manifest(
            tmp.path(),
            r#"{
                "assistants": [
                    { "id": "a1", "name": "A", "presetAgentType": "gemini" },
                    { "id": "a2", "name": "B", "presetAgentType": "claude" }
                ]
            }"#,
        );
        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        let ids: Vec<String> = reg.all().map(|a| a.id.clone()).collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(reg.get("a1").unwrap().name, "A");
        assert_eq!(reg.get("a2").unwrap().preset_agent_type, "claude");
    }
}
