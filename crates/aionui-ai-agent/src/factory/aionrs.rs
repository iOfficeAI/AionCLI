use std::collections::HashMap;
use std::sync::Arc;

use aion_agent::session::SessionManager;
use aion_config::config::{McpServerConfig, TransportType};
use aionui_api_types::{AionrsBuildExtra, GuideMcpConfig, TeamMcpStdioConfig};
use aionui_common::AppError;
use tracing::{debug, info};

use crate::agent_task::AgentInstance;
use crate::capability::team_guide_prompt;
use crate::factory::AgentFactoryDeps;
use crate::factory::context::FactoryContext;
use crate::manager::aionrs::AionrsAgentManager;
use crate::types::{AionrsCompatOverrides, AionrsResolvedConfig, BuildTaskOptions};

const TEAM_CAPABLE_BACKENDS: &[&str] = &["claude", "codex", "gemini", "aionrs", "codebuddy"];

pub(super) async fn build(
    deps: Arc<AgentFactoryDeps>,
    options: BuildTaskOptions,
    ctx: FactoryContext,
) -> Result<AgentInstance, AppError> {
    let belongs_to_team = options
        .extra
        .get("teamId")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|s| !s.is_empty());

    let mut overrides: AionrsBuildExtra = serde_json::from_value(options.extra).unwrap_or_default();

    // Merge preset assistant rules into system_prompt (used as custom_prompt
    // in aionrs's build_system_prompt). Mirrors the old architecture's
    // `init_history` injection of `[Assistant System Rules]`.
    if let Some(rules) = overrides.preset_rules.take() {
        overrides.system_prompt = Some(match overrides.system_prompt.take() {
            Some(existing) => format!("{existing}\n\n{rules}"),
            None => rules,
        });
    }

    // Inject Guide MCP config for solo (non-team) sessions, mirroring acp.rs.
    // Skip if the conversation already belongs to a team (extra.teamId set).
    if overrides.team_mcp_stdio_config.is_none()
        && overrides.guide_mcp_config.is_none()
        && deps.guide_mcp_config.is_some()
        && !belongs_to_team
    {
        overrides.guide_mcp_config.clone_from(&deps.guide_mcp_config);
        overrides.backend.get_or_insert_with(|| "aionrs".to_owned());
    }

    let extra_mcp_servers = resolve_mcp_servers(&overrides, &ctx.conversation_id);

    // Inject team guide system prompt for solo sessions with guide MCP
    if overrides.team_mcp_stdio_config.is_none()
        && overrides.guide_mcp_config.is_some()
        && team_guide_prompt::is_solo_team_guide_backend(overrides.backend.as_deref().unwrap_or("aionrs"))
    {
        let guide_prompt =
            team_guide_prompt::build_solo_team_guide_prompt(overrides.backend.as_deref().unwrap_or("aionrs"));
        overrides.system_prompt = Some(match overrides.system_prompt.take() {
            Some(existing) => format!("{existing}\n\n{guide_prompt}"),
            None => guide_prompt,
        });
    }

    if !extra_mcp_servers.is_empty() {
        info!(
            conversation_id = %ctx.conversation_id,
            mcp_count = extra_mcp_servers.len(),
            mcp_names = ?extra_mcp_servers.keys().collect::<Vec<_>>(),
            "Injecting MCP servers into aionrs session"
        );
    }

    let provider_id = &options.model.provider_id;
    let row = deps
        .provider_repo
        .find_by_id(provider_id)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to load provider config: {e}")))?
        .ok_or_else(|| AppError::BadRequest(format!("Provider '{provider_id}' not found")))?;

    let api_key = aionui_common::decrypt_string(&row.api_key_encrypted, &deps.encryption_key)?;

    let model_id = options
        .model
        .use_model
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&options.model.model)
        .to_owned();

    let provider = map_aionrs_provider(&row.platform, &model_id, row.model_protocols.as_deref());

    let (base_url, compat_overrides) = resolve_aionrs_url_and_compat(&row.platform, &row.base_url, &provider);

    let bedrock_config = if row.platform == "bedrock" {
        resolve_bedrock_config(row.bedrock_config.as_deref())
    } else {
        None
    };

    let session_directory = deps.data_dir.join("aionrs-sessions");

    let resume_session = {
        let session_mgr = SessionManager::new(session_directory.clone(), 100);
        match session_mgr.load(&ctx.conversation_id) {
            Ok(session) => {
                info!(
                    conversation_id = %ctx.conversation_id,
                    session_id = %session.id,
                    message_count = session.messages.len(),
                    "Loaded existing aionrs session for resume"
                );
                Some(session)
            }
            Err(_) => {
                // Fallback: old architecture stored sessions inside the workspace
                let legacy_dir = std::path::Path::new(&ctx.workspace).join(".aionrs/sessions");
                let legacy_mgr = SessionManager::new(legacy_dir.clone(), 100);
                match legacy_mgr.load(&ctx.conversation_id) {
                    Ok(session) => {
                        info!(
                            conversation_id = %ctx.conversation_id,
                            session_id = %session.id,
                            message_count = session.messages.len(),
                            "Loaded legacy aionrs session from workspace"
                        );
                        Some(session)
                    }
                    Err(e) => {
                        debug!(
                            conversation_id = %ctx.conversation_id,
                            error = %e,
                            "No existing aionrs session found, starting fresh"
                        );
                        None
                    }
                }
            }
        }
    };

    let config = AionrsResolvedConfig {
        provider,
        api_key,
        model: model_id,
        base_url,
        system_prompt: overrides.system_prompt,
        max_tokens: overrides.max_tokens,
        max_turns: overrides.max_turns,
        compat_overrides,
        session_directory,
        session_mode: overrides.session_mode,
        extra_mcp_servers,
        bedrock_config,
    };

    let agent = AionrsAgentManager::new(ctx.conversation_id, ctx.workspace, config, resume_session).await?;
    Ok(AgentInstance::Aionrs(Arc::new(agent)))
}

