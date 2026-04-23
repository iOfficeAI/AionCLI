//! Assistant service — three-source merge, CRUD, state overrides, import,
//! and source-dispatched rule/skill read/write helpers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aionui_api_types::{
    AssistantResponse, AssistantSource, CreateAssistantRequest, ImportAssistantsRequest,
    ImportAssistantsResult, ImportError, SetAssistantStateRequest, UpdateAssistantRequest,
};
use aionui_common::{AppError, now_ms};
use aionui_db::{
    AssistantOverrideRow, AssistantRow, CreateAssistantParams, IAssistantOverrideRepository,
    IAssistantRepository, UpdateAssistantParams, UpsertOverrideParams,
};
use aionui_extension::{
    AssistantClassifier, AssistantRuleDispatcher, ExtensionRegistry, ResolvedAssistant,
};
use serde_json;
use tracing::{debug, warn};

use crate::builtin::{BuiltinAssistant, BuiltinAssistantRegistry};

/// Aggregated business logic for `/api/assistants/*` and rule/skill dispatch.
pub struct AssistantService {
    repo: Arc<dyn IAssistantRepository>,
    override_repo: Arc<dyn IAssistantOverrideRepository>,
    builtin: Arc<BuiltinAssistantRegistry>,
    extension_registry: ExtensionRegistry,
    /// Root directory holding user-authored rule/skill md files and avatars.
    /// Defaults to `~/.aionui/` but can be overridden for tests.
    user_data_dir: PathBuf,
}

impl AssistantService {
    pub fn new(
        repo: Arc<dyn IAssistantRepository>,
        override_repo: Arc<dyn IAssistantOverrideRepository>,
        builtin: Arc<BuiltinAssistantRegistry>,
        extension_registry: ExtensionRegistry,
    ) -> Self {
        Self {
            repo,
            override_repo,
            builtin,
            extension_registry,
            user_data_dir: default_user_data_dir(),
        }
    }

    /// Override the user-data directory. Used by tests and callers that want
    /// to pin a custom root (e.g. `~/.aionui-dev/`).
    pub fn with_user_data_dir(mut self, dir: PathBuf) -> Self {
        self.user_data_dir = dir;
        self
    }

    // -----------------------------------------------------------------------
    // Classification
    // -----------------------------------------------------------------------

    /// Classify an assistant id into its source.
    pub async fn classify_source(&self, id: &str) -> AssistantSource {
        if self.builtin.has(id) {
            return AssistantSource::Builtin;
        }
        if self.extension_registry.has_assistant(id).await {
            return AssistantSource::Extension;
        }
        AssistantSource::User
    }

    // -----------------------------------------------------------------------
    // List / Get
    // -----------------------------------------------------------------------

