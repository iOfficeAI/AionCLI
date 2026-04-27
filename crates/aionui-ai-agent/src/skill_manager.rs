use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use regex::Regex;
use tokio::sync::RwLock;
use tracing::{debug, warn};

static LOAD_SKILL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[LOAD_SKILL:\s*([^\]]+)\]").expect("valid regex"));

/// A discovered skill definition.
#[derive(Debug, Clone)]
pub struct SkillDefinition {
    /// Skill name (directory name or frontmatter `name`).
    pub name: String,
    /// One-line description from SKILL.md frontmatter.
    pub description: String,
    /// File system path to the SKILL.md file (absolute for custom/extension,
    /// or the materialized view path for builtin).
    pub location: PathBuf,
    /// Origin of this skill (builtin/custom/extension).
    pub source: aionui_extension::SkillSource,
    /// Relative path inside the builtin skill corpus
    /// (e.g. `auto-inject/cron/SKILL.md`); `None` for non-builtin sources.
    pub relative_location: Option<String>,
    /// Lazily-loaded full content (body after frontmatter).
    pub body: Option<String>,
}

/// Lightweight skill reference for index listings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillIndex {
    pub name: String,
    pub description: String,
}

/// Manages skill discovery, indexing, and on-demand loading.
///
/// Skills are stored in directories containing a `SKILL.md` file.
/// The SKILL.md frontmatter provides `name` and `description`.
/// The body (content after frontmatter) is loaded on demand.
pub struct AcpSkillManager {
    /// Cached skill definitions keyed by skill name.
    cache: RwLock<HashMap<String, SkillDefinition>>,
    /// Whether discovery has been performed.
    discovered: RwLock<bool>,
    /// Resolved skill paths, shared across the app.
    /// Consumed by `discover_skills` / `get_skill` (Task 4 / 5 of the refactor).
    #[allow(dead_code)]
    paths: Arc<aionui_extension::SkillPaths>,
}

impl AcpSkillManager {
    pub fn new(paths: Arc<aionui_extension::SkillPaths>) -> Arc<Self> {
        Arc::new(Self {
            cache: RwLock::new(HashMap::new()),
            discovered: RwLock::new(false),
            paths,
        })
    }

