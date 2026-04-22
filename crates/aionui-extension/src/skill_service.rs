use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::constants::{
    ASSISTANT_RULES_DIR_NAME, ASSISTANT_SKILLS_DIR_NAME, BUILTIN_RULES_DIR_NAME,
    BUILTIN_SKILLS_DIR_NAME, COMMON_SKILL_DIRS, SKILL_MANIFEST_FILE, SKILLS_DIR_NAME,
};
use crate::error::ExtensionError;

// ---------------------------------------------------------------------------
// Skill paths resolution
// ---------------------------------------------------------------------------

/// Resolved base directories for skill and rule management.
#[derive(Debug, Clone)]
pub struct SkillPaths {
    /// Root data directory (~/.aionui/).
    pub data_dir: PathBuf,
    /// User-created skills directory (~/.aionui/skills/).
    pub user_skills_dir: PathBuf,
    /// Built-in skills directory (app bundle resource).
    pub builtin_skills_dir: PathBuf,
    /// Built-in rules directory (app bundle resource).
    pub builtin_rules_dir: PathBuf,
    /// Assistant-level rules directory (~/.aionui/assistant-rules/).
    pub assistant_rules_dir: PathBuf,
    /// Assistant-level skills directory (~/.aionui/assistant-skills/).
    pub assistant_skills_dir: PathBuf,
}

/// Resolve standard skill paths.
///
/// `app_resource_dir` is the application's bundled resource directory
/// (e.g. the binary's parent or a configured resource path).
pub fn resolve_skill_paths(app_resource_dir: &Path) -> SkillPaths {
    let data_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".aionui");

    SkillPaths {
        user_skills_dir: data_dir.join(SKILLS_DIR_NAME),
        builtin_skills_dir: app_resource_dir.join(BUILTIN_SKILLS_DIR_NAME),
        builtin_rules_dir: app_resource_dir.join(BUILTIN_RULES_DIR_NAME),
        assistant_rules_dir: data_dir.join(ASSISTANT_RULES_DIR_NAME),
        assistant_skills_dir: data_dir.join(ASSISTANT_SKILLS_DIR_NAME),
        data_dir,
    }
}

// ---------------------------------------------------------------------------
// A. Built-in resource reading
// ---------------------------------------------------------------------------

/// Read a built-in rule file by name.
///
/// Returns the file content as a string. Returns an empty string if the
/// file does not exist (graceful degradation per API spec).
pub async fn read_builtin_rule(
    paths: &SkillPaths,
    file_name: &str,
) -> Result<String, ExtensionError> {
    validate_filename(file_name)?;
    let file_path = paths.builtin_rules_dir.join(file_name);
    read_file_or_empty(&file_path).await
}

/// Read a built-in skill file by name.
///
/// Returns the file content as a string. Returns an empty string if the
/// file does not exist.
pub async fn read_builtin_skill(
    paths: &SkillPaths,
    file_name: &str,
) -> Result<String, ExtensionError> {
    validate_filename(file_name)?;
    let file_path = paths.builtin_skills_dir.join(file_name);
    read_file_or_empty(&file_path).await
}

// ---------------------------------------------------------------------------
// B. Assistant-level CRUD
// ---------------------------------------------------------------------------

/// Read an assistant rule with locale fallback.
///
/// Fallback order:
/// 1. `{assistantId}.{locale}.md` (if locale provided)
/// 2. `{assistantId}.md`
/// 3. Empty string
pub async fn read_assistant_rule(
    paths: &SkillPaths,
    assistant_id: &str,
    locale: Option<&str>,
) -> Result<String, ExtensionError> {
    read_assistant_resource(&paths.assistant_rules_dir, assistant_id, locale).await
}

/// Write an assistant rule.
///
/// Creates `{assistantId}.{locale}.md` or `{assistantId}.md` in the
/// assistant rules directory.
pub async fn write_assistant_rule(
    paths: &SkillPaths,
    assistant_id: &str,
    content: &str,
    locale: Option<&str>,
) -> Result<bool, ExtensionError> {
    write_assistant_resource(&paths.assistant_rules_dir, assistant_id, content, locale).await
}

/// Delete all locale versions of an assistant rule.
pub async fn delete_assistant_rule(
    paths: &SkillPaths,
    assistant_id: &str,
) -> Result<bool, ExtensionError> {
    delete_assistant_resource(&paths.assistant_rules_dir, assistant_id).await
}