    /// Three-source merge (built-in + user + extension) with per-assistant
    /// override application. Also performs opportunistic orphan cleanup on
    /// the overrides table.
    pub async fn list(&self) -> Result<Vec<AssistantResponse>, AppError> {
        let user_rows = self.repo.list().await?;
        let extensions = self.extension_registry.get_assistants().await;
        let overrides = self.override_repo.get_all().await?;

        let overrides_map: HashMap<String, AssistantOverrideRow> = overrides
            .into_iter()
            .map(|o| (o.assistant_id.clone(), o))
            .collect();

        let mut result = Vec::new();

        for b in self.builtin.all() {
            result.push(builtin_to_response(b, overrides_map.get(&b.id)));
        }
        for u in &user_rows {
            result.push(user_row_to_response(u, overrides_map.get(&u.id))?);
        }
        for e in &extensions {
            result.push(extension_to_response(e));
        }

        // Sort by sort_order asc, then last_used_at desc (newer first).
        result.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| b.last_used_at.cmp(&a.last_used_at))
        });

        // Opportunistic orphan cleanup: any override row whose assistant_id no
        // longer appears in the merged list is stale.
        let valid_ids: Vec<&str> = result.iter().map(|a| a.id.as_str()).collect();
        if let Err(e) = self.override_repo.delete_orphans(&valid_ids).await {
            warn!("override orphan cleanup failed: {e}");
        }

        Ok(result)
    }

    pub async fn get(&self, id: &str) -> Result<AssistantResponse, AppError> {
        match self.classify_source(id).await {
            AssistantSource::Builtin => {
                let b = self
                    .builtin
                    .get(id)
                    .ok_or_else(|| AppError::NotFound(format!("assistant '{id}' not found")))?;
                let ov = self.override_repo.get(id).await?;
                Ok(builtin_to_response(b, ov.as_ref()))
            }
            AssistantSource::Extension => {
                let e = self
                    .extension_registry
                    .get_assistant_by_id(id)
                    .await
                    .ok_or_else(|| AppError::NotFound(format!("assistant '{id}' not found")))?;
                Ok(extension_to_response(&e))
            }
            AssistantSource::User => {
                let row = self
                    .repo
                    .get(id)
                    .await?
                    .ok_or_else(|| AppError::NotFound(format!("assistant '{id}' not found")))?;
                let ov = self.override_repo.get(id).await?;
                user_row_to_response(&row, ov.as_ref())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Create / Update / Delete
    // -----------------------------------------------------------------------

    pub async fn create(&self, req: CreateAssistantRequest) -> Result<AssistantResponse, AppError> {
        let name = req.name.trim().to_string();
        if name.is_empty() {
            return Err(AppError::BadRequest("name is required".into()));
        }

        let id = match req.id.as_deref() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => generate_user_id(),
        };

        // Reject id collisions with built-in / extension-contributed.
        if self.builtin.has(&id) {
            return Err(AppError::BadRequest(
                "Id conflicts with built-in assistant".into(),
            ));
        }
        if self.extension_registry.has_assistant(&id).await {
            return Err(AppError::BadRequest(
                "Id conflicts with extension-contributed assistant".into(),
            ));
        }

        let serialized = SerializedFields::from_create(&req)?;
        let params = CreateAssistantParams {
            id: &id,
            name: &name,
            description: req.description.as_deref(),
            avatar: req.avatar.as_deref(),
            preset_agent_type: req
                .preset_agent_type
                .as_deref()
                .unwrap_or(DEFAULT_AGENT_TYPE),
            enabled_skills: serialized.enabled_skills.as_deref(),
            custom_skill_names: serialized.custom_skill_names.as_deref(),
            disabled_builtin_skills: serialized.disabled_builtin_skills.as_deref(),
            prompts: serialized.prompts.as_deref(),
            models: serialized.models.as_deref(),
            name_i18n: serialized.name_i18n.as_deref(),
            description_i18n: serialized.description_i18n.as_deref(),
            prompts_i18n: serialized.prompts_i18n.as_deref(),
        };

        let row = self.repo.create(&params).await?;
        let ov = self.override_repo.get(&id).await?;
        user_row_to_response(&row, ov.as_ref())
    }

    pub async fn update(
        &self,
        id: &str,
        req: UpdateAssistantRequest,
    ) -> Result<AssistantResponse, AppError> {
        match self.classify_source(id).await {
            AssistantSource::Builtin => {
                return Err(AppError::Forbidden(
                    "Cannot modify built-in assistant".into(),
                ));
            }
            AssistantSource::Extension => {
                return Err(AppError::Forbidden(
                    "Cannot modify extension-contributed assistant".into(),
                ));
            }
            AssistantSource::User => {}
        }

        let serialized = SerializedFields::from_update(&req)?;
        let params = UpdateAssistantParams {
            name: req.name.as_deref(),
            description: req.description.as_ref().map(|s| Some(s.as_str())),
            avatar: req.avatar.as_ref().map(|s| Some(s.as_str())),
            preset_agent_type: req.preset_agent_type.as_deref(),
            enabled_skills: serialized.enabled_skills.as_ref().map(|s| Some(s.as_str())),
            custom_skill_names: serialized
                .custom_skill_names
                .as_ref()
                .map(|s| Some(s.as_str())),
            disabled_builtin_skills: serialized
                .disabled_builtin_skills
                .as_ref()
                .map(|s| Some(s.as_str())),
            prompts: serialized.prompts.as_ref().map(|s| Some(s.as_str())),
            models: serialized.models.as_ref().map(|s| Some(s.as_str())),
            name_i18n: serialized.name_i18n.as_ref().map(|s| Some(s.as_str())),
            description_i18n: serialized
                .description_i18n
                .as_ref()
                .map(|s| Some(s.as_str())),
            prompts_i18n: serialized.prompts_i18n.as_ref().map(|s| Some(s.as_str())),
        };

        let row = self
            .repo
            .update(id, &params)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("assistant '{id}' not found")))?;
        let ov = self.override_repo.get(id).await?;
        user_row_to_response(&row, ov.as_ref())
    }

    pub async fn delete(&self, id: &str) -> Result<(), AppError> {
        match self.classify_source(id).await {
            AssistantSource::Builtin => {
                return Err(AppError::Forbidden(
                    "Cannot delete built-in assistant".into(),
                ));
            }
            AssistantSource::Extension => {
                return Err(AppError::Forbidden(
                    "Cannot delete extension-contributed assistant".into(),
                ));
            }
            AssistantSource::User => {}
        }

        let removed = self.repo.delete(id).await?;
        if !removed {
            return Err(AppError::NotFound(format!("assistant '{id}' not found")));
        }

        // Drop the override row (best-effort).
        if let Err(e) = self.override_repo.delete(id).await {
            warn!("failed to remove override for deleted assistant '{id}': {e}");
        }

        // Best-effort filesystem cleanup.
        self.cleanup_user_assets(id);

        Ok(())
    }

    pub async fn set_state(
        &self,
        id: &str,
        req: SetAssistantStateRequest,
    ) -> Result<AssistantResponse, AppError> {
        match self.classify_source(id).await {
            AssistantSource::Extension => {
                return Err(AppError::BadRequest(
                    "Extension assistants are read-only".into(),
                ));
            }
            AssistantSource::Builtin => {}
            AssistantSource::User => {
                // Confirm the user row exists (otherwise 404).
                if self.repo.get(id).await?.is_none() {
                    return Err(AppError::NotFound(format!("assistant '{id}' not found")));
                }
            }
        }

        // Merge with existing override to preserve fields not in this request.
        let existing = self.override_repo.get(id).await?;
        let enabled = req
            .enabled
            .unwrap_or_else(|| existing.as_ref().is_none_or(|o| o.enabled));
        let sort_order = req
            .sort_order
            .or_else(|| existing.as_ref().map(|o| o.sort_order))
            .unwrap_or(0);
        let last_used_at = req
            .last_used_at
            .or_else(|| existing.as_ref().and_then(|o| o.last_used_at));

        let params = UpsertOverrideParams {
            assistant_id: id,
            enabled,
            sort_order,
            last_used_at,
        };
        self.override_repo.upsert(&params).await?;

        self.get(id).await
    }

    // -----------------------------------------------------------------------
    // Import (insert-only, idempotent)
    // -----------------------------------------------------------------------

    /// Bulk insert-only import of legacy Electron config rows. Skip on
    /// built-in / extension id collision or already-imported user-id collision.
    /// Never overwrites an existing user row.
    pub async fn import(
        &self,
        req: ImportAssistantsRequest,
    ) -> Result<ImportAssistantsResult, AppError> {
        let mut result = ImportAssistantsResult::default();

        for entry in req.assistants {
            let id = entry
                .id
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(generate_user_id);

            if self.builtin.has(&id) {
                result.skipped += 1;
                continue;
            }
            if self.extension_registry.has_assistant(&id).await {
                result.skipped += 1;
                continue;
            }
            match self.repo.get(&id).await {
                Ok(Some(_)) => {
                    result.skipped += 1;
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    result.failed += 1;
                    result.errors.push(ImportError {
                        id: id.clone(),
                        error: e.to_string(),
                    });
                    continue;
                }
            }

            let name = entry.name.trim().to_string();
            if name.is_empty() {
                result.failed += 1;
                result.errors.push(ImportError {
                    id,
                    error: "name is required".into(),
                });
                continue;
            }

            let serialized = match SerializedFields::from_create(&entry) {
                Ok(s) => s,
                Err(e) => {
                    result.failed += 1;
                    result.errors.push(ImportError {
                        id,
                        error: e.to_string(),
                    });
                    continue;
                }
            };

            let params = CreateAssistantParams {
                id: &id,
                name: &name,
                description: entry.description.as_deref(),
                avatar: entry.avatar.as_deref(),
                preset_agent_type: entry
                    .preset_agent_type
                    .as_deref()
                    .unwrap_or(DEFAULT_AGENT_TYPE),
                enabled_skills: serialized.enabled_skills.as_deref(),
                custom_skill_names: serialized.custom_skill_names.as_deref(),
                disabled_builtin_skills: serialized.disabled_builtin_skills.as_deref(),
                prompts: serialized.prompts.as_deref(),
                models: serialized.models.as_deref(),
                name_i18n: serialized.name_i18n.as_deref(),
                description_i18n: serialized.description_i18n.as_deref(),
                prompts_i18n: serialized.prompts_i18n.as_deref(),
            };

            match self.repo.create(&params).await {
                Ok(_) => result.imported += 1,
                Err(aionui_db::DbError::Conflict(_)) => {
                    // Someone raced us into the table — treat as skip to
                    // keep import idempotent across retries.
                    result.skipped += 1;
                }
                Err(e) => {
                    result.failed += 1;
                    result.errors.push(ImportError {
                        id,
                        error: e.to_string(),
                    });
                }
            }
        }

        Ok(result)
    }

    // -----------------------------------------------------------------------
    // Rule / skill dispatch helpers
    // -----------------------------------------------------------------------

    /// Read an assistant rule file, dispatching by source.
    pub async fn read_rule(&self, id: &str, locale: Option<&str>) -> Result<String, AppError> {
        match self.classify_source(id).await {
            AssistantSource::Builtin => {
                let locale = locale.unwrap_or("");
                if let Some(path) = self.builtin.rule_path(id, locale) {
                    return Ok(read_file_or_empty(&path));
                }
                Ok(String::new())
            }
            AssistantSource::Extension => {
                // ResolvedAssistant doesn't expose rule content directly in
                // the current backend; return empty until extension schema
                // gains this field. Callers see empty == "no rule".
                Ok(String::new())
            }
            AssistantSource::User => {
                let path = self.user_rule_path(id, locale);
                Ok(read_file_or_empty(&path))
            }
        }
    }

    /// Write an assistant rule file. User only; built-in / extension reject.
    pub async fn write_rule(
        &self,
        id: &str,
        locale: Option<&str>,
        content: &str,
    ) -> Result<(), AppError> {
        match self.classify_source(id).await {
            AssistantSource::Builtin => Err(AppError::BadRequest(
                "Cannot write rule for built-in assistant".into(),
            )),
            AssistantSource::Extension => Err(AppError::BadRequest(
                "Cannot write rule for extension-contributed assistant".into(),
            )),
            AssistantSource::User => {
                let path = self.user_rule_path(id, locale);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| AppError::Internal(format!("create dir failed: {e}")))?;
                }
                std::fs::write(&path, content)
                    .map_err(|e| AppError::Internal(format!("write failed: {e}")))?;
                Ok(())
            }
        }
    }

    /// Delete all locale versions of an assistant rule. User only.
    pub async fn delete_rule(&self, id: &str) -> Result<bool, AppError> {
        match self.classify_source(id).await {
            AssistantSource::Builtin => Err(AppError::BadRequest(
                "Cannot delete rule for built-in assistant".into(),
            )),
            AssistantSource::Extension => Err(AppError::BadRequest(
                "Cannot delete rule for extension-contributed assistant".into(),
            )),
            AssistantSource::User => Ok(remove_assistant_md_files(&self.user_rules_dir(), id)),
        }
    }

    pub async fn read_skill(&self, id: &str, locale: Option<&str>) -> Result<String, AppError> {
        match self.classify_source(id).await {
            AssistantSource::Builtin => {
                let locale = locale.unwrap_or("");
                if let Some(path) = self.builtin.skill_path(id, locale) {
                    return Ok(read_file_or_empty(&path));
                }
                Ok(String::new())
            }
            AssistantSource::Extension => Ok(String::new()),
            AssistantSource::User => {
                let path = self.user_skill_path(id, locale);
                Ok(read_file_or_empty(&path))
            }
        }
    }

    pub async fn write_skill(
        &self,
        id: &str,
        locale: Option<&str>,
        content: &str,
    ) -> Result<(), AppError> {
        match self.classify_source(id).await {
            AssistantSource::Builtin => Err(AppError::BadRequest(
                "Cannot write skill for built-in assistant".into(),
            )),
            AssistantSource::Extension => Err(AppError::BadRequest(
                "Cannot write skill for extension-contributed assistant".into(),
            )),
            AssistantSource::User => {
                let path = self.user_skill_path(id, locale);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| AppError::Internal(format!("create dir failed: {e}")))?;
                }
                std::fs::write(&path, content)
                    .map_err(|e| AppError::Internal(format!("write failed: {e}")))?;
                Ok(())
            }
        }
    }

    pub async fn delete_skill(&self, id: &str) -> Result<bool, AppError> {
        match self.classify_source(id).await {
            AssistantSource::Builtin => Err(AppError::BadRequest(
                "Cannot delete skill for built-in assistant".into(),
            )),
            AssistantSource::Extension => Err(AppError::BadRequest(
                "Cannot delete skill for extension-contributed assistant".into(),
            )),
            AssistantSource::User => Ok(remove_assistant_md_files(&self.user_skills_dir(), id)),
        }
    }

    // -----------------------------------------------------------------------
    // Avatar helpers
    // -----------------------------------------------------------------------

    /// Resolve the on-disk avatar path for an assistant. Extensions return
    /// `None` here — the frontend serves them via `aion-asset://`.
    pub async fn avatar_path(&self, id: &str) -> Option<PathBuf> {
        match self.classify_source(id).await {
            AssistantSource::Builtin => self.builtin.avatar_path(id),
            AssistantSource::Extension => None,
            AssistantSource::User => {
                let dir = self.user_avatars_dir();
                // Return whatever file exists with the id prefix.
                let entries = std::fs::read_dir(&dir).ok()?;
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if let Some(stem) = name.split('.').next()
                        && stem == id
                    {
                        return Some(entry.path());
                    }
                }
                None
            }
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn user_rules_dir(&self) -> PathBuf {
        self.user_data_dir.join("assistant-rules")
    }

    fn user_skills_dir(&self) -> PathBuf {
        self.user_data_dir.join("assistant-skills")
    }

    fn user_avatars_dir(&self) -> PathBuf {
        self.user_data_dir.join("assistant-avatars")
    }

    fn user_rule_path(&self, id: &str, locale: Option<&str>) -> PathBuf {
        assistant_md_path(&self.user_rules_dir(), id, locale)
    }

    fn user_skill_path(&self, id: &str, locale: Option<&str>) -> PathBuf {
        assistant_md_path(&self.user_skills_dir(), id, locale)
    }

    fn cleanup_user_assets(&self, id: &str) {
        remove_assistant_md_files(&self.user_rules_dir(), id);
        remove_assistant_md_files(&self.user_skills_dir(), id);
        remove_assistant_avatar_files(&self.user_avatars_dir(), id);
    }
}