    /// Discover skills via `aionui_extension::list_available_skills`.
    ///
    /// Filtering rules:
    /// - Auto-inject builtin skills (under `auto-inject/` in the corpus) are
    ///   always included unless listed in `exclude_builtin_skills`.
    /// - Opt-in builtin skills (siblings of `auto-inject/`) and custom/extension
    ///   skills are included only if `enabled_skills` contains their name.
    ///
    /// Populates the cache; subsequent `get_skill(name)` calls read body lazily.
    pub async fn discover_skills(
        &self,
        enabled_skills: Option<&[String]>,
        exclude_builtin_skills: Option<&[String]>,
    ) -> Vec<SkillIndex> {
        let items = match aionui_extension::list_available_skills(&self.paths).await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "Failed to list skills via extension service");
                Vec::new()
            }
        };

        let mut cache = self.cache.write().await;
        cache.clear();

        for item in items {
            let is_auto_inject = item
                .relative_location
                .as_deref()
                .is_some_and(|r| r.starts_with("auto-inject/"));

            let keep = match item.source {
                aionui_extension::SkillSource::Builtin => {
                    if is_auto_inject {
                        !exclude_builtin_skills
                            .is_some_and(|ex| ex.iter().any(|n| n == &item.name))
                    } else {
                        enabled_skills
                            .is_some_and(|en| en.iter().any(|n| n == &item.name))
                    }
                }
                aionui_extension::SkillSource::Custom
                | aionui_extension::SkillSource::Extension => enabled_skills
                    .is_some_and(|en| en.iter().any(|n| n == &item.name)),
            };
            if !keep {
                continue;
            }

            cache.insert(
                item.name.clone(),
                SkillDefinition {
                    name: item.name.clone(),
                    description: item.description.clone(),
                    location: std::path::PathBuf::from(&item.location),
                    source: item.source,
                    relative_location: item.relative_location.clone(),
                    body: None,
                },
            );
        }

        let mut discovered = self.discovered.write().await;
        *discovered = true;

        let index: Vec<SkillIndex> = cache
            .values()
            .map(|d| SkillIndex {
                name: d.name.clone(),
                description: d.description.clone(),
            })
            .collect();

        debug!(count = index.len(), "Skills discovered");
        index
    }

    /// Return the current skill index without re-scanning.
    pub async fn get_skills_index(&self) -> Vec<SkillIndex> {
        let cache = self.cache.read().await;
        cache
            .values()
            .map(|d| SkillIndex {
                name: d.name.clone(),
                description: d.description.clone(),
            })
            .collect()
    }

    /// Load a skill's full content by name.
    ///
    /// Returns `None` if the skill is unknown. On first access the body is
    /// read via the appropriate channel based on `source`:
    /// - `Builtin` → `aionui_extension::read_builtin_skill(&paths, relative)`
    /// - `Custom` / `Extension` → direct `tokio::fs::read_to_string(location/SKILL.md)`
    pub async fn get_skill(&self, name: &str) -> Option<SkillDefinition> {
        // Fast path: check if body is already cached
        {
            let cache = self.cache.read().await;
            match cache.get(name) {
                Some(def) if def.body.is_some() => return Some(def.clone()),
                None => return None,
                _ => {} // known, body absent — fall through
            }
        }

        // Slow path: read body per source and cache it
        let mut cache = self.cache.write().await;
        let def = cache.get_mut(name)?;
        if def.body.is_some() {
            return Some(def.clone());
        }

        let content = match def.source {
            aionui_extension::SkillSource::Builtin => {
                if let Some(rel) = def.relative_location.as_deref() {
                    match aionui_extension::read_builtin_skill(&self.paths, rel).await {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(skill = name, error = %e, "Failed to read builtin skill");
                            String::new()
                        }
                    }
                } else {
                    warn!(skill = name, "Builtin skill missing relative_location");
                    String::new()
                }
            }
            aionui_extension::SkillSource::Custom | aionui_extension::SkillSource::Extension => {
                // `location` for scanned user skills is the directory; append SKILL.md.
                let skill_file = if def.location.is_dir() {
                    def.location.join("SKILL.md")
                } else {
                    def.location.clone()
                };
                match tokio::fs::read_to_string(&skill_file).await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(skill = name, path = %skill_file.display(), error = %e, "Failed to read skill file");
                        String::new()
                    }
                }
            }
        };

        if content.is_empty() {
            return None;
        }

        def.body = Some(extract_body(&content));
        Some(def.clone())
    }

    /// Check whether discovery has been performed.
    pub async fn is_discovered(&self) -> bool {
        *self.discovered.read().await
    }
}

/// Build a formatted text block listing available skills for injection.
///
/// The output includes skill names with descriptions and instructions
/// on how to request loading via `[LOAD_SKILL: name]`.
pub fn build_skills_index_text(skills: &[SkillIndex]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut lines = Vec::with_capacity(skills.len() + 4);
    lines.push("## Available Skills".to_string());
    lines.push(String::new());
    lines.push("To load a skill, include `[LOAD_SKILL: skill-name]` in your response.".to_string());
    lines.push(String::new());

    for skill in skills {
        lines.push(format!("- **{}**: {}", skill.name, skill.description));
    }

    lines.join("\n")
}

/// Detect `[LOAD_SKILL: ...]` requests in agent output content.
///
/// Returns a list of requested skill names.
pub fn detect_skill_load_request(content: &str) -> Vec<String> {
    LOAD_SKILL_RE
        .captures_iter(content)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().trim().to_string()))
        .filter(|name| !name.is_empty())
        .collect()
}

/// Build system instructions text with full skill content (for Gemini).
pub fn build_system_instructions(base_instructions: &str, skills: &[SkillDefinition]) -> String {
    if skills.is_empty() {
        return base_instructions.to_string();
    }

    let mut parts = vec![base_instructions.to_string()];

    for skill in skills {
        if let Some(body) = &skill.body {
            parts.push(format!("\n## Skill: {}\n\n{}", skill.name, body));
        }
    }

    parts.join("\n")
}

/// Prepare the first message with skills index prefix (for ACP/Codex).
///
/// Prepends `[Assistant Rules]` block with skill index to the user content.
pub fn prepare_first_message_with_skills_index(
    content: &str,
    skills: &[SkillIndex],
    preset_context: Option<&str>,
) -> String {
    let mut parts = Vec::new();

    let index_text = build_skills_index_text(skills);
    let has_rules = !index_text.is_empty() || preset_context.is_some();

    if has_rules {
        parts.push("[Assistant Rules]".to_string());

        if let Some(ctx) = preset_context
            && !ctx.is_empty()
        {
            parts.push(ctx.to_string());
        }

        if !index_text.is_empty() {
            parts.push(index_text);
        }

        parts.push("[/Assistant Rules]".to_string());
        parts.push(String::new());
    }

    parts.push(content.to_string());
    parts.join("\n")
}

