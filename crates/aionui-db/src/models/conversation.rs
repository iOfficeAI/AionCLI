// The `status` field is `#[deprecated]` (Phase 2). The `sqlx::FromRow`
// derive expands to code that writes that field at row-materialisation
// time, which fires the deprecation lint inside this very file. The
// struct-level `#[allow(deprecated)]` does not silence the warning at
// the field's declaration site, so we apply a module-level allow here.
// This is intentionally narrow: only this single model file is affected;
// every other consumer of `ConversationRow::status` still sees the
// deprecation warning.
#![allow(deprecated)]

use aionui_common::TimestampMs;
use serde::{Deserialize, Serialize};

/// Row mapping for the `conversations` table.
///
/// Enum-like fields (`type`, `status`, `source`) are stored as TEXT strings.
/// The service layer converts them to/from `aionui_common` enums
/// (`AgentType`, `ConversationStatus`, `ConversationSource`).
///
/// JSON fields (`extra`, `model`) are stored as TEXT in SQLite and
/// deserialized by the service layer.
///
/// Note: this module declares `#![allow(deprecated)]` so the
/// `sqlx::FromRow` derive can still write to the (deprecated) `status`
/// field when materialising rows from the database. The deprecation
/// warning still fires at every other (hand-written) construction or
/// read site outside this file, which is the intended UX.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ConversationRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    /// Agent type string (e.g. "gemini", "acp", "remote").
    #[sqlx(rename = "type")]
    pub r#type: String,
    /// JSON object: type-specific extra data.
    pub extra: String,
    /// JSON object: `ProviderWithModel` serialized.
    pub model: Option<String>,
    /// One of: "pending", "running", "finished". NULL in legacy rows.
    ///
    /// Phase 2 (2026-05-26): `aionui_conversation::ConvActor` is now the
    /// runtime source of truth for whether a conversation is processing
    /// a message. This column is purely advisory legacy and will be
    /// dropped from the schema after N stable releases.
    /// See `docs/superpowers/specs/2026-05-26-conversation-layer-refactor-design.md`.
    #[deprecated(note = "Phase 2: ConvActor is the runtime source of truth. \
                DB.status is purely advisory legacy and will be dropped \
                from the schema after N stable releases.")]
    pub status: Option<String>,
    /// One of: "aionui", "telegram", "lark", "dingtalk", "weixin".
    pub source: Option<String>,
    /// Channel isolation ID (e.g. "user:xxx", "group:xxx").
    pub channel_chat_id: Option<String>,
    /// Whether this conversation is pinned (SQLite INTEGER 0/1).
    pub pinned: bool,
    pub pinned_at: Option<TimestampMs>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}