#[async_trait::async_trait]
impl AssistantClassifier for AssistantService {
    async fn classify(&self, id: &str) -> AssistantSource {
        self.classify_source(id).await
    }
}

#[async_trait::async_trait]
impl AssistantRuleDispatcher for AssistantService {
    async fn read_rule(&self, id: &str, locale: Option<&str>) -> Result<String, AppError> {
        AssistantService::read_rule(self, id, locale).await
    }

    async fn write_rule(
        &self,
        id: &str,
        locale: Option<&str>,
        content: &str,
    ) -> Result<(), AppError> {
        AssistantService::write_rule(self, id, locale, content).await
    }

    async fn delete_rule(&self, id: &str) -> Result<bool, AppError> {
        AssistantService::delete_rule(self, id).await
    }

    async fn read_skill(&self, id: &str, locale: Option<&str>) -> Result<String, AppError> {
        AssistantService::read_skill(self, id, locale).await
    }

    async fn write_skill(
        &self,
        id: &str,
        locale: Option<&str>,
        content: &str,
    ) -> Result<(), AppError> {
        AssistantService::write_skill(self, id, locale, content).await
    }

    async fn delete_skill(&self, id: &str) -> Result<bool, AppError> {
        AssistantService::delete_skill(self, id).await
    }
}