/// Build system instructions with skills index only (for Gemini index-only mode).
///
/// Unlike [`build_system_instructions`] which injects full skill bodies,
/// this variant injects only the skill index (name + description) and
/// the `[LOAD_SKILL]` protocol, allowing the agent to request full content on demand.
pub fn build_system_instructions_with_skills_index(
    base_instructions: &str,
    skills: &[SkillIndex],
) -> String {
    let index_text = build_skills_index_text(skills);
    if index_text.is_empty() {
        return base_instructions.to_string();
    }

    format!("{base_instructions}\n\n{index_text}")
}

/// Prepare the first message with full skill content (for Gemini).
///
/// Prepends `[Assistant Rules]` block with complete skill bodies.
pub fn prepare_first_message(
    content: &str,
    skills: &[SkillDefinition],
    preset_context: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    let has_rules = !skills.is_empty() || preset_context.is_some();

    if has_rules {
        parts.push("[Assistant Rules]".to_string());

        if let Some(ctx) = preset_context
            && !ctx.is_empty()
        {
            parts.push(ctx.to_string());
        }

        for skill in skills {
            if let Some(body) = &skill.body {
                parts.push(format!("## Skill: {}\n\n{}", skill.name, body));
            }
        }

        parts.push("[/Assistant Rules]".to_string());
        parts.push(String::new());
    }

    parts.push(content.to_string());
    parts.join("\n")
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parse SKILL.md frontmatter to extract name and description.
///
/// Expected format:
/// ```text
/// ---
/// name: skill-name
/// description: One line description
/// ---
/// Body content here...
/// ```
async fn parse_skill_frontmatter(path: &Path) -> Option<SkillDefinition> {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(e) => {
            warn!(path = %path.display(), error = %e, "Failed to read SKILL.md");
            return None;
        }
    };

    let (name, description) = parse_frontmatter_fields(&content)?;

    // Use directory name as fallback for name
    let final_name = if name.is_empty() {
        path.parent()?.file_name()?.to_string_lossy().into_owned()
    } else {
        name
    };

    Some(SkillDefinition {
        name: final_name,
        description,
        location: path.to_path_buf(),
        source: aionui_extension::SkillSource::Custom,
        relative_location: None,
        body: None, // Lazy loaded
    })
}

/// Parse frontmatter fields from SKILL.md content.
///
/// Returns `(name, description)` if valid frontmatter is found.
fn parse_frontmatter_fields(content: &str) -> Option<(String, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    // Find the closing `---`
    let after_open = &trimmed[3..];
    let close_idx = after_open.find("---")?;
    let frontmatter = &after_open[..close_idx];

    let mut name = String::new();
    let mut description = String::new();

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("name:") {
            name = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("description:") {
            description = val.trim().to_string();
        }
    }

    if description.is_empty() {
        return None;
    }

    Some((name, description))
}

