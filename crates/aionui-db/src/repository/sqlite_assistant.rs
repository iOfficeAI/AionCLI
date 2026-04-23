//! SQLite-backed assistant repositories.

use aionui_common::{TimestampMs, now_ms};
use sqlx::SqlitePool;

use crate::error::DbError;
use crate::models::{
    AssistantOverrideRow, AssistantRow, CreateAssistantParams, UpdateAssistantParams,
    UpsertOverrideParams,
};
use crate::repository::assistant::{IAssistantOverrideRepository, IAssistantRepository};

/// SQLite-backed implementation of [`IAssistantRepository`].
#[derive(Clone, Debug)]
pub struct SqliteAssistantRepository {
    pool: SqlitePool,
}

impl SqliteAssistantRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn is_unique_violation(err: &dyn sqlx::error::DatabaseError) -> bool {
    err.code().is_some_and(|c| c == "2067" || c == "1555")
}

#[async_trait::async_trait]
impl IAssistantRepository for SqliteAssistantRepository {
    async fn list(&self) -> Result<Vec<AssistantRow>, DbError> {
        let rows =
            sqlx::query_as::<_, AssistantRow>("SELECT * FROM assistants ORDER BY updated_at DESC")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    async fn get(&self, id: &str) -> Result<Option<AssistantRow>, DbError> {
        let row = sqlx::query_as::<_, AssistantRow>("SELECT * FROM assistants WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    async fn create(&self, params: &CreateAssistantParams<'_>) -> Result<AssistantRow, DbError> {
        let now = now_ms();

        sqlx::query(
            "INSERT INTO assistants \
                (id, name, description, avatar, preset_agent_type, enabled_skills, \
                 custom_skill_names, disabled_builtin_skills, prompts, models, \
                 name_i18n, description_i18n, prompts_i18n, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(params.id)
        .bind(params.name)
        .bind(params.description)
        .bind(params.avatar)
        .bind(params.preset_agent_type)
        .bind(params.enabled_skills)
        .bind(params.custom_skill_names)
        .bind(params.disabled_builtin_skills)
        .bind(params.prompts)
        .bind(params.models)
        .bind(params.name_i18n)
        .bind(params.description_i18n)
        .bind(params.prompts_i18n)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db_err) if is_unique_violation(db_err.as_ref()) => {
                DbError::Conflict(format!("Assistant with id '{}' already exists", params.id))
            }
            _ => DbError::Query(e),
        })?;

        Ok(AssistantRow {
            id: params.id.to_string(),
            name: params.name.to_string(),
            description: params.description.map(String::from),
            avatar: params.avatar.map(String::from),
            preset_agent_type: params.preset_agent_type.to_string(),
            enabled_skills: params.enabled_skills.map(String::from),
            custom_skill_names: params.custom_skill_names.map(String::from),
            disabled_builtin_skills: params.disabled_builtin_skills.map(String::from),
            prompts: params.prompts.map(String::from),
            models: params.models.map(String::from),
            name_i18n: params.name_i18n.map(String::from),
            description_i18n: params.description_i18n.map(String::from),
            prompts_i18n: params.prompts_i18n.map(String::from),
            created_at: now,
            updated_at: now,
        })
    }

    async fn update(
        &self,
        id: &str,
        params: &UpdateAssistantParams<'_>,
    ) -> Result<Option<AssistantRow>, DbError> {
        let Some(existing) = self.get(id).await? else {
            return Ok(None);
        };

        let merged = merge_update(existing, params);

        sqlx::query(
            "UPDATE assistants SET \
                name = ?, description = ?, avatar = ?, preset_agent_type = ?, \
                enabled_skills = ?, custom_skill_names = ?, disabled_builtin_skills = ?, \
                prompts = ?, models = ?, name_i18n = ?, description_i18n = ?, \
                prompts_i18n = ?, updated_at = ? \
             WHERE id = ?",
        )
        .bind(&merged.name)
        .bind(&merged.description)
        .bind(&merged.avatar)
        .bind(&merged.preset_agent_type)
        .bind(&merged.enabled_skills)
        .bind(&merged.custom_skill_names)
        .bind(&merged.disabled_builtin_skills)
        .bind(&merged.prompts)
        .bind(&merged.models)
        .bind(&merged.name_i18n)
        .bind(&merged.description_i18n)
        .bind(&merged.prompts_i18n)
        .bind(merged.updated_at)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(Some(merged))
    }