// ---------------------------------------------------------------------------
// Response conversion
// ---------------------------------------------------------------------------

const DEFAULT_AGENT_TYPE: &str = "gemini";

fn builtin_to_response(
    b: &BuiltinAssistant,
    ov: Option<&AssistantOverrideRow>,
) -> AssistantResponse {
    AssistantResponse {
        id: b.id.clone(),
        source: AssistantSource::Builtin,
        name: b.name.clone(),
        name_i18n: b.name_i18n.clone(),
        description: b.description.clone(),
        description_i18n: b.description_i18n.clone(),
        avatar: b.avatar.clone(),
        enabled: ov.map(|o| o.enabled).unwrap_or(true),
        sort_order: ov.map(|o| o.sort_order).unwrap_or(0),
        preset_agent_type: b.preset_agent_type.clone(),
        enabled_skills: b.enabled_skills.clone(),
        custom_skill_names: b.custom_skill_names.clone(),
        disabled_builtin_skills: b.disabled_builtin_skills.clone(),
        context: None,
        context_i18n: HashMap::new(),
        prompts: b.prompts.clone(),
        prompts_i18n: b.prompts_i18n.clone(),
        models: b.models.clone(),
        last_used_at: ov.and_then(|o| o.last_used_at),
    }
}

fn user_row_to_response(
    row: &AssistantRow,
    ov: Option<&AssistantOverrideRow>,
) -> Result<AssistantResponse, AppError> {
    Ok(AssistantResponse {
        id: row.id.clone(),
        source: AssistantSource::User,
        name: row.name.clone(),
        name_i18n: decode_str_map(row.name_i18n.as_deref())?,
        description: row.description.clone(),
        description_i18n: decode_str_map(row.description_i18n.as_deref())?,
        avatar: row.avatar.clone(),
        enabled: ov.map(|o| o.enabled).unwrap_or(true),
        sort_order: ov.map(|o| o.sort_order).unwrap_or(0),
        preset_agent_type: row.preset_agent_type.clone(),
        enabled_skills: decode_str_list(row.enabled_skills.as_deref())?,
        custom_skill_names: decode_str_list(row.custom_skill_names.as_deref())?,
        disabled_builtin_skills: decode_str_list(row.disabled_builtin_skills.as_deref())?,
        context: None,
        context_i18n: HashMap::new(),
        prompts: decode_str_list(row.prompts.as_deref())?,
        prompts_i18n: decode_list_map(row.prompts_i18n.as_deref())?,
        models: decode_str_list(row.models.as_deref())?,
        last_used_at: ov.and_then(|o| o.last_used_at),
    })
}

