use std::path::Path;
use std::sync::Arc;

use aionui_api_types::{
    ConversationArtifactResponse, ConversationResponse, ConversationStatus, MessageResponse, MessageSearchItem,
};
use aionui_common::{
    AgentType, AppError, ConversationSource, MessagePosition, MessageStatus, MessageType, ProviderWithModel,
};
use aionui_db::MessageSearchRow;
use aionui_db::models::{ConversationArtifactRow, ConversationRow, MessageRow};
use dashmap::DashMap;

use crate::conv_actor::ConvActor;
use crate::conv_service_trait::ConversationStatus as ConvConversationStatus;

/// Convert a database row into an API response DTO.
///
/// Parses string enum fields and JSON text fields back into typed values.
/// `data_dir` is required so the response can expose a derived
/// `is_temporary_workspace` flag without storing that attribute on disk —
/// see [`row_to_response_with_extra`].
///
/// `actors` is the per-conversation runtime state map owned by
/// `ConversationService`. Phase 2 derives `ConversationResponse.status`
/// from this map rather than from `row.status`. Callers that legitimately
/// have no actor map at hand (e.g. ad-hoc tests for the conversion logic
/// itself) can pass an empty `DashMap`, which yields `Idle/Finished`.
pub fn row_to_response(
    row: ConversationRow,
    data_dir: &Path,
    actors: &DashMap<String, Arc<ConvActor>>,
) -> Result<ConversationResponse, AppError> {
    let extra: serde_json::Value =
        serde_json::from_str(&row.extra).map_err(|e| AppError::Internal(format!("Invalid extra JSON: {e}")))?;
    row_to_response_with_extra(row, extra, data_dir, actors)
}