/// Map AionUi DB platform name to the aionrs provider identifier.
///
/// Mirrors the frontend `src/process/agent/aionrs/envBuilder.ts` mapping.
/// For `new-api` platform, per-model protocol overrides from `model_protocols`
/// JSON take precedence.
fn map_aionrs_provider(platform: &str, model_id: &str, model_protocols: Option<&str>) -> String {
    if platform == "new-api"
        && let Some(protocols_json) = model_protocols
        && let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(protocols_json)
        && let Some(serde_json::Value::String(protocol)) = map.get(model_id)
        && protocol == "anthropic"
    {
        return "anthropic".to_owned();
    }

    match platform {
        "anthropic" => "anthropic",
        "bedrock" => "bedrock",
        "gemini-vertex-ai" => "vertex",
        _ => "openai",
    }
    .to_owned()
}

/// Resolve base_url and compat overrides for the aionrs provider.
///
/// Mirrors the frontend `envBuilder.ts` logic:
/// - Strips trailing `/v1` from base_url (aionrs appends its own path)
/// - Gemini: prepends `/v1beta/openai` and overrides `api_path`
/// - OpenAI official (`api.openai.com`): sets `max_completion_tokens`
fn resolve_aionrs_url_and_compat(
    platform: &str,
    raw_base_url: &str,
    mapped_provider: &str,
) -> (Option<String>, AionrsCompatOverrides) {
    let mut compat = AionrsCompatOverrides::default();

    if platform == "gemini" {
        let trimmed = raw_base_url.trim_end_matches('/');
        let base = format!("{trimmed}/v1beta/openai");
        compat.api_path = Some("/chat/completions".to_owned());
        return (Some(base), compat);
    }

    let normalized = normalize_aionrs_base_url(raw_base_url);
    let base_url = Some(normalized).filter(|u| !u.is_empty());

    if mapped_provider == "openai" && is_openai_host(raw_base_url) {
        compat.max_tokens_field = Some("max_completion_tokens".to_owned());
    }

    (base_url, compat)
}