fn extension_to_response(e: &ResolvedAssistant) -> AssistantResponse {
    AssistantResponse {
        id: e.id.clone(),
        source: AssistantSource::Extension,
        name: e.name.clone(),
        name_i18n: HashMap::new(),
        description: e.description.clone(),
        description_i18n: HashMap::new(),
        avatar: e.icon.clone(),
        enabled: true,
        sort_order: 0,
        preset_agent_type: DEFAULT_AGENT_TYPE.to_string(),
        enabled_skills: Vec::new(),
        custom_skill_names: Vec::new(),
        disabled_builtin_skills: Vec::new(),
        context: e.context.clone(),
        context_i18n: HashMap::new(),
        prompts: Vec::new(),
        prompts_i18n: HashMap::new(),
        models: Vec::new(),
        last_used_at: None,
    }
}

// ---------------------------------------------------------------------------
// Serialization helpers
// ---------------------------------------------------------------------------

/// Serialized-JSON fragments for a single user-authored assistant row,
/// produced from either a create or update request.
struct SerializedFields {
    enabled_skills: Option<String>,
    custom_skill_names: Option<String>,
    disabled_builtin_skills: Option<String>,
    prompts: Option<String>,
    models: Option<String>,
    name_i18n: Option<String>,
    description_i18n: Option<String>,
    prompts_i18n: Option<String>,
}

impl SerializedFields {
    fn from_create(req: &CreateAssistantRequest) -> Result<Self, AppError> {
        Ok(Self {
            enabled_skills: encode_str_list(req.enabled_skills.as_deref())?,
            custom_skill_names: encode_str_list(req.custom_skill_names.as_deref())?,
            disabled_builtin_skills: encode_str_list(req.disabled_builtin_skills.as_deref())?,
            prompts: encode_str_list(req.prompts.as_deref())?,
            models: encode_str_list(req.models.as_deref())?,
            name_i18n: encode_str_map(req.name_i18n.as_ref())?,
            description_i18n: encode_str_map(req.description_i18n.as_ref())?,
            prompts_i18n: encode_list_map(req.prompts_i18n.as_ref())?,
        })
    }

    fn from_update(req: &UpdateAssistantRequest) -> Result<Self, AppError> {
        Ok(Self {
            enabled_skills: encode_str_list(req.enabled_skills.as_deref())?,
            custom_skill_names: encode_str_list(req.custom_skill_names.as_deref())?,
            disabled_builtin_skills: encode_str_list(req.disabled_builtin_skills.as_deref())?,
            prompts: encode_str_list(req.prompts.as_deref())?,
            models: encode_str_list(req.models.as_deref())?,
            name_i18n: encode_str_map(req.name_i18n.as_ref())?,
            description_i18n: encode_str_map(req.description_i18n.as_ref())?,
            prompts_i18n: encode_list_map(req.prompts_i18n.as_ref())?,
        })
    }
}

fn encode_str_list(value: Option<&[String]>) -> Result<Option<String>, AppError> {
    match value {
        Some(v) => {
            Ok(Some(serde_json::to_string(v).map_err(|e| {
                AppError::Internal(format!("encode list: {e}"))
            })?))
        }
        None => Ok(None),
    }
}

