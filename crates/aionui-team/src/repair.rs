//! Pure reverse-derivation of team agents from persisted conversations.
//!
//! When a team row survives with an empty `agents` array (e.g. cleared by a
//! legacy migration) but its conversations still carry `extra.team_id`, we can
//! reconstruct the agent list from those conversation rows. This is a read-only
//! inference; persistence is the caller's responsibility (see W3-D13c).

use aionui_common::ProviderWithModel;
use aionui_db::models::ConversationRow;
use serde_json::Value;

use crate::types::{TeamAgent, TeammateRole};

/// Reconstruct a team's `agents` list from its conversations.
///
/// Rules (matching `docs/teams/phase1/interface-contracts.md` §14.2):
/// - Input conversations are sorted by `created_at` ascending; the earliest
///   one becomes the Lead, the rest are Teammates.
/// - `slot_id` is read from `extra.team_mcp_stdio_config.slot_id` (written by
///   Wave 2), falling back to a bare `extra.slot_id`, then empty.
/// - `backend` comes from `conversation.type`; `model` is extracted from the
///   JSON-encoded `ProviderWithModel` stored in `conversation.model`.
pub fn repair_team_agents_if_missing(convs: &[ConversationRow]) -> Vec<TeamAgent> {
    let mut sorted: Vec<&ConversationRow> = convs.iter().collect();
    sorted.sort_by_key(|c| c.created_at);

    sorted
        .into_iter()
        .enumerate()
        .map(|(idx, conv)| {
            let extra: Value = serde_json::from_str(&conv.extra).unwrap_or(Value::Null);
            let slot_id = extract_slot_id(&extra);
            let model = conv
                .model
                .as_deref()
                .and_then(|m| serde_json::from_str::<ProviderWithModel>(m).ok())
                .map(|p| p.model)
                .unwrap_or_default();
            let role = if idx == 0 {
                TeammateRole::Lead
            } else {
                TeammateRole::Teammate
            };
            TeamAgent {
                slot_id,
                name: conv.name.clone(),
                role,
                conversation_id: conv.id.clone(),
                backend: conv.r#type.clone(),
                model,
                custom_agent_id: None,
                status: None,
                conversation_type: None,
                cli_path: None,
            }
        })
        .collect()
}

fn extract_slot_id(extra: &Value) -> String {
    extra
        .get("team_mcp_stdio_config")
        .and_then(|c| c.get("slot_id"))
        .and_then(Value::as_str)
        .or_else(|| extra.get("slot_id").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_conv(
        id: &str,
        name: &str,
        created_at: i64,
        extra: Value,
        model: &str,
    ) -> ConversationRow {
        ConversationRow {
            id: id.to_owned(),
            user_id: "u1".to_owned(),
            name: name.to_owned(),
            r#type: "acp".to_owned(),
            extra: extra.to_string(),
            model: Some(
                serde_json::json!({
                    "provider_id": "anthropic",
                    "model": model,
                    "use_model": null,
                })
                .to_string(),
            ),
            status: None,
            source: None,
            channel_chat_id: None,
            pinned: false,
            pinned_at: None,
            created_at,
            updated_at: created_at,
        }
    }

    #[test]
    fn reverse_derives_two_agents_and_first_is_lead() {
        // Out-of-order input: second conversation is older and must become Lead.
        let convs = vec![
            make_conv(
                "conv-b",
                "Worker",
                2_000,
                serde_json::json!({
                    "team_id": "team-1",
                    "team_mcp_stdio_config": { "slot_id": "slot-b" },
                }),
                "claude",
            ),
            make_conv(
                "conv-a",
                "Lead",
                1_000,
                serde_json::json!({
                    "team_id": "team-1",
                    "team_mcp_stdio_config": { "slot_id": "slot-a" },
                }),
                "sonnet",
            ),
        ];

        let agents = repair_team_agents_if_missing(&convs);

        assert_eq!(agents.len(), 2);
        // Earliest created_at wins the Lead slot, regardless of input order.
        assert_eq!(agents[0].conversation_id, "conv-a");
        assert_eq!(agents[0].role, TeammateRole::Lead);
        assert_eq!(agents[0].slot_id, "slot-a");
        assert_eq!(agents[0].name, "Lead");
        assert_eq!(agents[0].backend, "acp");
        assert_eq!(agents[0].model, "sonnet");

        assert_eq!(agents[1].conversation_id, "conv-b");
        assert_eq!(agents[1].role, TeammateRole::Teammate);
        assert_eq!(agents[1].slot_id, "slot-b");
        assert_eq!(agents[1].model, "claude");
    }

    #[test]
    fn empty_input_returns_empty_and_slot_id_fallback() {
        assert!(repair_team_agents_if_missing(&[]).is_empty());

        // Fallback path: no `team_mcp_stdio_config`, only a bare `slot_id`.
        let convs = vec![make_conv(
            "conv-x",
            "Solo",
            500,
            serde_json::json!({ "team_id": "t", "slot_id": "fallback-slot" }),
            "opus",
        )];
        let agents = repair_team_agents_if_missing(&convs);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].slot_id, "fallback-slot");
        assert_eq!(agents[0].role, TeammateRole::Lead);
    }
}