/// Read an assistant skill with locale fallback.
pub async fn read_assistant_skill(
    paths: &SkillPaths,
    assistant_id: &str,
    locale: Option<&str>,
) -> Result<String, ExtensionError> {
    read_assistant_resource(&paths.assistant_skills_dir, assistant_id, locale).await
}

/// Write an assistant skill.
pub async fn write_assistant_skill(
    paths: &SkillPaths,
    assistant_id: &str,
    content: &str,
    locale: Option<&str>,
) -> Result<bool, ExtensionError> {
    write_assistant_resource(&paths.assistant_skills_dir, assistant_id, content, locale).await
}

/// Delete all locale versions of an assistant skill.
pub async fn delete_assistant_skill(
    paths: &SkillPaths,
    assistant_id: &str,
) -> Result<bool, ExtensionError> {
    delete_assistant_resource(&paths.assistant_skills_dir, assistant_id).await
}

// ---------------------------------------------------------------------------
// C. Skill listing & info
// ---------------------------------------------------------------------------

/// Origin of a listed skill.
///
/// Matches the renderer contract in
/// `src/common/adapter/ipcBridge.ts::listAvailableSkills`, which filters the
/// Skills Hub UI by this value. `Extension` is reserved for
/// extension-contributed skills once `ExtensionRegistry` is wired into the
/// Rust backend; the pilot only emits `Builtin` / `Custom`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    Builtin,
    Custom,
    Extension,
}

/// A discovered skill item for listing.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillListItem {
    pub name: String,
    pub description: String,
    pub location: String,
    pub is_custom: bool,
    pub source: SkillSource,
}