fn encode_str_map(value: Option<&HashMap<String, String>>) -> Result<Option<String>, AppError> {
    match value {
        Some(v) => {
            Ok(Some(serde_json::to_string(v).map_err(|e| {
                AppError::Internal(format!("encode map: {e}"))
            })?))
        }
        None => Ok(None),
    }
}

fn encode_list_map(
    value: Option<&HashMap<String, Vec<String>>>,
) -> Result<Option<String>, AppError> {
    match value {
        Some(v) => {
            Ok(Some(serde_json::to_string(v).map_err(|e| {
                AppError::Internal(format!("encode map: {e}"))
            })?))
        }
        None => Ok(None),
    }
}

fn decode_str_list(raw: Option<&str>) -> Result<Vec<String>, AppError> {
    match raw {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(s).map_err(|e| AppError::Internal(format!("decode list: {e}")))
        }
        _ => Ok(Vec::new()),
    }
}

fn decode_str_map(raw: Option<&str>) -> Result<HashMap<String, String>, AppError> {
    match raw {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(s).map_err(|e| AppError::Internal(format!("decode map: {e}")))
        }
        _ => Ok(HashMap::new()),
    }
}

fn decode_list_map(raw: Option<&str>) -> Result<HashMap<String, Vec<String>>, AppError> {
    match raw {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(s).map_err(|e| AppError::Internal(format!("decode map: {e}")))
        }
        _ => Ok(HashMap::new()),
    }
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

fn default_user_data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".aionui")
}

fn assistant_md_path(dir: &Path, id: &str, locale: Option<&str>) -> PathBuf {
    let filename = match locale {
        Some(loc) if !loc.is_empty() => format!("{id}.{loc}.md"),
        _ => format!("{id}.md"),
    };
    dir.join(filename)
}

fn read_file_or_empty(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Remove every `{id}*.md` file in `dir`. Returns `true` if any file was
/// deleted.
fn remove_assistant_md_files(dir: &Path, id: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut deleted = false;
    let prefix = format!("{id}.");
    let exact = format!("{id}.md");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == exact || (name.starts_with(&prefix) && name.ends_with(".md")) {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                warn!("failed to remove {}: {e}", entry.path().display());
                continue;
            }
            deleted = true;
        }
    }
    deleted
}

fn remove_assistant_avatar_files(dir: &Path, id: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut deleted = false;
    let prefix = format!("{id}.");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                warn!("failed to remove {}: {e}", entry.path().display());
                continue;
            }
            deleted = true;
        }
    }
    deleted
}