fn is_openai_host(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .map(|rest| rest == "api.openai.com" || rest.starts_with("api.openai.com/"))
        .unwrap_or(false)
}

/// Strip trailing `/v1`, `/v1/`, or lone `/` from a base URL so that
/// aionrs can append its own path suffix (`/v1/messages`, `/v1/chat/completions`).
fn normalize_aionrs_base_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    trimmed.strip_suffix("/v1").unwrap_or(trimmed).to_owned()
}

fn resolve_bedrock_config(json: Option<&str>) -> Option<aion_config::config::BedrockConfig> {
    let bc: aionui_api_types::BedrockConfig = serde_json::from_str(json?).ok()?;
    Some(aion_config::config::BedrockConfig {
        region: Some(bc.region),
        access_key_id: bc.access_key_id,
        secret_access_key: bc.secret_access_key,
        session_token: None,
        profile: bc.profile,
    })
}

fn resolve_mcp_servers(overrides: &AionrsBuildExtra, conversation_id: &str) -> HashMap<String, McpServerConfig> {
    if let Some(cfg) = &overrides.team_mcp_stdio_config {
        return team_mcp_to_config(cfg);
    }
    if let Some(guide_cfg) = &overrides.guide_mcp_config
        && overrides
            .backend
            .as_deref()
            .is_some_and(|b| TEAM_CAPABLE_BACKENDS.contains(&b))
    {
        return guide_mcp_to_config(guide_cfg, overrides, conversation_id);
    }
    HashMap::new()
}

fn team_mcp_to_config(cfg: &TeamMcpStdioConfig) -> HashMap<String, McpServerConfig> {
    let mut env = HashMap::new();
    env.insert(TeamMcpStdioConfig::ENV_PORT.into(), cfg.port.to_string());
    env.insert(TeamMcpStdioConfig::ENV_TOKEN.into(), cfg.token.clone());
    env.insert(TeamMcpStdioConfig::ENV_SLOT_ID.into(), cfg.slot_id.clone());

    let server = McpServerConfig {
        transport: TransportType::Stdio,
        command: Some(cfg.binary_path.clone()),
        args: Some(vec!["mcp-team-stdio".into()]),
        env: Some(env),
        url: None,
        headers: None,
        deferred: Some(false),
    };

    HashMap::from([(format!("aionui-team-{}", cfg.team_id), server)])
}