/// Same as [`row_to_response`] but takes a pre-parsed `extra` value. Used
/// by callers that need to mutate `extra` (e.g. lazy `skills` backfill)
/// before building the response DTO.
///
/// Injects a derived `is_temporary_workspace: bool` into the returned
/// `extra` blob by checking whether `extra.workspace` sits under the
/// backend-managed `data_dir`. The flag is not persisted — it is
/// computed on every read so the frontend never has to pattern-match
/// the directory name. Old rows that have no such flag on disk
/// automatically gain it on read, which means no migration is needed.
pub fn row_to_response_with_extra(
    row: ConversationRow,
    mut extra: serde_json::Value,
    data_dir: &Path,
    actors: &DashMap<String, Arc<ConvActor>>,
) -> Result<ConversationResponse, AppError> {
    let is_temporary_workspace = {
        let ws = extra.get("workspace").and_then(|v| v.as_str()).unwrap_or("");
        !ws.is_empty() && Path::new(ws).starts_with(data_dir)
    };
    if let Some(obj) = extra.as_object_mut() {
        obj.insert(
            "is_temporary_workspace".to_owned(),
            serde_json::Value::Bool(is_temporary_workspace),
        );
    }

    let agent_type: AgentType = string_to_enum(&row.r#type)?;

    // Phase 2: runtime status (Idle/Running) is derived from the ConvActor
    // map. When no actor exists for this row (newly-created conversation,
    // or one that has not been opened since process start), fall back to
    // the legacy DB.status so the wire format keeps emitting `pending`
    // for the never-opened state. The legacy three-state enum
    // (Pending/Running/Finished) is preserved end-to-end through Phase 5;
    // Phase 6 schedules the DTO/column cleanup. Frontend compatibility
    // requires `pending` for newly-created rows so the empty conversation
    // view renders correctly.
    #[allow(deprecated)]
    let status: ConversationStatus = match actors.get(&row.id).map(|a| a.public_status()) {
        Some(ConvConversationStatus::Running { .. }) => ConversationStatus::Running,
        Some(ConvConversationStatus::Idle) => ConversationStatus::Finished,
        None => row
            .status
            .as_deref()
            .map(string_to_enum)
            .transpose()?
            .unwrap_or(ConversationStatus::Pending),
    };

    let source: Option<ConversationSource> = row.source.as_deref().map(string_to_enum).transpose()?;

    let model: Option<ProviderWithModel> = row.model.as_deref().map(parse_provider_with_model).transpose()?;

    Ok(ConversationResponse {
        id: row.id,
        name: row.name,
        r#type: agent_type,
        model,
        status,
        source,
        pinned: row.pinned,
        pinned_at: row.pinned_at,
        channel_chat_id: row.channel_chat_id,
        created_at: row.created_at,
        modified_at: row.updated_at,
        extra,
    })
}

/// Parse the model JSON column into `ProviderWithModel`.
///
/// AionUi stores the full provider object (`TProviderWithModel`) which includes
/// fields like `id`, `platform`, `base_url`, `api_key`, `use_model`, and a `model`
/// field that can be an array of model objects. The backend only needs
/// `provider_id`, `model` (the selected model name), and `use_model`.
/// Accepts both snake_case and legacy camelCase key names for backward compatibility.
fn parse_provider_with_model(s: &str) -> Result<ProviderWithModel, AppError> {
    let v: serde_json::Value =
        serde_json::from_str(s).map_err(|e| AppError::Internal(format!("Invalid model JSON: {e}")))?;

    if let Some(provider_id) = v
        .get("provider_id")
        .or_else(|| v.get("providerId"))
        .and_then(|v| v.as_str())
    {
        let model = v.get("model").and_then(|v| v.as_str()).unwrap_or_default();
        let use_model = v
            .get("use_model")
            .or_else(|| v.get("useModel"))
            .and_then(|v| v.as_str())
            .map(String::from);
        return Ok(ProviderWithModel {
            provider_id: provider_id.to_string(),
            model: model.to_string(),
            use_model,
        });
    }

    if let Some(id) = v.get("id").and_then(|v| v.as_str()) {
        let use_model_str = v
            .get("use_model")
            .or_else(|| v.get("useModel"))
            .and_then(|v| v.as_str())
            .map(String::from);
        return Ok(ProviderWithModel {
            provider_id: id.to_string(),
            model: use_model_str.clone().unwrap_or_default(),
            use_model: use_model_str,
        });
    }

    Err(AppError::Internal(format!(
        "Model JSON missing both 'provider_id'/'providerId' and 'id': {s}"
    )))
}

/// Parse a DB string value into a typed enum via serde.
///
/// e.g. `"acp"` → `AgentType::Acp`
pub fn string_to_enum<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, AppError> {
    serde_json::from_value(serde_json::Value::String(s.to_owned()))
        .map_err(|e| AppError::Internal(format!("Invalid enum value '{s}': {e}")))
}

/// Convert a message database row into an API response DTO.
pub fn row_to_message_response(row: MessageRow) -> Result<MessageResponse, AppError> {
    let msg_type: MessageType = string_to_enum(&row.r#type)?;

    let position: Option<MessagePosition> = row.position.as_deref().map(string_to_enum).transpose()?;

    let status: Option<MessageStatus> = row.status.as_deref().map(string_to_enum).transpose()?;

    let content: serde_json::Value = serde_json::from_str(&row.content)
        .map_err(|e| AppError::Internal(format!("Invalid message content JSON: {e}")))?;

    Ok(MessageResponse {
        id: row.id,
        conversation_id: row.conversation_id,
        msg_id: row.msg_id,
        r#type: msg_type,
        content,
        position,
        status,
        hidden: row.hidden,
        created_at: row.created_at,
    })
}

/// Convert an artifact database row into an API response DTO.
pub fn row_to_artifact_response(row: ConversationArtifactRow) -> Result<ConversationArtifactResponse, AppError> {
    let kind = string_to_enum(&row.kind)?;
    let status = string_to_enum(&row.status)?;
    let payload: serde_json::Value = serde_json::from_str(&row.payload)
        .map_err(|e| AppError::Internal(format!("Invalid artifact payload JSON: {e}")))?;

    Ok(ConversationArtifactResponse {
        id: row.id,
        conversation_id: row.conversation_id,
        cron_job_id: row.cron_job_id,
        kind,
        status,
        payload,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// Extract plain-text preview from a message content field.
///
/// Message content is stored as JSON (arrays, objects with nested strings).
/// This recursively collects all string values and joins them with spaces,
/// producing a flat preview suitable for search snippet display.
fn extract_preview_text(raw_content: &str) -> String {
    fn collect_strings(value: &serde_json::Value, bucket: &mut Vec<String>) {
        match value {
            serde_json::Value::String(s) => {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    bucket.push(trimmed.to_owned());
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr {
                    collect_strings(item, bucket);
                }
            }
            serde_json::Value::Object(map) => {
                for item in map.values() {
                    collect_strings(item, bucket);
                }
            }
            _ => {}
        }
    }

    match serde_json::from_str::<serde_json::Value>(raw_content) {
        Ok(parsed) => {
            let mut bucket = Vec::new();
            collect_strings(&parsed, &mut bucket);
            let joined = bucket.join(" ");
            let normalized = joined.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized.is_empty() {
                raw_content.split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                normalized
            }
        }
        Err(_) => raw_content.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

/// Convert a search result row into an API search item DTO.
pub fn search_row_to_item(
    row: MessageSearchRow,
    data_dir: &Path,
    actors: &DashMap<String, Arc<ConvActor>>,
) -> Result<MessageSearchItem, AppError> {
    // We rebuild a `ConversationRow` purely so we can reuse `row_to_response`.
    // Phase 2: the `status` column is `#[deprecated]` because runtime code
    // must not consult it. We still copy `row.conversation_status` so the
    // struct can be populated; `row_to_response` then ignores that value
    // and derives the runtime status from the `actors` map.
    #[allow(deprecated)]
    let conversation_row = ConversationRow {
        id: row.conversation_id,
        user_id: String::new(),
        name: row.conversation_name,
        r#type: row.conversation_type,
        extra: row.conversation_extra,
        model: row.conversation_model,
        status: row.conversation_status,
        source: row.conversation_source,
        channel_chat_id: row.conversation_channel_chat_id,
        pinned: row.conversation_pinned,
        pinned_at: row.conversation_pinned_at,
        created_at: row.conversation_created_at,
        updated_at: row.conversation_updated_at,
    };

    let conversation = row_to_response(conversation_row, data_dir, actors)?;
    let preview_text = extract_preview_text(&row.content);

    Ok(MessageSearchItem {
        message_id: row.message_id,
        message_type: row.r#type,
        message_created_at: row.created_at,
        preview_text,
        conversation,
    })
}

#[cfg(test)]
#[allow(deprecated)] // Tests deliberately construct `ConversationRow.status` to
// verify legacy fixture handling. The field is `#[deprecated]`
// (Phase 2) but remains the DB column shape.
mod tests {
    use super::*;
    use aionui_api_types::ConversationStatus;
    use aionui_common::{AgentType, ConversationSource};
    use serde_json::json;

    fn empty_actors() -> DashMap<String, Arc<ConvActor>> {
        DashMap::new()
    }

    fn make_row(
        agent_type: &str,
        status: &str,
        source: Option<&str>,
        model_json: Option<&str>,
        extra_json: &str,
    ) -> ConversationRow {
        ConversationRow {
            id: "conv_1".into(),
            user_id: "user_1".into(),
            name: "Test".into(),
            r#type: agent_type.into(),
            extra: extra_json.into(),
            model: model_json.map(|s| s.into()),
            status: Some(status.into()),
            source: source.map(|s| s.into()),
            channel_chat_id: None,
            pinned: false,
            pinned_at: None,
            created_at: 1000,
            updated_at: 2000,
        }
    }

    #[test]
    fn row_to_response_basic() {
        let model = json!({"providerId": "p1", "model": "m1"});
        let row = make_row(
            "acp",
            "pending",
            Some("aionui"),
            Some(&model.to_string()),
            r#"{"workspace": "/project"}"#,
        );
        let actors = empty_actors();
        let resp = row_to_response(row, Path::new("/tmp/data"), &actors).unwrap();
        assert_eq!(resp.id, "conv_1");
        assert_eq!(resp.r#type, AgentType::Acp);
        // Phase 2: when no actor is registered, status falls back to the
        // legacy DB.status so the wire format keeps emitting `pending` for
        // never-opened rows. Once an actor exists, runtime state takes over.
        assert_eq!(resp.status, ConversationStatus::Pending);
        assert_eq!(resp.source, Some(ConversationSource::Aionui));
        assert_eq!(resp.model.unwrap().model, "m1");
        assert_eq!(resp.extra["workspace"], "/project");
        assert_eq!(resp.modified_at, 2000);
    }

    #[test]
    fn row_to_response_status_running_when_actor_running() {
        let row = make_row("acp", "pending", Some("aionui"), None, "{}");
        let actors: DashMap<String, Arc<ConvActor>> = DashMap::new();
        let actor = ConvActor::new("conv_1".into());
        // Drive the actor into Running synchronously via the public surface:
        // mark_idle then begin_turn.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _handle = rt.block_on(async {
            actor.mark_idle().await;
            actor.begin_turn("msg-1".into()).await.unwrap()
        });
        actors.insert("conv_1".into(), actor);

        let resp = row_to_response(row, Path::new("/tmp/data"), &actors).unwrap();
        assert_eq!(resp.status, ConversationStatus::Running);
        // Drop the handle inside the runtime so the actor's Drop side-effects
        // (which spawn nothing) don't leak.
        drop(_handle);
        drop(rt);
    }

    #[test]
    fn row_to_response_no_source() {
        let row = make_row("acp", "running", None, None, "{}");
        let actors = empty_actors();
        let resp = row_to_response(row, Path::new("/tmp/data"), &actors).unwrap();
        assert!(resp.source.is_none());
        assert!(resp.model.is_none());
    }

    #[test]
    fn row_to_response_invalid_type() {
        let row = make_row("invalid", "pending", None, None, "{}");
        let actors = empty_actors();
        let err = row_to_response(row, Path::new("/tmp/data"), &actors).unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn row_to_response_invalid_extra_json() {
        let row = ConversationRow {
            id: "conv_1".into(),
            user_id: "user_1".into(),
            name: "Test".into(),
            r#type: "acp".into(),
            extra: "not-json".into(),
            model: None,
            status: Some("pending".into()),
            source: None,
            channel_chat_id: None,
            pinned: false,
            pinned_at: None,
            created_at: 1000,
            updated_at: 2000,
        };
        let actors = empty_actors();
        let err = row_to_response(row, Path::new("/tmp/data"), &actors).unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn string_to_enum_valid() {
        let agent: AgentType = string_to_enum("acp").unwrap();
        assert_eq!(agent, AgentType::Acp);

        let status: ConversationStatus = string_to_enum("finished").unwrap();
        assert_eq!(status, ConversationStatus::Finished);

        let src: ConversationSource = string_to_enum("telegram").unwrap();
        assert_eq!(src, ConversationSource::Telegram);
    }

    #[test]
    fn string_to_enum_invalid() {
        let err = string_to_enum::<AgentType>("not_valid").unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn parse_provider_with_model_backend_format() {
        let json = r#"{"providerId":"p1","model":"claude-sonnet-4-20250514","useModel":"claude-sonnet"}"#;
        let result = parse_provider_with_model(json).unwrap();
        assert_eq!(result.provider_id, "p1");
        assert_eq!(result.model, "claude-sonnet-4-20250514");
        assert_eq!(result.use_model.as_deref(), Some("claude-sonnet"));
    }

    #[test]
    fn parse_provider_with_model_aionui_format() {
        let json = r#"{"id":"prov_1","platform":"openai","name":"My Provider","baseUrl":"https://api.openai.com","apiKey":"sk-xxx","model":[{"id":"gpt-4","name":"GPT-4"}],"capabilities":["text","vision"],"useModel":"gpt-4-turbo","enabled":true}"#;
        let result = parse_provider_with_model(json).unwrap();
        assert_eq!(result.provider_id, "prov_1");
        assert_eq!(result.model, "gpt-4-turbo");
        assert_eq!(result.use_model.as_deref(), Some("gpt-4-turbo"));
    }

    #[test]
    fn parse_provider_with_model_missing_both_ids() {
        let json = r#"{"name":"invalid"}"#;
        assert!(parse_provider_with_model(json).is_err());
    }

    #[test]
    fn row_to_response_marks_workspace_inside_data_dir_as_temporary() {
        let row = make_row(
            "acp",
            "pending",
            Some("aionui"),
            None,
            r#"{"workspace":"/srv/aionui-data/conversations/claude-temp-abc"}"#,
        );
        let actors = empty_actors();
        let resp = row_to_response(row, Path::new("/srv/aionui-data"), &actors).unwrap();
        assert_eq!(resp.extra["is_temporary_workspace"], true);
    }

    #[test]
    fn row_to_response_marks_workspace_outside_data_dir_as_non_temporary() {
        let row = make_row(
            "acp",
            "pending",
            Some("aionui"),
            None,
            r#"{"workspace":"/Users/alice/my-project"}"#,
        );
        let actors = empty_actors();
        let resp = row_to_response(row, Path::new("/srv/aionui-data"), &actors).unwrap();
        assert_eq!(resp.extra["is_temporary_workspace"], false);
    }

    #[test]
    fn row_to_response_marks_missing_workspace_as_non_temporary() {
        let row = make_row("acp", "pending", Some("aionui"), None, r#"{}"#);
        let actors = empty_actors();
        let resp = row_to_response(row, Path::new("/srv/aionui-data"), &actors).unwrap();
        assert_eq!(resp.extra["is_temporary_workspace"], false);
    }

    #[test]
    fn row_with_pinned_at() {
        let row = ConversationRow {
            id: "conv_2".into(),
            user_id: "user_1".into(),
            name: "Pinned".into(),
            r#type: "acp".into(),
            extra: "{}".into(),
            model: None,
            status: Some("pending".into()),
            source: Some("aionui".into()),
            channel_chat_id: Some("chat:1".into()),
            pinned: true,
            pinned_at: Some(5000),
            created_at: 1000,
            updated_at: 3000,
        };
        let actors = empty_actors();
        let resp = row_to_response(row, Path::new("/tmp/data"), &actors).unwrap();
        assert!(resp.pinned);
        assert_eq!(resp.pinned_at, Some(5000));
        assert_eq!(resp.channel_chat_id.as_deref(), Some("chat:1"));
    }

    // ── extract_preview_text ───────────────────────────────────────────

    #[test]
    fn test_extract_preview_text_json_array() {
        let content = r#"[{"type":"text","content":"Hello world"},{"type":"text","content":"How are you?"}]"#;
        let result = extract_preview_text(content);
        assert!(result.contains("Hello world"));
        assert!(result.contains("How are you?"));
    }

    #[test]
    fn test_extract_preview_text_plain_string() {
        let content = "Just plain text message";
        let result = extract_preview_text(content);
        assert_eq!(result, "Just plain text message");
    }

    #[test]
    fn test_extract_preview_text_nested_object() {
        let content = r#"{"text":"nested value","items":[{"content":"inner"}]}"#;
        let result = extract_preview_text(content);
        assert!(result.contains("nested value"));
        assert!(result.contains("inner"));
    }

    #[test]
    fn test_extract_preview_text_malformed_json() {
        let content = "this is not { json at all";
        let result = extract_preview_text(content);
        assert_eq!(result, "this is not { json at all");
    }

    #[test]
    fn test_extract_preview_text_empty_content() {
        let result = extract_preview_text("");
        assert_eq!(result, "");
    }

    #[test]
    fn test_extract_preview_text_whitespace_normalization() {
        let content = r#"{"content":"  hello   world  "}"#;
        let result = extract_preview_text(content);
        assert_eq!(result, "hello world");
    }

    // ── search_row_to_item ─────────────────────────────────────────────

    #[test]
    fn test_search_row_to_item_builds_nested_conversation() {
        let row = MessageSearchRow {
            message_id: "msg_1".into(),
            r#type: "text".into(),
            content: r#"{"content":"hello world"}"#.into(),
            created_at: 5000,
            conversation_id: "conv_1".into(),
            conversation_name: "Test Conv".into(),
            conversation_type: "acp".into(),
            conversation_extra: r#"{"workspace":"/project"}"#.into(),
            conversation_model: None,
            conversation_status: Some("finished".into()),
            conversation_source: Some("aionui".into()),
            conversation_channel_chat_id: None,
            conversation_pinned: false,
            conversation_pinned_at: None,
            conversation_created_at: 1000,
            conversation_updated_at: 2000,
        };

        let actors = empty_actors();
        let item = search_row_to_item(row, Path::new("/tmp/data"), &actors).unwrap();

        assert_eq!(item.message_id, "msg_1");
        assert_eq!(item.message_type, "text");
        assert_eq!(item.message_created_at, 5000);
        assert_eq!(item.preview_text, "hello world");

        assert_eq!(item.conversation.id, "conv_1");
        assert_eq!(item.conversation.name, "Test Conv");
        assert_eq!(item.conversation.r#type, AgentType::Acp);
        assert_eq!(item.conversation.source, Some(ConversationSource::Aionui));
        assert_eq!(item.conversation.extra["workspace"], "/project");
        assert_eq!(item.conversation.modified_at, 2000);
    }

    #[test]
    fn test_search_row_to_item_invalid_conversation_type() {
        let row = MessageSearchRow {
            message_id: "msg_1".into(),
            r#type: "text".into(),
            content: "plain text".into(),
            created_at: 5000,
            conversation_id: "conv_1".into(),
            conversation_name: "Test".into(),
            conversation_type: "invalid_type".into(),
            conversation_extra: "{}".into(),
            conversation_model: None,
            conversation_status: Some("finished".into()),
            conversation_source: None,
            conversation_channel_chat_id: None,
            conversation_pinned: false,
            conversation_pinned_at: None,
            conversation_created_at: 1000,
            conversation_updated_at: 2000,
        };

        let actors = empty_actors();
        let err = search_row_to_item(row, Path::new("/tmp/data"), &actors).unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn test_search_row_to_item_invalid_conversation_extra_json() {
        let row = MessageSearchRow {
            message_id: "msg_1".into(),
            r#type: "text".into(),
            content: r#"{"content":"hello"}"#.into(),
            created_at: 5000,
            conversation_id: "conv_1".into(),
            conversation_name: "Test".into(),
            conversation_type: "acp".into(),
            conversation_extra: "not valid json".into(),
            conversation_model: None,
            conversation_status: Some("finished".into()),
            conversation_source: None,
            conversation_channel_chat_id: None,
            conversation_pinned: false,
            conversation_pinned_at: None,
            conversation_created_at: 1000,
            conversation_updated_at: 2000,
        };

        let actors = empty_actors();
        let err = search_row_to_item(row, Path::new("/tmp/data"), &actors).unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }
}