/// Generate a new user-authored assistant id with millisecond-resolution
/// timestamp + 4 hex chars of randomness.
pub fn generate_user_id() -> String {
    // Use time + a pseudo-random 16-bit value (sufficient for collision-free
    // ids within the same millisecond for any realistic UI workflow).
    let ms = now_ms();
    // Best-effort 16-bit random: hash the current nanos.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let hex = format!("{:04x}", (nanos as u16) ^ 0xA5A5);
    debug!("generated user assistant id: custom-{ms}-{hex}");
    format!("custom-{ms}-{hex}")
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_db::{
        SqliteAssistantOverrideRepository, SqliteAssistantRepository, init_database_memory,
    };
    use aionui_extension::ExtensionStateStore;
    use aionui_realtime::BroadcastEventBus;
    use tempfile::TempDir;

    struct Fixture {
        service: AssistantService,
        _tmp: TempDir,
        _db: aionui_db::Database,
    }

    async fn fixture() -> Fixture {
        fixture_with_builtins(vec![]).await
    }

    async fn fixture_with_builtins(builtins: Vec<BuiltinAssistant>) -> Fixture {
        let tmp = TempDir::new().unwrap();
        let db = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAssistantRepository> =
            Arc::new(SqliteAssistantRepository::new(db.pool().clone()));
        let orepo: Arc<dyn IAssistantOverrideRepository> =
            Arc::new(SqliteAssistantOverrideRepository::new(db.pool().clone()));

        // Write a manifest into a temp dir and load from it.
        let assets_dir = tmp.path().join("assets");
        std::fs::create_dir_all(&assets_dir).unwrap();
        let manifest_json = serde_json::json!({
            "version": "1.0.0",
            "assistants": builtins
                .iter()
                .map(|b| {
                    serde_json::json!({
                        "id": b.id,
                        "name": b.name,
                        "presetAgentType": b.preset_agent_type,
                        "ruleFile": b.rule_file,
                        "skillFile": b.skill_file,
                    })
                })
                .collect::<Vec<_>>()
        });
        std::fs::write(
            assets_dir.join("assistants.json"),
            serde_json::to_string(&manifest_json).unwrap(),
        )
        .unwrap();
        let builtin_reg = Arc::new(BuiltinAssistantRegistry::load_from_dir(assets_dir));

        let event_bus = Arc::new(BroadcastEventBus::new(8));
        let ext_state_store = ExtensionStateStore::new(tmp.path().join("ext-states.json"));
        let extension_registry =
            ExtensionRegistry::new(ext_state_store, event_bus, "1.0.0".to_string());

        let service = AssistantService::new(repo, orepo, builtin_reg, extension_registry)
            .with_user_data_dir(tmp.path().to_path_buf());

        Fixture {
            service,
            _tmp: tmp,
            _db: db,
        }
    }

    fn mk_builtin(id: &str, name: &str) -> BuiltinAssistant {
        BuiltinAssistant {
            id: id.into(),
            name: name.into(),
            name_i18n: HashMap::new(),
            description: None,
            description_i18n: HashMap::new(),
            avatar: None,
            preset_agent_type: "gemini".into(),
            enabled_skills: Vec::new(),
            custom_skill_names: Vec::new(),
            disabled_builtin_skills: Vec::new(),
            rule_file: None,
            skill_file: None,
            prompts: Vec::new(),
            prompts_i18n: HashMap::new(),
            models: Vec::new(),
        }
    }

    #[tokio::test]
    async fn list_empty_is_empty() {
        let fx = fixture().await;
        let list = fx.service.list().await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn list_includes_builtin_and_user() {
        let fx = fixture_with_builtins(vec![mk_builtin("builtin-office", "Office")]).await;

        let created = fx
            .service
            .create(CreateAssistantRequest {
                id: Some("u1".into()),
                name: "Mine".into(),
                ..req_default()
            })
            .await
            .unwrap();
        assert_eq!(created.source, AssistantSource::User);

        let list = fx.service.list().await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|a| a.id == "builtin-office"));
        assert!(list.iter().any(|a| a.id == "u1"));
    }

    #[tokio::test]
    async fn create_rejects_empty_name() {
        let fx = fixture().await;
        let err = fx
            .service
            .create(CreateAssistantRequest {
                id: Some("u1".into()),
                name: "   ".into(),
                ..req_default()
            })
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[tokio::test]
    async fn create_rejects_builtin_id_collision() {
        let fx = fixture_with_builtins(vec![mk_builtin("builtin-office", "Office")]).await;
        let err = fx
            .service
            .create(CreateAssistantRequest {
                id: Some("builtin-office".into()),
                name: "Mine".into(),
                ..req_default()
            })
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[tokio::test]
    async fn create_rejects_duplicate_user_id() {
        let fx = fixture().await;
        fx.service
            .create(CreateAssistantRequest {
                id: Some("u1".into()),
                name: "A".into(),
                ..req_default()
            })
            .await
            .unwrap();
        let err = fx
            .service
            .create(CreateAssistantRequest {
                id: Some("u1".into()),
                name: "B".into(),
                ..req_default()
            })
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
    }

    #[tokio::test]
    async fn update_rejects_builtin() {
        let fx = fixture_with_builtins(vec![mk_builtin("builtin-office", "Office")]).await;
        let err = fx
            .service
            .update(
                "builtin-office",
                UpdateAssistantRequest {
                    name: Some("New".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Forbidden(_)));
    }

    #[tokio::test]
    async fn update_user_partial_preserves_other_fields() {
        let fx = fixture().await;
        fx.service
            .create(CreateAssistantRequest {
                id: Some("u1".into()),
                name: "original".into(),
                description: Some("desc".into()),
                ..req_default()
            })
            .await
            .unwrap();
        let updated = fx
            .service
            .update(
                "u1",
                UpdateAssistantRequest {
                    name: Some("renamed".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.description.as_deref(), Some("desc"));
    }

    #[tokio::test]
    async fn delete_user_removes_row_and_override() {
        let fx = fixture().await;
        fx.service
            .create(CreateAssistantRequest {
                id: Some("u1".into()),
                name: "A".into(),
                ..req_default()
            })
            .await
            .unwrap();
        fx.service
            .set_state(
                "u1",
                SetAssistantStateRequest {
                    enabled: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        fx.service.delete("u1").await.unwrap();
        // list now empty
        let list = fx.service.list().await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn delete_builtin_rejects() {
        let fx = fixture_with_builtins(vec![mk_builtin("builtin-office", "Office")]).await;
        let err = fx.service.delete("builtin-office").await.unwrap_err();
        assert!(matches!(err, AppError::Forbidden(_)));
    }

    #[tokio::test]
    async fn set_state_builtin_writes_override() {
        let fx = fixture_with_builtins(vec![mk_builtin("builtin-office", "Office")]).await;
        let resp = fx
            .service
            .set_state(
                "builtin-office",
                SetAssistantStateRequest {
                    enabled: Some(false),
                    sort_order: Some(7),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!resp.enabled);
        assert_eq!(resp.sort_order, 7);
        assert_eq!(resp.source, AssistantSource::Builtin);
    }

    #[tokio::test]
    async fn set_state_user_404_when_missing() {
        let fx = fixture().await;
        let err = fx
            .service
            .set_state(
                "unknown",
                SetAssistantStateRequest {
                    enabled: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn import_happy_path() {
        let fx = fixture().await;
        let res = fx
            .service
            .import(ImportAssistantsRequest {
                assistants: vec![
                    CreateAssistantRequest {
                        id: Some("u1".into()),
                        name: "A".into(),
                        ..req_default()
                    },
                    CreateAssistantRequest {
                        id: Some("u2".into()),
                        name: "B".into(),
                        ..req_default()
                    },
                ],
            })
            .await
            .unwrap();
        assert_eq!(res.imported, 2);
        assert_eq!(res.skipped, 0);
        assert_eq!(res.failed, 0);
    }

    #[tokio::test]
    async fn import_skips_builtin_collision() {
        let fx = fixture_with_builtins(vec![mk_builtin("builtin-office", "Office")]).await;
        let res = fx
            .service
            .import(ImportAssistantsRequest {
                assistants: vec![CreateAssistantRequest {
                    id: Some("builtin-office".into()),
                    name: "spoof".into(),
                    ..req_default()
                }],
            })
            .await
            .unwrap();
        assert_eq!(res.imported, 0);
        assert_eq!(res.skipped, 1);
    }

    #[tokio::test]
    async fn import_retry_is_idempotent() {
        let fx = fixture().await;
        let first = fx
            .service
            .import(ImportAssistantsRequest {
                assistants: vec![CreateAssistantRequest {
                    id: Some("u1".into()),
                    name: "A".into(),
                    ..req_default()
                }],
            })
            .await
            .unwrap();
        assert_eq!(first.imported, 1);

        let second = fx
            .service
            .import(ImportAssistantsRequest {
                assistants: vec![CreateAssistantRequest {
                    id: Some("u1".into()),
                    name: "A".into(),
                    ..req_default()
                }],
            })
            .await
            .unwrap();
        assert_eq!(second.imported, 0);
        assert_eq!(second.skipped, 1);
    }

    #[tokio::test]
    async fn import_fails_on_empty_name() {
        let fx = fixture().await;
        let res = fx
            .service
            .import(ImportAssistantsRequest {
                assistants: vec![CreateAssistantRequest {
                    id: Some("u1".into()),
                    name: "  ".into(),
                    ..req_default()
                }],
            })
            .await
            .unwrap();
        assert_eq!(res.imported, 0);
        assert_eq!(res.failed, 1);
        assert_eq!(res.errors.len(), 1);
        assert_eq!(res.errors[0].id, "u1");
    }

    #[tokio::test]
    async fn read_rule_user_returns_empty_when_missing() {
        let fx = fixture().await;
        fx.service
            .create(CreateAssistantRequest {
                id: Some("u1".into()),
                name: "A".into(),
                ..req_default()
            })
            .await
            .unwrap();
        let content = fx.service.read_rule("u1", Some("en-US")).await.unwrap();
        assert!(content.is_empty());
    }

    #[tokio::test]
    async fn write_rule_user_then_read_returns_same() {
        let fx = fixture().await;
        fx.service
            .create(CreateAssistantRequest {
                id: Some("u1".into()),
                name: "A".into(),
                ..req_default()
            })
            .await
            .unwrap();
        fx.service
            .write_rule("u1", Some("en-US"), "rule body")
            .await
            .unwrap();
        let content = fx.service.read_rule("u1", Some("en-US")).await.unwrap();
        assert_eq!(content, "rule body");
    }

    #[tokio::test]
    async fn write_rule_builtin_rejects() {
        let fx = fixture_with_builtins(vec![mk_builtin("builtin-office", "Office")]).await;
        let err = fx
            .service
            .write_rule("builtin-office", Some("en-US"), "x")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[tokio::test]
    async fn read_rule_builtin_dispatches_to_manifest() {
        let tmp = TempDir::new().unwrap();
        let db = init_database_memory().await.unwrap();

        let assets_dir = tmp.path().join("assets");
        let rules_dir = assets_dir.join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(rules_dir.join("office.en-US.md"), "office rules").unwrap();
        let manifest = serde_json::json!({
            "assistants": [{
                "id": "builtin-office",
                "name": "Office",
                "presetAgentType": "gemini",
                "ruleFile": "rules/office.{locale}.md",
            }]
        });
        std::fs::write(
            assets_dir.join("assistants.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        let builtin_reg = Arc::new(BuiltinAssistantRegistry::load_from_dir(assets_dir));

        let repo: Arc<dyn IAssistantRepository> =
            Arc::new(SqliteAssistantRepository::new(db.pool().clone()));
        let orepo: Arc<dyn IAssistantOverrideRepository> =
            Arc::new(SqliteAssistantOverrideRepository::new(db.pool().clone()));
        let event_bus = Arc::new(BroadcastEventBus::new(8));
        let ext_state_store = ExtensionStateStore::new(tmp.path().join("ext-states.json"));
        let extension_registry =
            ExtensionRegistry::new(ext_state_store, event_bus, "1.0.0".to_string());

        let service = AssistantService::new(repo, orepo, builtin_reg, extension_registry);
        let content = service
            .read_rule("builtin-office", Some("en-US"))
            .await
            .unwrap();
        assert_eq!(content, "office rules");
    }

    #[tokio::test]
    async fn classify_falls_back_to_user() {
        let fx = fixture().await;
        assert_eq!(
            fx.service.classify_source("ghost").await,
            AssistantSource::User
        );
    }

    #[tokio::test]
    async fn classify_builtin_wins() {
        let fx = fixture_with_builtins(vec![mk_builtin("builtin-office", "Office")]).await;
        assert_eq!(
            fx.service.classify_source("builtin-office").await,
            AssistantSource::Builtin
        );
    }

    fn req_default() -> CreateAssistantRequest {
        CreateAssistantRequest {
            id: None,
            name: String::new(),
            description: None,
            avatar: None,
            preset_agent_type: None,
            enabled_skills: None,
            custom_skill_names: None,
            disabled_builtin_skills: None,
            prompts: None,
            models: None,
            name_i18n: None,
            description_i18n: None,
            prompts_i18n: None,
        }
    }
}