/// List all available skills (built-in + user custom), deduplicated.
///
/// User custom skills override built-in skills with the same name.
pub async fn list_available_skills(
    paths: &SkillPaths,
) -> Result<Vec<SkillListItem>, ExtensionError> {
    let mut skills = std::collections::HashMap::new();

    // 1. Built-in skills (lower priority)
    if let Ok(entries) = scan_skill_dirs(&paths.builtin_skills_dir).await {
        for item in entries {
            skills.insert(
                item.name.clone(),
                SkillListItem {
                    name: item.name,
                    description: item.description,
                    location: item.path,
                    is_custom: false,
                    source: SkillSource::Builtin,
                },
            );
        }
    }

    // 2. User custom skills (higher priority, overrides builtin)
    if let Ok(entries) = scan_skill_dirs(&paths.user_skills_dir).await {
        for item in entries {
            skills.insert(
                item.name.clone(),
                SkillListItem {
                    name: item.name,
                    description: item.description,
                    location: item.path,
                    is_custom: true,
                    source: SkillSource::Custom,
                },
            );
        }
    }

    let mut result: Vec<SkillListItem> = skills.into_values().collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

/// A skill discovered during directory scanning.
#[derive(Debug, Clone, PartialEq)]
pub struct ScannedSkill {
    pub name: String,
    pub description: String,
    pub path: String,
}

/// Read skill info from a SKILL.md file without importing.
///
/// Returns `(name, description)` extracted from frontmatter.
pub async fn read_skill_info(skill_path: &Path) -> Result<(String, String), ExtensionError> {
    let skill_file = if skill_path.is_dir() {
        skill_path.join(SKILL_MANIFEST_FILE)
    } else {
        skill_path.to_path_buf()
    };

    let content = tokio::fs::read_to_string(&skill_file)
        .await
        .map_err(|_| ExtensionError::SkillNotFound(skill_path.display().to_string()))?;

    let (name, description) = parse_frontmatter_fields(&content).ok_or_else(|| {
        ExtensionError::InvalidSkillPath(format!(
            "No valid frontmatter in {}",
            skill_file.display()
        ))
    })?;

    // Fallback: use directory name if name is empty
    let final_name = if name.is_empty() {
        skill_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        name
    };

    Ok((final_name, description))
}

// ---------------------------------------------------------------------------
// D. Skill import / export / delete
// ---------------------------------------------------------------------------

/// Import a skill by copying its directory to the user skills directory.
///
/// Returns the skill name.
pub async fn import_skill(paths: &SkillPaths, skill_path: &Path) -> Result<String, ExtensionError> {
    let (name, _) = read_skill_info(skill_path).await?;
    validate_filename(&name)?;

    let target_dir = paths.user_skills_dir.join(&name);
    tokio::fs::create_dir_all(&paths.user_skills_dir).await?;

    copy_dir_recursive(skill_path, &target_dir).await?;

    debug!(skill = %name, target = %target_dir.display(), "skill imported (copy)");
    Ok(name)
}

/// Import a skill by creating a symlink in the user skills directory.
///
/// Returns the skill name.
pub async fn import_skill_with_symlink(
    paths: &SkillPaths,
    skill_path: &Path,
) -> Result<String, ExtensionError> {
    let (name, _) = read_skill_info(skill_path).await?;
    validate_filename(&name)?;

    let target_link = paths.user_skills_dir.join(&name);
    tokio::fs::create_dir_all(&paths.user_skills_dir).await?;

    // Remove existing link/dir if present
    if target_link.exists() {
        if target_link.is_symlink() || target_link.is_file() {
            tokio::fs::remove_file(&target_link).await?;
        } else {
            tokio::fs::remove_dir_all(&target_link).await?;
        }
    }

    create_symlink(skill_path, &target_link).await?;

    debug!(skill = %name, link = %target_link.display(), "skill imported (symlink)");
    Ok(name)
}

/// Export a skill by creating a symlink in the target directory.
pub async fn export_skill_with_symlink(
    skill_path: &Path,
    target_dir: &Path,
) -> Result<(), ExtensionError> {
    let skill_name = skill_path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .ok_or_else(|| ExtensionError::InvalidSkillPath(skill_path.display().to_string()))?;

    let target_link = target_dir.join(&skill_name);
    tokio::fs::create_dir_all(target_dir).await?;

    // Remove existing link if present
    if target_link.exists() {
        if target_link.is_symlink() || target_link.is_file() {
            tokio::fs::remove_file(&target_link).await?;
        } else {
            tokio::fs::remove_dir_all(&target_link).await?;
        }
    }

    create_symlink(skill_path, &target_link).await?;

    debug!(
        skill = %skill_name,
        link = %target_link.display(),
        "skill exported (symlink)"
    );
    Ok(())
}

/// Delete a user-custom skill by name.
///
/// Returns an error if the skill is built-in or does not exist.
pub async fn delete_skill(paths: &SkillPaths, skill_name: &str) -> Result<(), ExtensionError> {
    // Safety: reject path traversal
    if skill_name.contains('/') || skill_name.contains('\\') || skill_name.contains("..") {
        return Err(ExtensionError::PathTraversal(skill_name.to_string()));
    }

    // Check if it's a built-in skill
    let builtin_path = paths.builtin_skills_dir.join(skill_name);
    let user_path = paths.user_skills_dir.join(skill_name);

    if !user_path.exists() {
        // Check if it exists as a built-in
        if builtin_path.exists() {
            return Err(ExtensionError::BuiltinSkillDeletion(skill_name.to_string()));
        }
        return Err(ExtensionError::SkillNotFound(skill_name.to_string()));
    }

    if user_path.is_symlink() || user_path.is_file() {
        tokio::fs::remove_file(&user_path).await?;
    } else {
        tokio::fs::remove_dir_all(&user_path).await?;
    }

    debug!(skill = %skill_name, "skill deleted");
    Ok(())
}

// ---------------------------------------------------------------------------
// E. Scanning & discovery
// ---------------------------------------------------------------------------

/// Scan a directory for subdirectories containing SKILL.md.
pub async fn scan_for_skills(folder_path: &Path) -> Result<Vec<ScannedSkill>, ExtensionError> {
    scan_skill_dirs(folder_path).await
}

/// Named filesystem path.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedPath {
    pub name: String,
    pub path: String,
}

/// Detect common skill paths relative to the user's home directory.
///
/// Returns paths that actually exist on the filesystem.
pub async fn detect_common_skill_paths() -> Vec<NamedPath> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    let mut result = Vec::new();
    for (name, rel_path) in COMMON_SKILL_DIRS {
        let full_path = home.join(rel_path);
        if full_path.exists() {
            result.push(NamedPath {
                name: (*name).to_string(),
                path: full_path.to_string_lossy().into_owned(),
            });
        }
    }

    result
}

/// An external skill source with discovered skills.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalSkillSource {
    pub name: String,
    pub path: String,
    pub skill_count: usize,
    pub skills: Vec<ScannedSkill>,
}