fn guide_mcp_to_config(
    cfg: &GuideMcpConfig,
    overrides: &AionrsBuildExtra,
    conversation_id: &str,
) -> HashMap<String, McpServerConfig> {
    let mut env = HashMap::new();
    env.insert("AION_MCP_PORT".into(), cfg.port.to_string());
    env.insert("AION_MCP_TOKEN".into(), cfg.token.clone());
    env.insert("AION_MCP_BACKEND".into(), overrides.backend.clone().unwrap_or_default());
    env.insert("AION_MCP_CONVERSATION_ID".into(), conversation_id.to_owned());
    env.insert("AION_MCP_USER_ID".into(), overrides.user_id.clone().unwrap_or_default());

    let server = McpServerConfig {
        transport: TransportType::Stdio,
        command: Some(cfg.binary_path.clone()),
        args: Some(vec!["mcp-guide-stdio".into()]),
        env: Some(env),
        url: None,
        headers: None,
        deferred: Some(false),
    };

    HashMap::from([("aionui-team-guide".into(), server)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_aionrs_base_url_strips_v1() {
        assert_eq!(
            normalize_aionrs_base_url("https://api.openai.com/v1"),
            "https://api.openai.com"
        );
        assert_eq!(
            normalize_aionrs_base_url("https://api.openai.com/v1/"),
            "https://api.openai.com"
        );
        assert_eq!(
            normalize_aionrs_base_url("https://api.anthropic.com"),
            "https://api.anthropic.com"
        );
        assert_eq!(
            normalize_aionrs_base_url("https://api.deepseek.com/"),
            "https://api.deepseek.com"
        );
        assert_eq!(
            normalize_aionrs_base_url("http://localhost:11434"),
            "http://localhost:11434"
        );
        assert_eq!(normalize_aionrs_base_url(""), "");
    }

    #[test]
    fn map_aionrs_provider_known_platforms() {
        assert_eq!(map_aionrs_provider("anthropic", "m", None), "anthropic");
        assert_eq!(map_aionrs_provider("bedrock", "m", None), "bedrock");
        assert_eq!(map_aionrs_provider("gemini-vertex-ai", "m", None), "vertex");
    }

    #[test]
    fn map_aionrs_provider_custom_and_others_default_to_openai() {
        assert_eq!(map_aionrs_provider("custom", "gpt-4o", None), "openai");
        assert_eq!(map_aionrs_provider("gemini", "gemini-2.5-pro", None), "openai");
        assert_eq!(map_aionrs_provider("new-api", "m", None), "openai");
        assert_eq!(map_aionrs_provider("unknown", "m", None), "openai");
    }

    #[test]
    fn map_aionrs_provider_new_api_with_anthropic_protocol() {
        let protocols = r#"{"claude-sonnet":"anthropic","gpt-4o":"openai"}"#;
        assert_eq!(
            map_aionrs_provider("new-api", "claude-sonnet", Some(protocols)),
            "anthropic"
        );
        assert_eq!(map_aionrs_provider("new-api", "gpt-4o", Some(protocols)), "openai");
        assert_eq!(
            map_aionrs_provider("new-api", "unknown-model", Some(protocols)),
            "openai"
        );
    }

    #[test]
    fn map_aionrs_provider_new_api_with_invalid_json() {
        assert_eq!(map_aionrs_provider("new-api", "m", Some("not json")), "openai");
    }

    #[test]
    fn map_aionrs_provider_non_new_api_ignores_protocols() {
        let protocols = r#"{"m":"anthropic"}"#;
        assert_eq!(map_aionrs_provider("custom", "m", Some(protocols)), "openai");
    }

    #[test]
    fn is_openai_host_detects_official_api() {
        assert!(is_openai_host("https://api.openai.com/v1"));
        assert!(is_openai_host("https://api.openai.com"));
        assert!(is_openai_host("https://API.OPENAI.COM/v1"));
        assert!(!is_openai_host("https://api.deepseek.com/v1"));
        assert!(!is_openai_host("https://openai.example.com/v1"));
        assert!(!is_openai_host(""));
        assert!(!is_openai_host("not-a-url"));
    }

    #[test]
    fn resolve_openai_official_sets_max_completion_tokens() {
        let (base_url, compat) = resolve_aionrs_url_and_compat("custom", "https://api.openai.com/v1", "openai");
        assert_eq!(base_url.as_deref(), Some("https://api.openai.com"));
        assert_eq!(compat.max_tokens_field.as_deref(), Some("max_completion_tokens"));
        assert!(compat.api_path.is_none());
    }

    #[test]
    fn resolve_non_openai_keeps_default_max_tokens() {
        let (base_url, compat) = resolve_aionrs_url_and_compat("custom", "https://api.deepseek.com/v1", "openai");
        assert_eq!(base_url.as_deref(), Some("https://api.deepseek.com"));
        assert!(compat.max_tokens_field.is_none());
    }

    #[test]
    fn resolve_gemini_prepends_path_and_sets_api_path() {
        let (base_url, compat) =
            resolve_aionrs_url_and_compat("gemini", "https://generativelanguage.googleapis.com", "openai");
        assert_eq!(
            base_url.as_deref(),
            Some("https://generativelanguage.googleapis.com/v1beta/openai")
        );
        assert_eq!(compat.api_path.as_deref(), Some("/chat/completions"));
        assert!(compat.max_tokens_field.is_none());
    }

    #[test]
    fn resolve_anthropic_no_compat_overrides() {
        let (base_url, compat) = resolve_aionrs_url_and_compat("anthropic", "https://api.anthropic.com", "anthropic");
        assert_eq!(base_url.as_deref(), Some("https://api.anthropic.com"));
        assert!(compat.max_tokens_field.is_none());
        assert!(compat.api_path.is_none());
    }

    #[test]
    fn resolve_mcp_servers_team_takes_priority() {
        let overrides = AionrsBuildExtra {
            team_mcp_stdio_config: Some(TeamMcpStdioConfig {
                team_id: "team-42".into(),
                port: 9000,
                token: "tok".into(),
                slot_id: "slot-1".into(),
                binary_path: "/usr/bin/backend".into(),
            }),
            guide_mcp_config: Some(GuideMcpConfig {
                port: 8000,
                token: "guide-tok".into(),
                binary_path: "/usr/bin/backend".into(),
            }),
            backend: Some("aionrs".into()),
            ..Default::default()
        };

        let result = resolve_mcp_servers(&overrides, "conv-1");
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("aionui-team-team-42"));

        let server = &result["aionui-team-team-42"];
        assert_eq!(server.transport, TransportType::Stdio);
        assert_eq!(server.command.as_deref(), Some("/usr/bin/backend"));
        assert_eq!(server.args.as_deref(), Some(&["mcp-team-stdio".to_owned()][..]));
        assert_eq!(server.deferred, Some(false));

        let env = server.env.as_ref().unwrap();
        assert_eq!(env.get("TEAM_MCP_PORT"), Some(&"9000".to_owned()));
        assert_eq!(env.get("TEAM_MCP_TOKEN"), Some(&"tok".to_owned()));
        assert_eq!(env.get("TEAM_AGENT_SLOT_ID"), Some(&"slot-1".to_owned()));
    }

    #[test]
    fn resolve_mcp_servers_guide_when_no_team() {
        let overrides = AionrsBuildExtra {
            guide_mcp_config: Some(GuideMcpConfig {
                port: 8000,
                token: "guide-tok".into(),
                binary_path: "/usr/bin/backend".into(),
            }),
            backend: Some("aionrs".into()),
            user_id: Some("user-1".into()),
            ..Default::default()
        };

        let result = resolve_mcp_servers(&overrides, "conv-2");
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("aionui-team-guide"));

        let server = &result["aionui-team-guide"];
        assert_eq!(server.transport, TransportType::Stdio);
        assert_eq!(server.command.as_deref(), Some("/usr/bin/backend"));
        assert_eq!(server.args.as_deref(), Some(&["mcp-guide-stdio".to_owned()][..]));
        assert_eq!(server.deferred, Some(false));

        let env = server.env.as_ref().unwrap();
        assert_eq!(env.get("AION_MCP_PORT"), Some(&"8000".to_owned()));
        assert_eq!(env.get("AION_MCP_TOKEN"), Some(&"guide-tok".to_owned()));
        assert_eq!(env.get("AION_MCP_BACKEND"), Some(&"aionrs".to_owned()));
        assert_eq!(env.get("AION_MCP_CONVERSATION_ID"), Some(&"conv-2".to_owned()));
        assert_eq!(env.get("AION_MCP_USER_ID"), Some(&"user-1".to_owned()));
    }

    #[test]
    fn resolve_mcp_servers_empty_when_no_config() {
        let overrides = AionrsBuildExtra::default();
        let result = resolve_mcp_servers(&overrides, "conv-3");
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_mcp_servers_guide_skipped_for_unknown_backend() {
        let overrides = AionrsBuildExtra {
            guide_mcp_config: Some(GuideMcpConfig {
                port: 8000,
                token: "tok".into(),
                binary_path: "/bin/x".into(),
            }),
            backend: Some("unknown-vendor".into()),
            ..Default::default()
        };

        let result = resolve_mcp_servers(&overrides, "conv-4");
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_mcp_servers_guide_skipped_when_backend_none() {
        let overrides = AionrsBuildExtra {
            guide_mcp_config: Some(GuideMcpConfig {
                port: 8000,
                token: "tok".into(),
                binary_path: "/bin/x".into(),
            }),
            backend: None,
            ..Default::default()
        };

        let result = resolve_mcp_servers(&overrides, "conv-5");
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_bedrock_config_access_key() {
        let json = r#"{"auth_method":"accessKey","region":"us-west-2","access_key_id":"AKIA123","secret_access_key":"secret456"}"#;
        let result = resolve_bedrock_config(Some(json)).unwrap();
        assert_eq!(result.region.as_deref(), Some("us-west-2"));
        assert_eq!(result.access_key_id.as_deref(), Some("AKIA123"));
        assert_eq!(result.secret_access_key.as_deref(), Some("secret456"));
        assert!(result.profile.is_none());
        assert!(result.session_token.is_none());
    }

    #[test]
    fn resolve_bedrock_config_profile() {
        let json = r#"{"auth_method":"profile","region":"eu-west-1","profile":"my-profile"}"#;
        let result = resolve_bedrock_config(Some(json)).unwrap();
        assert_eq!(result.region.as_deref(), Some("eu-west-1"));
        assert_eq!(result.profile.as_deref(), Some("my-profile"));
        assert!(result.access_key_id.is_none());
        assert!(result.secret_access_key.is_none());
    }

    #[test]
    fn resolve_bedrock_config_none_when_json_missing() {
        assert!(resolve_bedrock_config(None).is_none());
    }

    #[test]
    fn resolve_bedrock_config_none_when_json_invalid() {
        assert!(resolve_bedrock_config(Some("not-json")).is_none());
    }

    #[test]
    fn preset_rules_merged_into_system_prompt_when_no_existing() {
        let json = serde_json::json!({
            "preset_rules": "You are a data analyst. Always use Python.",
        });
        let mut overrides: AionrsBuildExtra = serde_json::from_value(json).unwrap();

        if let Some(rules) = overrides.preset_rules.take() {
            overrides.system_prompt = Some(match overrides.system_prompt.take() {
                Some(existing) => format!("{existing}\n\n{rules}"),
                None => rules,
            });
        }

        assert_eq!(
            overrides.system_prompt.as_deref(),
            Some("You are a data analyst. Always use Python.")
        );
        assert!(overrides.preset_rules.is_none());
    }

    #[test]
    fn preset_rules_appended_to_existing_system_prompt() {
        let json = serde_json::json!({
            "system_prompt": "Be concise.",
            "preset_rules": "You are a data analyst.",
        });
        let mut overrides: AionrsBuildExtra = serde_json::from_value(json).unwrap();

        if let Some(rules) = overrides.preset_rules.take() {
            overrides.system_prompt = Some(match overrides.system_prompt.take() {
                Some(existing) => format!("{existing}\n\n{rules}"),
                None => rules,
            });
        }

        assert_eq!(
            overrides.system_prompt.as_deref(),
            Some("Be concise.\n\nYou are a data analyst.")
        );
    }

    #[test]
    fn no_preset_rules_leaves_system_prompt_unchanged() {
        let json = serde_json::json!({
            "system_prompt": "Be concise.",
        });
        let mut overrides: AionrsBuildExtra = serde_json::from_value(json).unwrap();

        if let Some(rules) = overrides.preset_rules.take() {
            overrides.system_prompt = Some(match overrides.system_prompt.take() {
                Some(existing) => format!("{existing}\n\n{rules}"),
                None => rules,
            });
        }

        assert_eq!(overrides.system_prompt.as_deref(), Some("Be concise."));
    }
}