    async fn delete(&self, id: &str) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM assistants WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn upsert(&self, params: &CreateAssistantParams<'_>) -> Result<AssistantRow, DbError> {
        let now = now_ms();

        sqlx::query(
            "INSERT INTO assistants \
                (id, name, description, avatar, preset_agent_type, enabled_skills, \
                 custom_skill_names, disabled_builtin_skills, prompts, models, \
                 name_i18n, description_i18n, prompts_i18n, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
                name = excluded.name, \
                description = excluded.description, \
                avatar = excluded.avatar, \
                preset_agent_type = excluded.preset_agent_type, \
                enabled_skills = excluded.enabled_skills, \
                custom_skill_names = excluded.custom_skill_names, \
                disabled_builtin_skills = excluded.disabled_builtin_skills, \
                prompts = excluded.prompts, \
                models = excluded.models, \
                name_i18n = excluded.name_i18n, \
                description_i18n = excluded.description_i18n, \
                prompts_i18n = excluded.prompts_i18n, \
                updated_at = excluded.updated_at",
        )
        .bind(params.id)
        .bind(params.name)
        .bind(params.description)
        .bind(params.avatar)
        .bind(params.preset_agent_type)
        .bind(params.enabled_skills)
        .bind(params.custom_skill_names)
        .bind(params.disabled_builtin_skills)
        .bind(params.prompts)
        .bind(params.models)
        .bind(params.name_i18n)
        .bind(params.description_i18n)
        .bind(params.prompts_i18n)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        let row = self.get(params.id).await?.ok_or_else(|| {
            DbError::Init(format!("upsert did not produce row for id '{}'", params.id))
        })?;
        Ok(row)
    }
}

fn merge_update(existing: AssistantRow, params: &UpdateAssistantParams<'_>) -> AssistantRow {
    let now = now_ms();
    AssistantRow {
        id: existing.id,
        name: params.name.map(String::from).unwrap_or(existing.name),
        description: params
            .description
            .map_or(existing.description, |v| v.map(String::from)),
        avatar: params
            .avatar
            .map_or(existing.avatar, |v| v.map(String::from)),
        preset_agent_type: params
            .preset_agent_type
            .map(String::from)
            .unwrap_or(existing.preset_agent_type),
        enabled_skills: params
            .enabled_skills
            .map_or(existing.enabled_skills, |v| v.map(String::from)),
        custom_skill_names: params
            .custom_skill_names
            .map_or(existing.custom_skill_names, |v| v.map(String::from)),
        disabled_builtin_skills: params
            .disabled_builtin_skills
            .map_or(existing.disabled_builtin_skills, |v| v.map(String::from)),
        prompts: params
            .prompts
            .map_or(existing.prompts, |v| v.map(String::from)),
        models: params
            .models
            .map_or(existing.models, |v| v.map(String::from)),
        name_i18n: params
            .name_i18n
            .map_or(existing.name_i18n, |v| v.map(String::from)),
        description_i18n: params
            .description_i18n
            .map_or(existing.description_i18n, |v| v.map(String::from)),
        prompts_i18n: params
            .prompts_i18n
            .map_or(existing.prompts_i18n, |v| v.map(String::from)),
        created_at: existing.created_at,
        updated_at: now,
    }
}

/// SQLite-backed implementation of [`IAssistantOverrideRepository`].
#[derive(Clone, Debug)]
pub struct SqliteAssistantOverrideRepository {
    pool: SqlitePool,
}

impl SqliteAssistantOverrideRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl IAssistantOverrideRepository for SqliteAssistantOverrideRepository {
    async fn get(&self, assistant_id: &str) -> Result<Option<AssistantOverrideRow>, DbError> {
        let row = sqlx::query_as::<_, AssistantOverrideRow>(
            "SELECT * FROM assistant_overrides WHERE assistant_id = ?",
        )
        .bind(assistant_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn get_all(&self) -> Result<Vec<AssistantOverrideRow>, DbError> {
        let rows = sqlx::query_as::<_, AssistantOverrideRow>("SELECT * FROM assistant_overrides")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn upsert(
        &self,
        params: &UpsertOverrideParams<'_>,
    ) -> Result<AssistantOverrideRow, DbError> {
        let now = now_ms();
        let last_used_at: Option<TimestampMs> = params.last_used_at;

        sqlx::query(
            "INSERT INTO assistant_overrides \
                (assistant_id, enabled, sort_order, last_used_at, updated_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(assistant_id) DO UPDATE SET \
                enabled = excluded.enabled, \
                sort_order = excluded.sort_order, \
                last_used_at = COALESCE(excluded.last_used_at, assistant_overrides.last_used_at), \
                updated_at = excluded.updated_at",
        )
        .bind(params.assistant_id)
        .bind(params.enabled)
        .bind(params.sort_order)
        .bind(last_used_at)
        .bind(now)
        .execute(&self.pool)
        .await?;

        let row = self.get(params.assistant_id).await?.ok_or_else(|| {
            DbError::Init(format!(
                "upsert did not produce override row for id '{}'",
                params.assistant_id
            ))
        })?;
        Ok(row)
    }

    async fn delete(&self, assistant_id: &str) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM assistant_overrides WHERE assistant_id = ?")
            .bind(assistant_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn delete_orphans(&self, valid_ids: &[&str]) -> Result<u64, DbError> {
        if valid_ids.is_empty() {
            let result = sqlx::query("DELETE FROM assistant_overrides")
                .execute(&self.pool)
                .await?;
            return Ok(result.rows_affected());
        }

        let placeholders = std::iter::repeat_n("?", valid_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql =
            format!("DELETE FROM assistant_overrides WHERE assistant_id NOT IN ({placeholders})");
        let mut q = sqlx::query(&sql);
        for id in valid_ids {
            q = q.bind(*id);
        }
        let result = q.execute(&self.pool).await?;
        Ok(result.rows_affected())
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init_database_memory;

    async fn setup() -> (
        SqliteAssistantRepository,
        SqliteAssistantOverrideRepository,
        crate::Database,
    ) {
        let db = init_database_memory().await.unwrap();
        let a = SqliteAssistantRepository::new(db.pool().clone());
        let o = SqliteAssistantOverrideRepository::new(db.pool().clone());
        (a, o, db)
    }

    fn params<'a>(id: &'a str, name: &'a str) -> CreateAssistantParams<'a> {
        CreateAssistantParams {
            id,
            name,
            description: Some("desc"),
            avatar: None,
            preset_agent_type: "gemini",
            enabled_skills: Some(r#"["skill-a"]"#),
            custom_skill_names: None,
            disabled_builtin_skills: None,
            prompts: Some(r#"["hello"]"#),
            models: None,
            name_i18n: Some(r#"{"zh-CN":"助手"}"#),
            description_i18n: None,
            prompts_i18n: None,
        }
    }

    #[tokio::test]
    async fn assistant_list_empty() {
        let (a, _o, _db) = setup().await;
        assert!(a.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn assistant_create_then_get() {
        let (a, _o, _db) = setup().await;
        let row = a.create(&params("u1", "User One")).await.unwrap();
        assert_eq!(row.id, "u1");
        assert_eq!(row.name, "User One");
        assert_eq!(row.preset_agent_type, "gemini");
        assert_eq!(row.enabled_skills.as_deref(), Some(r#"["skill-a"]"#));
        assert!(row.created_at > 0);
        assert_eq!(row.created_at, row.updated_at);

        let fetched = a.get("u1").await.unwrap().unwrap();
        assert_eq!(fetched.name, "User One");
    }

    #[tokio::test]
    async fn assistant_create_duplicate_id_returns_conflict() {
        let (a, _o, _db) = setup().await;
        a.create(&params("u1", "A")).await.unwrap();
        let err = a.create(&params("u1", "B")).await.unwrap_err();
        assert!(matches!(err, DbError::Conflict(_)));
    }

    #[tokio::test]
    async fn assistant_get_missing_returns_none() {
        let (a, _o, _db) = setup().await;
        assert!(a.get("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn assistant_list_orders_by_updated_at_desc() {
        let (a, _o, _db) = setup().await;
        a.create(&params("u1", "first")).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        a.create(&params("u2", "second")).await.unwrap();

        let list = a.list().await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "u2");
        assert_eq!(list[1].id, "u1");
    }

    #[tokio::test]
    async fn assistant_update_partial_keeps_other_fields() {
        let (a, _o, _db) = setup().await;
        a.create(&params("u1", "original")).await.unwrap();

        let upd = UpdateAssistantParams {
            name: Some("renamed"),
            ..Default::default()
        };
        let updated = a.update("u1", &upd).await.unwrap().unwrap();
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.preset_agent_type, "gemini");
        assert_eq!(updated.description.as_deref(), Some("desc"));
        assert_eq!(updated.enabled_skills.as_deref(), Some(r#"["skill-a"]"#));
        assert!(updated.updated_at >= updated.created_at);
    }

    #[tokio::test]
    async fn assistant_update_clears_nullable_with_some_none() {
        let (a, _o, _db) = setup().await;
        a.create(&params("u1", "has-desc")).await.unwrap();

        let upd = UpdateAssistantParams {
            description: Some(None),
            ..Default::default()
        };
        let updated = a.update("u1", &upd).await.unwrap().unwrap();
        assert!(updated.description.is_none());
    }

    #[tokio::test]
    async fn assistant_update_nonexistent_returns_none() {
        let (a, _o, _db) = setup().await;
        let res = a
            .update(
                "nope",
                &UpdateAssistantParams {
                    name: Some("x"),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn assistant_delete_existing_returns_true() {
        let (a, _o, _db) = setup().await;
        a.create(&params("u1", "x")).await.unwrap();
        assert!(a.delete("u1").await.unwrap());
        assert!(a.get("u1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn assistant_delete_missing_returns_false() {
        let (a, _o, _db) = setup().await;
        assert!(!a.delete("nope").await.unwrap());
    }

    #[tokio::test]
    async fn assistant_upsert_inserts_then_updates() {
        let (a, _o, _db) = setup().await;
        let first = a.upsert(&params("u1", "first")).await.unwrap();
        assert_eq!(first.name, "first");

        let mut p = params("u1", "second");
        p.preset_agent_type = "claude";
        let second = a.upsert(&p).await.unwrap();
        assert_eq!(second.name, "second");
        assert_eq!(second.preset_agent_type, "claude");

        let list = a.list().await.unwrap();
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn override_get_missing_returns_none() {
        let (_a, o, _db) = setup().await;
        assert!(o.get("u1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn override_upsert_inserts_row() {
        let (_a, o, _db) = setup().await;
        let row = o
            .upsert(&UpsertOverrideParams {
                assistant_id: "u1",
                enabled: false,
                sort_order: 5,
                last_used_at: Some(1000),
            })
            .await
            .unwrap();
        assert_eq!(row.assistant_id, "u1");
        assert!(!row.enabled);
        assert_eq!(row.sort_order, 5);
        assert_eq!(row.last_used_at, Some(1000));
    }

    #[tokio::test]
    async fn override_upsert_updates_existing() {
        let (_a, o, _db) = setup().await;
        o.upsert(&UpsertOverrideParams {
            assistant_id: "u1",
            enabled: true,
            sort_order: 0,
            last_used_at: Some(1000),
        })
        .await
        .unwrap();

        let updated = o
            .upsert(&UpsertOverrideParams {
                assistant_id: "u1",
                enabled: false,
                sort_order: 3,
                last_used_at: None,
            })
            .await
            .unwrap();

        assert!(!updated.enabled);
        assert_eq!(updated.sort_order, 3);
        // last_used_at None does not overwrite previous value (COALESCE)
        assert_eq!(updated.last_used_at, Some(1000));
    }

    #[tokio::test]
    async fn override_get_all_returns_rows() {
        let (_a, o, _db) = setup().await;
        o.upsert(&UpsertOverrideParams {
            assistant_id: "u1",
            enabled: true,
            sort_order: 0,
            last_used_at: None,
        })
        .await
        .unwrap();
        o.upsert(&UpsertOverrideParams {
            assistant_id: "u2",
            enabled: false,
            sort_order: 1,
            last_used_at: None,
        })
        .await
        .unwrap();

        let all = o.get_all().await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn override_delete() {
        let (_a, o, _db) = setup().await;
        o.upsert(&UpsertOverrideParams {
            assistant_id: "u1",
            enabled: true,
            sort_order: 0,
            last_used_at: None,
        })
        .await
        .unwrap();
        assert!(o.delete("u1").await.unwrap());
        assert!(!o.delete("u1").await.unwrap());
    }

    #[tokio::test]
    async fn override_delete_orphans_removes_only_absent() {
        let (_a, o, _db) = setup().await;
        for id in ["a", "b", "c"] {
            o.upsert(&UpsertOverrideParams {
                assistant_id: id,
                enabled: true,
                sort_order: 0,
                last_used_at: None,
            })
            .await
            .unwrap();
        }
        let removed = o.delete_orphans(&["a", "c"]).await.unwrap();
        assert_eq!(removed, 1);
        let remaining: Vec<String> = o
            .get_all()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.assistant_id)
            .collect();
        assert!(remaining.contains(&"a".to_string()));
        assert!(remaining.contains(&"c".to_string()));
        assert!(!remaining.contains(&"b".to_string()));
    }

    #[tokio::test]
    async fn override_delete_orphans_empty_valid_ids_clears_table() {
        let (_a, o, _db) = setup().await;
        o.upsert(&UpsertOverrideParams {
            assistant_id: "a",
            enabled: true,
            sort_order: 0,
            last_used_at: None,
        })
        .await
        .unwrap();
        let removed = o.delete_orphans(&[]).await.unwrap();
        assert_eq!(removed, 1);
        assert!(o.get_all().await.unwrap().is_empty());
    }
}