/// Discover external skills from common paths and custom external paths.
pub async fn detect_and_count_external_skills(
    custom_paths: &[NamedPath],
) -> Vec<ExternalSkillSource> {
    let mut sources = Vec::new();

    // 1. Common paths
    let common_paths = detect_common_skill_paths().await;
    for np in &common_paths {
        let path = Path::new(&np.path);
        if let Ok(skills) = scan_skill_dirs(path).await {
            sources.push(ExternalSkillSource {
                name: np.name.clone(),
                path: np.path.clone(),
                skill_count: skills.len(),
                skills,
            });
        }
    }

    // 2. Custom external paths
    for np in custom_paths {
        let path = Path::new(&np.path);
        if let Ok(skills) = scan_skill_dirs(path).await {
            sources.push(ExternalSkillSource {
                name: np.name.clone(),
                path: np.path.clone(),
                skill_count: skills.len(),
                skills,
            });
        }
    }

    sources
}

/// Get the user and built-in skill directory paths.
pub fn get_skill_paths(paths: &SkillPaths) -> (String, String) {
    (
        paths.user_skills_dir.to_string_lossy().into_owned(),
        paths.builtin_skills_dir.to_string_lossy().into_owned(),
    )
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Read a file and return its content, or an empty string if it does not exist.
async fn read_file_or_empty(path: &Path) -> Result<String, ExtensionError> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Ok(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(ExtensionError::Io(e)),
    }
}

/// Validate a filename to prevent path traversal.
fn validate_filename(name: &str) -> Result<(), ExtensionError> {
    if name.contains('/') || name.contains('\\') || name.contains("..") || name.is_empty() {
        return Err(ExtensionError::PathTraversal(name.to_string()));
    }
    Ok(())
}

/// Read an assistant resource (rule or skill) with locale fallback.
async fn read_assistant_resource(
    dir: &Path,
    assistant_id: &str,
    locale: Option<&str>,
) -> Result<String, ExtensionError> {
    validate_filename(assistant_id)?;
    if let Some(loc) = locale {
        validate_filename(loc)?;
    }

    // 1. Try locale-specific file
    if let Some(loc) = locale
        && !loc.is_empty()
    {
        let locale_file = dir.join(format!("{assistant_id}.{loc}.md"));
        if let Ok(content) = tokio::fs::read_to_string(&locale_file).await {
            return Ok(content);
        }
    }

    // 2. Try default file (no locale suffix)
    let default_file = dir.join(format!("{assistant_id}.md"));
    match tokio::fs::read_to_string(&default_file).await {
        Ok(content) => Ok(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(ExtensionError::Io(e)),
    }
}

/// Write an assistant resource file.
async fn write_assistant_resource(
    dir: &Path,
    assistant_id: &str,
    content: &str,
    locale: Option<&str>,
) -> Result<bool, ExtensionError> {
    validate_filename(assistant_id)?;
    if let Some(loc) = locale {
        validate_filename(loc)?;
    }

    tokio::fs::create_dir_all(dir).await?;

    let filename = match locale {
        Some(loc) if !loc.is_empty() => format!("{assistant_id}.{loc}.md"),
        _ => format!("{assistant_id}.md"),
    };

    let file_path = dir.join(filename);
    tokio::fs::write(&file_path, content).await?;
    debug!(path = %file_path.display(), "assistant resource written");
    Ok(true)
}

/// Delete all files matching `{assistant_id}*.md` in a directory.
async fn delete_assistant_resource(dir: &Path, assistant_id: &str) -> Result<bool, ExtensionError> {
    validate_filename(assistant_id)?;

    let mut deleted_any = false;

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(ExtensionError::Io(e)),
    };

    let prefix = format!("{assistant_id}.");
    let exact = format!("{assistant_id}.md");

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == exact || (name.starts_with(&prefix) && name.ends_with(".md")) {
            tokio::fs::remove_file(entry.path()).await?;
            deleted_any = true;
            debug!(file = %name, "deleted assistant resource");
        }
    }

    Ok(deleted_any)
}