/// Extract the body content after YAML frontmatter.
fn extract_body(content: &str) -> String {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content.to_string();
    }

    let after_open = &trimmed[3..];
    if let Some(close_idx) = after_open.find("---") {
        let after_close = &after_open[close_idx + 3..];
        after_close.trim_start_matches('\n').to_string()
    } else {
        content.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn new_accepts_skill_paths() {
        let tmp = TempDir::new().unwrap();
        let paths = std::sync::Arc::new(aionui_extension::resolve_skill_paths(
            tmp.path(),
            tmp.path(),
        ));
        let mgr = AcpSkillManager::new(paths.clone());
        assert!(!mgr.is_discovered().await);
    }

    #[test]
    fn skill_definition_has_source_and_relative_location() {
        let def = SkillDefinition {
            name: "x".into(),
            description: "d".into(),
            location: PathBuf::from("/tmp/x"),
            source: aionui_extension::SkillSource::Builtin,
            relative_location: Some("auto-inject/x/SKILL.md".into()),
            body: None,
        };
        assert_eq!(def.source, aionui_extension::SkillSource::Builtin);
        assert_eq!(
            def.relative_location.as_deref(),
            Some("auto-inject/x/SKILL.md")
        );
    }

    fn create_skill_dir(base: &Path, dir_name: &str, skill_name: &str, desc: &str, body: &str) {
        let dir = base.join(dir_name);
        fs::create_dir_all(&dir).unwrap();
        let content = format!(
            "---\nname: {}\ndescription: {}\n---\n{}",
            skill_name, desc, body
        );
        fs::write(dir.join("SKILL.md"), content).unwrap();
    }

    // -----------------------------------------------------------------------
    // Frontmatter parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_frontmatter_valid() {
        let content = "---\nname: security-review\ndescription: Review code for security issues\n---\nBody here";
        let result = parse_frontmatter_fields(content);
        assert!(result.is_some());
        let (name, desc) = result.unwrap();
        assert_eq!(name, "security-review");
        assert_eq!(desc, "Review code for security issues");
    }

    #[test]
    fn parse_frontmatter_no_opening_delimiter() {
        let content = "name: test\ndescription: desc\n---\nbody";
        assert!(parse_frontmatter_fields(content).is_none());
    }

    #[test]
    fn parse_frontmatter_no_closing_delimiter() {
        let content = "---\nname: test\ndescription: desc\nbody without close";
        assert!(parse_frontmatter_fields(content).is_none());
    }

    #[test]
    fn parse_frontmatter_missing_description() {
        let content = "---\nname: test\n---\nbody";
        assert!(parse_frontmatter_fields(content).is_none());
    }

    #[test]
    fn parse_frontmatter_empty_name_uses_dir() {
        let content = "---\nname: \ndescription: A useful skill\n---\nbody";
        let (name, desc) = parse_frontmatter_fields(content).unwrap();
        assert!(name.is_empty()); // Will be replaced by dir name in parse_skill_frontmatter
        assert_eq!(desc, "A useful skill");
    }

    // -----------------------------------------------------------------------
    // Body extraction
    // -----------------------------------------------------------------------

    #[test]
    fn extract_body_with_frontmatter() {
        let content = "---\nname: test\ndescription: desc\n---\nBody content\nMore lines";
        let body = extract_body(content);
        assert_eq!(body, "Body content\nMore lines");
    }

    #[test]
    fn extract_body_no_frontmatter() {
        let content = "Just plain text";
        assert_eq!(extract_body(content), "Just plain text");
    }

    #[test]
    fn extract_body_no_closing_delimiter() {
        let content = "---\nname: test\nno closing";
        assert_eq!(extract_body(content), content);
    }

    // -----------------------------------------------------------------------
    // LOAD_SKILL detection
    // -----------------------------------------------------------------------

    #[test]
    fn detect_single_skill_request() {
        let content = "Let me use [LOAD_SKILL: security-review] for this.";
        let skills = detect_skill_load_request(content);
        assert_eq!(skills, vec!["security-review"]);
    }

    #[test]
    fn detect_multiple_skill_requests() {
        let content = "[LOAD_SKILL: a] some text [LOAD_SKILL: b]";
        let skills = detect_skill_load_request(content);
        assert_eq!(skills, vec!["a", "b"]);
    }

    #[test]
    fn detect_skill_request_with_spaces() {
        let content = "[LOAD_SKILL:   padded-name   ]";
        let skills = detect_skill_load_request(content);
        assert_eq!(skills, vec!["padded-name"]);
    }

    #[test]
    fn detect_no_skill_request() {
        let content = "Just regular text with no commands.";
        let skills = detect_skill_load_request(content);
        assert!(skills.is_empty());
    }

    #[test]
    fn detect_skill_request_empty_name_ignored() {
        let content = "[LOAD_SKILL:  ]";
        let skills = detect_skill_load_request(content);
        assert!(skills.is_empty());
    }

    // -----------------------------------------------------------------------
    // Skills index text
    // -----------------------------------------------------------------------

    #[test]
    fn build_skills_index_text_empty() {
        assert!(build_skills_index_text(&[]).is_empty());
    }

    #[test]
    fn build_skills_index_text_with_skills() {
        let skills = vec![
            SkillIndex {
                name: "review".into(),
                description: "Code review".into(),
            },
            SkillIndex {
                name: "debug".into(),
                description: "Debugging helper".into(),
            },
        ];
        let text = build_skills_index_text(&skills);
        assert!(text.contains("## Available Skills"));
        assert!(text.contains("[LOAD_SKILL: skill-name]"));
        assert!(text.contains("- **review**: Code review"));
        assert!(text.contains("- **debug**: Debugging helper"));
    }

    // -----------------------------------------------------------------------
    // First message preparation
    // -----------------------------------------------------------------------

    #[test]
    fn prepare_first_message_with_index_no_skills() {
        let result = prepare_first_message_with_skills_index("Hello", &[], None);
        assert_eq!(result, "Hello");
    }

    #[test]
    fn prepare_first_message_with_index_and_context() {
        let skills = vec![SkillIndex {
            name: "test".into(),
            description: "Testing".into(),
        }];
        let result = prepare_first_message_with_skills_index("Hello", &skills, Some("Be concise."));
        assert!(result.contains("[Assistant Rules]"));
        assert!(result.contains("Be concise."));
        assert!(result.contains("- **test**: Testing"));
        assert!(result.contains("[/Assistant Rules]"));
        assert!(result.ends_with("Hello"));
    }

    #[test]
    fn prepare_first_message_with_full_skills() {
        let skills = vec![SkillDefinition {
            name: "review".into(),
            description: "Review".into(),
            location: PathBuf::new(),
            source: aionui_extension::SkillSource::Custom,
            relative_location: None,
            body: Some("Full review instructions here.".into()),
        }];
        let result = prepare_first_message("Hello", &skills, None);
        assert!(result.contains("[Assistant Rules]"));
        assert!(result.contains("## Skill: review"));
        assert!(result.contains("Full review instructions here."));
        assert!(result.contains("[/Assistant Rules]"));
        assert!(result.ends_with("Hello"));
    }

    #[test]
    fn prepare_first_message_no_skills_no_context() {
        let result = prepare_first_message("Hello", &[], None);
        assert_eq!(result, "Hello");
    }

    #[test]
    fn prepare_first_message_context_only() {
        let result = prepare_first_message_with_skills_index("Hello", &[], Some("Rules here."));
        assert!(result.contains("[Assistant Rules]"));
        assert!(result.contains("Rules here."));
        assert!(result.ends_with("Hello"));
    }

    // -----------------------------------------------------------------------
    // System instructions builder
    // -----------------------------------------------------------------------

    #[test]
    fn build_system_instructions_no_skills() {
        let result = build_system_instructions("Base prompt", &[]);
        assert_eq!(result, "Base prompt");
    }

    #[test]
    fn build_system_instructions_with_skills() {
        let skills = vec![SkillDefinition {
            name: "helper".into(),
            description: "A helper".into(),
            location: PathBuf::new(),
            source: aionui_extension::SkillSource::Custom,
            relative_location: None,
            body: Some("Helper body content.".into()),
        }];
        let result = build_system_instructions("Base prompt", &skills);
        assert!(result.starts_with("Base prompt"));
        assert!(result.contains("## Skill: helper"));
        assert!(result.contains("Helper body content."));
    }

    #[test]
    fn build_system_instructions_with_skills_index_no_skills() {
        let result = build_system_instructions_with_skills_index("Base prompt", &[]);
        assert_eq!(result, "Base prompt");
    }

    #[test]
    fn build_system_instructions_with_skills_index_includes_index() {
        let skills = vec![SkillIndex {
            name: "helper".into(),
            description: "A helper skill".into(),
        }];
        let result = build_system_instructions_with_skills_index("Base prompt", &skills);
        assert!(result.starts_with("Base prompt"));
        assert!(result.contains("## Available Skills"));
        assert!(result.contains("- **helper**: A helper skill"));
        assert!(result.contains("[LOAD_SKILL: skill-name]"));
    }

    #[test]
    fn build_system_instructions_skips_unloaded_skills() {
        let skills = vec![SkillDefinition {
            name: "unloaded".into(),
            description: "Not loaded".into(),
            location: PathBuf::new(),
            source: aionui_extension::SkillSource::Custom,
            relative_location: None,
            body: None,
        }];
        let result = build_system_instructions("Base", &skills);
        assert_eq!(result, "Base");
    }

    // -----------------------------------------------------------------------
    // AcpSkillManager async tests
    //
    // Discovery-layout tests moved to `tests/skill_manager_integration.rs`
    // because they now need `aionui_extension::BUILTIN_SKILLS_ENV_VAR` to
    // point the extension service at a tempdir corpus. Only the tests that
    // don't require a skill corpus remain here.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_skill_unknown_returns_none() {
        let tmp = TempDir::new().unwrap();
        let mgr = AcpSkillManager::new(std::sync::Arc::new(
            aionui_extension::resolve_skill_paths(tmp.path(), tmp.path()),
        ));
        assert!(mgr.get_skill("nonexistent").await.is_none());
    }
}