/// Scan a directory for subdirectories containing a SKILL.md file.
async fn scan_skill_dirs(dir: &Path) -> Result<Vec<ScannedSkill>, ExtensionError> {
    let mut result = Vec::new();

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(e) => return Err(ExtensionError::Io(e)),
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let entry_path = entry.path();
        if !entry_path.is_dir() {
            continue;
        }

        let skill_file = entry_path.join(SKILL_MANIFEST_FILE);
        if !skill_file.exists() {
            continue;
        }

        match tokio::fs::read_to_string(&skill_file).await {
            Ok(content) => {
                if let Some((name, description)) = parse_frontmatter_fields(&content) {
                    let final_name = if name.is_empty() {
                        entry_path
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    } else {
                        name
                    };
                    result.push(ScannedSkill {
                        name: final_name,
                        description,
                        path: entry_path.to_string_lossy().into_owned(),
                    });
                }
            }
            Err(e) => {
                warn!(
                    path = %skill_file.display(),
                    error = %e,
                    "failed to read SKILL.md"
                );
            }
        }
    }

    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

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
fn parse_frontmatter_fields(content: &str) -> Option<(String, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

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

/// Recursively copy a directory.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), ExtensionError> {
    tokio::fs::create_dir_all(dst).await?;

    let mut entries = tokio::fs::read_dir(src).await?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let entry_path = entry.path();
        let dest_path = dst.join(entry.file_name());

        if entry_path.is_dir() {
            Box::pin(copy_dir_recursive(&entry_path, &dest_path)).await?;
        } else {
            tokio::fs::copy(&entry_path, &dest_path).await?;
        }
    }

    Ok(())
}

/// Create a symlink (platform-aware).
#[cfg(unix)]
async fn create_symlink(src: &Path, dst: &Path) -> Result<(), ExtensionError> {
    tokio::fs::symlink(src, dst)
        .await
        .map_err(ExtensionError::Io)
}

#[cfg(windows)]
async fn create_symlink(src: &Path, dst: &Path) -> Result<(), ExtensionError> {
    // Use junction on Windows for directory symlinks
    if src.is_dir() {
        tokio::fs::symlink_dir(src, dst)
            .await
            .map_err(ExtensionError::Io)
    } else {
        tokio::fs::symlink_file(src, dst)
            .await
            .map_err(ExtensionError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // Frontmatter parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_frontmatter_valid() {
        let content = "---\nname: my-skill\ndescription: A useful skill\n---\nBody content here.";
        let (name, desc) = parse_frontmatter_fields(content).unwrap();
        assert_eq!(name, "my-skill");
        assert_eq!(desc, "A useful skill");
    }

    #[test]
    fn parse_frontmatter_empty_name() {
        let content = "---\nname: \ndescription: Has description\n---\nBody";
        let (name, desc) = parse_frontmatter_fields(content).unwrap();
        assert!(name.is_empty());
        assert_eq!(desc, "Has description");
    }

    #[test]
    fn parse_frontmatter_no_opening() {
        let content = "name: test\ndescription: desc\n---\nbody";
        assert!(parse_frontmatter_fields(content).is_none());
    }

    #[test]
    fn parse_frontmatter_no_closing() {
        let content = "---\nname: test\ndescription: desc";
        assert!(parse_frontmatter_fields(content).is_none());
    }

    #[test]
    fn parse_frontmatter_missing_description() {
        let content = "---\nname: test\n---\nbody";
        assert!(parse_frontmatter_fields(content).is_none());
    }

    // -----------------------------------------------------------------------
    // Filename validation
    // -----------------------------------------------------------------------

    #[test]
    fn validate_filename_normal() {
        assert!(validate_filename("code-review.md").is_ok());
    }

    #[test]
    fn validate_filename_path_traversal() {
        assert!(validate_filename("../etc/passwd").is_err());
        assert!(validate_filename("foo/bar.md").is_err());
        assert!(validate_filename("foo\\bar.md").is_err());
    }

    #[test]
    fn validate_filename_empty() {
        assert!(validate_filename("").is_err());
    }

    // -----------------------------------------------------------------------
    // Built-in resource reading
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn read_builtin_rule_existing_file() {
        let tmp = TempDir::new().unwrap();
        let rules_dir = tmp.path().join(BUILTIN_RULES_DIR_NAME);
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(rules_dir.join("code-review.md"), "# Review rules").unwrap();

        let paths = SkillPaths {
            data_dir: tmp.path().to_path_buf(),
            user_skills_dir: tmp.path().join(SKILLS_DIR_NAME),
            builtin_skills_dir: tmp.path().join(BUILTIN_SKILLS_DIR_NAME),
            builtin_rules_dir: rules_dir,
            assistant_rules_dir: tmp.path().join(ASSISTANT_RULES_DIR_NAME),
            assistant_skills_dir: tmp.path().join(ASSISTANT_SKILLS_DIR_NAME),
        };

        let content = read_builtin_rule(&paths, "code-review.md").await.unwrap();
        assert_eq!(content, "# Review rules");
    }

    #[tokio::test]
    async fn read_builtin_rule_missing_file() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        let content = read_builtin_rule(&paths, "nonexistent.md").await.unwrap();
        assert!(content.is_empty());
    }

    #[tokio::test]
    async fn read_builtin_rule_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        let result = read_builtin_rule(&paths, "../secret.md").await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Assistant CRUD
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn assistant_rule_write_and_read() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        write_assistant_rule(&paths, "abc123", "Be helpful.", None)
            .await
            .unwrap();

        let content = read_assistant_rule(&paths, "abc123", None).await.unwrap();
        assert_eq!(content, "Be helpful.");
    }

    #[tokio::test]
    async fn assistant_rule_locale_fallback() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        // Write default (no locale)
        write_assistant_rule(&paths, "abc123", "Default content", None)
            .await
            .unwrap();

        // Write zh-CN locale
        write_assistant_rule(&paths, "abc123", "中文内容", Some("zh-CN"))
            .await
            .unwrap();

        // Read with matching locale
        let content = read_assistant_rule(&paths, "abc123", Some("zh-CN"))
            .await
            .unwrap();
        assert_eq!(content, "中文内容");

        // Read with non-matching locale → falls back to default
        let content = read_assistant_rule(&paths, "abc123", Some("ja-JP"))
            .await
            .unwrap();
        assert_eq!(content, "Default content");

        // Read without locale → default
        let content = read_assistant_rule(&paths, "abc123", None).await.unwrap();
        assert_eq!(content, "Default content");
    }

    #[tokio::test]
    async fn assistant_rule_read_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        let content = read_assistant_rule(&paths, "missing", None).await.unwrap();
        assert!(content.is_empty());
    }

    #[tokio::test]
    async fn assistant_rule_delete_all_locales() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        write_assistant_rule(&paths, "abc123", "Default", None)
            .await
            .unwrap();
        write_assistant_rule(&paths, "abc123", "Chinese", Some("zh-CN"))
            .await
            .unwrap();
        write_assistant_rule(&paths, "abc123", "English", Some("en-US"))
            .await
            .unwrap();

        let deleted = delete_assistant_rule(&paths, "abc123").await.unwrap();
        assert!(deleted);

        // Verify all files are gone
        let content = read_assistant_rule(&paths, "abc123", None).await.unwrap();
        assert!(content.is_empty());
        let content = read_assistant_rule(&paths, "abc123", Some("zh-CN"))
            .await
            .unwrap();
        assert!(content.is_empty());
    }

    #[tokio::test]
    async fn assistant_skill_write_and_read() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        write_assistant_skill(&paths, "abc123", "Skill content", None)
            .await
            .unwrap();

        let content = read_assistant_skill(&paths, "abc123", None).await.unwrap();
        assert_eq!(content, "Skill content");
    }

    // -----------------------------------------------------------------------
    // Assistant CRUD — path traversal prevention
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn read_assistant_rule_rejects_traversal_id() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());
        let result = read_assistant_rule(&paths, "../etc/passwd", None).await;
        assert!(matches!(result, Err(ExtensionError::PathTraversal(_))));
    }

    #[tokio::test]
    async fn read_assistant_rule_rejects_traversal_locale() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());
        let result = read_assistant_rule(&paths, "valid-id", Some("../evil")).await;
        assert!(matches!(result, Err(ExtensionError::PathTraversal(_))));
    }

    #[tokio::test]
    async fn write_assistant_rule_rejects_traversal_id() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());
        let result = write_assistant_rule(&paths, "../../escape", "content", None).await;
        assert!(matches!(result, Err(ExtensionError::PathTraversal(_))));
    }

    #[tokio::test]
    async fn write_assistant_rule_rejects_traversal_locale() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());
        let result = write_assistant_rule(&paths, "valid-id", "content", Some("../bad")).await;
        assert!(matches!(result, Err(ExtensionError::PathTraversal(_))));
    }

    #[tokio::test]
    async fn delete_assistant_rule_rejects_traversal_id() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());
        let result = delete_assistant_rule(&paths, "foo/bar").await;
        assert!(matches!(result, Err(ExtensionError::PathTraversal(_))));
    }

    #[tokio::test]
    async fn read_assistant_skill_rejects_traversal_id() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());
        let result = read_assistant_skill(&paths, "..\\windows", None).await;
        assert!(matches!(result, Err(ExtensionError::PathTraversal(_))));
    }

    #[tokio::test]
    async fn write_assistant_skill_rejects_traversal_id() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());
        let result = write_assistant_skill(&paths, "../escape", "content", None).await;
        assert!(matches!(result, Err(ExtensionError::PathTraversal(_))));
    }

    #[tokio::test]
    async fn delete_assistant_skill_rejects_traversal_id() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());
        let result = delete_assistant_skill(&paths, "a/b").await;
        assert!(matches!(result, Err(ExtensionError::PathTraversal(_))));
    }

    // -----------------------------------------------------------------------
    // Skill listing
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn list_skills_builtin_and_custom() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        // Create builtin skills
        create_skill_in_dir(&paths.builtin_skills_dir, "review", "Code review skill");
        create_skill_in_dir(&paths.builtin_skills_dir, "debug", "Debugging skill");

        // Create custom skill (overrides review)
        create_skill_in_dir(&paths.user_skills_dir, "review", "Custom review skill");
        create_skill_in_dir(&paths.user_skills_dir, "my-skill", "My custom skill");

        let skills = list_available_skills(&paths).await.unwrap();

        assert_eq!(skills.len(), 3); // debug + review (custom) + my-skill

        let review = skills.iter().find(|s| s.name == "review").unwrap();
        assert!(review.is_custom);
        assert_eq!(review.description, "Custom review skill");
        assert_eq!(review.source, SkillSource::Custom);

        let debug_skill = skills.iter().find(|s| s.name == "debug").unwrap();
        assert!(!debug_skill.is_custom);
        assert_eq!(debug_skill.source, SkillSource::Builtin);

        let my_skill = skills.iter().find(|s| s.name == "my-skill").unwrap();
        assert_eq!(my_skill.source, SkillSource::Custom);
    }

    #[tokio::test]
    async fn list_skills_empty_dirs() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        let skills = list_available_skills(&paths).await.unwrap();
        assert!(skills.is_empty());
    }

    // -----------------------------------------------------------------------
    // Skill info
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn read_skill_info_valid() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join(SKILL_MANIFEST_FILE),
            "---\nname: my-skill\ndescription: A test skill\n---\nBody",
        )
        .unwrap();

        let (name, desc) = read_skill_info(&skill_dir).await.unwrap();
        assert_eq!(name, "my-skill");
        assert_eq!(desc, "A test skill");
    }

    #[tokio::test]
    async fn read_skill_info_missing() {
        let tmp = TempDir::new().unwrap();
        let result = read_skill_info(&tmp.path().join("nonexistent")).await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Skill import / delete
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn import_skill_copies_directory() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        // Create source skill
        let source_dir = tmp.path().join("source-skill");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(
            source_dir.join(SKILL_MANIFEST_FILE),
            "---\nname: imported\ndescription: Imported skill\n---\nBody",
        )
        .unwrap();
        std::fs::write(source_dir.join("extra.txt"), "extra data").unwrap();

        let name = import_skill(&paths, &source_dir).await.unwrap();
        assert_eq!(name, "imported");

        // Verify the skill was copied
        let imported_dir = paths.user_skills_dir.join("imported");
        assert!(imported_dir.join(SKILL_MANIFEST_FILE).exists());
        assert!(imported_dir.join("extra.txt").exists());
    }

    #[tokio::test]
    async fn import_skill_with_symlink_creates_link() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        let source_dir = tmp.path().join("link-skill");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(
            source_dir.join(SKILL_MANIFEST_FILE),
            "---\nname: linked\ndescription: Linked skill\n---\nBody",
        )
        .unwrap();

        let name = import_skill_with_symlink(&paths, &source_dir)
            .await
            .unwrap();
        assert_eq!(name, "linked");

        let link_path = paths.user_skills_dir.join("linked");
        assert!(link_path.is_symlink());
    }

    #[tokio::test]
    async fn import_skill_rejects_traversal_name() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        // Create a skill whose frontmatter name contains path traversal
        let source_dir = tmp.path().join("evil-skill");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(
            source_dir.join(SKILL_MANIFEST_FILE),
            "---\nname: ../../../etc/evil\ndescription: Malicious skill\n---\nBody",
        )
        .unwrap();

        let result = import_skill(&paths, &source_dir).await;
        assert!(matches!(result, Err(ExtensionError::PathTraversal(_))));
    }

    #[tokio::test]
    async fn import_skill_with_symlink_rejects_traversal_name() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        let source_dir = tmp.path().join("evil-skill");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(
            source_dir.join(SKILL_MANIFEST_FILE),
            "---\nname: ../../escape\ndescription: Malicious skill\n---\nBody",
        )
        .unwrap();

        let result = import_skill_with_symlink(&paths, &source_dir).await;
        assert!(matches!(result, Err(ExtensionError::PathTraversal(_))));
    }

    #[tokio::test]
    async fn delete_custom_skill() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        create_skill_in_dir(&paths.user_skills_dir, "to-delete", "Will be deleted");

        delete_skill(&paths, "to-delete").await.unwrap();
        assert!(!paths.user_skills_dir.join("to-delete").exists());
    }

    #[tokio::test]
    async fn delete_builtin_skill_rejected() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        create_skill_in_dir(&paths.builtin_skills_dir, "protected", "Built-in skill");

        let result = delete_skill(&paths, "protected").await;
        assert!(matches!(
            result,
            Err(ExtensionError::BuiltinSkillDeletion(_))
        ));
    }

    #[tokio::test]
    async fn delete_nonexistent_skill() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        let result = delete_skill(&paths, "ghost").await;
        assert!(matches!(result, Err(ExtensionError::SkillNotFound(_))));
    }

    #[tokio::test]
    async fn delete_skill_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let paths = make_test_paths(tmp.path());

        let result = delete_skill(&paths, "../etc").await;
        assert!(matches!(result, Err(ExtensionError::PathTraversal(_))));
    }

    // -----------------------------------------------------------------------
    // Scanning
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn scan_for_skills_finds_valid() {
        let tmp = TempDir::new().unwrap();
        create_skill_in_dir(tmp.path(), "skill-a", "First skill");
        create_skill_in_dir(tmp.path(), "skill-b", "Second skill");

        // Create a dir without SKILL.md (should be ignored)
        std::fs::create_dir_all(tmp.path().join("not-a-skill")).unwrap();

        let skills = scan_for_skills(tmp.path()).await.unwrap();
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "skill-a");
        assert_eq!(skills[1].name, "skill-b");
    }

    #[tokio::test]
    async fn scan_for_skills_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let skills = scan_for_skills(tmp.path()).await.unwrap();
        assert!(skills.is_empty());
    }

    #[tokio::test]
    async fn scan_for_skills_nonexistent_dir() {
        let skills = scan_for_skills(Path::new("/nonexistent/path"))
            .await
            .unwrap();
        assert!(skills.is_empty());
    }

    // -----------------------------------------------------------------------
    // Export
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn export_skill_creates_symlink() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(
            source_dir.join(SKILL_MANIFEST_FILE),
            "---\nname: my-skill\ndescription: Test\n---\nBody",
        )
        .unwrap();

        let target_dir = tmp.path().join("exports");
        export_skill_with_symlink(&source_dir, &target_dir)
            .await
            .unwrap();

        let link = target_dir.join("my-skill");
        assert!(link.is_symlink());
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn make_test_paths(base: &Path) -> SkillPaths {
        SkillPaths {
            data_dir: base.to_path_buf(),
            user_skills_dir: base.join(SKILLS_DIR_NAME),
            builtin_skills_dir: base.join(BUILTIN_SKILLS_DIR_NAME),
            builtin_rules_dir: base.join(BUILTIN_RULES_DIR_NAME),
            assistant_rules_dir: base.join(ASSISTANT_RULES_DIR_NAME),
            assistant_skills_dir: base.join(ASSISTANT_SKILLS_DIR_NAME),
        }
    }

    fn create_skill_in_dir(base: &Path, name: &str, description: &str) {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(SKILL_MANIFEST_FILE),
            format!("---\nname: {name}\ndescription: {description}\n---\nBody content for {name}."),
        )
        .unwrap();
    }
}
