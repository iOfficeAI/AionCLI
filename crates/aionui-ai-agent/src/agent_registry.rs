use std::collections::HashSet;

use aionui_api_types::{AgentSource, DetectedAgent};
use aionui_common::{AcpBackend, AgentType};
use tokio::sync::RwLock;
use tracing::{debug, info};

pub struct AgentRegistry {
    agents: RwLock<Vec<DetectedAgent>>,
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: RwLock::new(Vec::new()),
        }
    }

    /// Run builtin detection + merge injected agents, then populate the registry.
    pub async fn initialize(&self, extensions: Vec<DetectedAgent>, customs: Vec<DetectedAgent>) {
        let internal = Self::detect_internal_agent();
        let builtins = Self::detect_builtin_agents();

        let merged = Self::merge(customs, extensions, builtins, vec![internal]);

        info!(
            count = merged.len(),
            names = %merged.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "),
            "Agent registry initialized"
        );

        *self.agents.write().await = merged;
    }

    pub async fn get_all(&self) -> Vec<DetectedAgent> {
        self.agents.read().await.clone()
    }

    pub async fn get_by_id(&self, id: &str) -> Option<DetectedAgent> {
        self.agents
            .read()
            .await
            .iter()
            .find(|a| a.id == id)
            .cloned()
    }

    /// Re-scan builtin agents only (PATH may have changed).
    pub async fn refresh_builtins(&self) {
        let mut agents = self.agents.write().await;
        agents.retain(|a| a.source != AgentSource::Builtin);
        let builtins = Self::detect_builtin_agents();
        let mut merged = std::mem::take(&mut *agents);
        merged.extend(builtins);
        *agents = Self::deduplicate(merged);
    }

    /// Replace extension-contributed agents.
    pub async fn refresh_extensions(&self, extensions: Vec<DetectedAgent>) {
        let mut agents = self.agents.write().await;
        agents.retain(|a| a.source != AgentSource::Extension);
        let mut merged = std::mem::take(&mut *agents);
        merged.extend(extensions);
        *agents = Self::deduplicate(merged);
    }

    /// Replace custom agents.
    pub async fn refresh_customs(&self, customs: Vec<DetectedAgent>) {
        let mut agents = self.agents.write().await;
        agents.retain(|a| a.source != AgentSource::Custom);
        let mut merged = std::mem::take(&mut *agents);
        merged.extend(customs);
        *agents = Self::deduplicate(merged);
    }

    fn detect_internal_agent() -> DetectedAgent {
        DetectedAgent {
            id: AgentType::Aionrs.id(),
            name: "Aion CLI".into(),
            agent_type: AgentType::Aionrs,
            backend: None,
            available: true,
            source: AgentSource::Internal,
            command: None,
            args: vec![],
            env: vec![],
        }
    }

    /// Detect all builtin agents by scanning the system PATH.
    ///
    /// Each AgentType has its own detection logic — ACP backends are detected
    /// via CLI_BACKENDS, while other agent types have dedicated detectors.
    fn detect_builtin_agents() -> Vec<DetectedAgent> {
        let mut agents = Self::detect_acp_agents();
        agents.extend(Self::detect_openclaw());
        agents.extend(Self::detect_nanobot());
        agents
    }

    fn detect_acp_agents() -> Vec<DetectedAgent> {
        // TODO: use bundled bun instead of PATH bun.
        let bun_path = which::which("bun").ok();

        AcpBackend::CLI_BACKENDS
            .iter()
            .filter_map(|&backend| {
                if let Some(bridge_pkg) = backend.bridge_package() {
                    let bun = bun_path.as_ref()?;
                    let bun_cmd = bun.to_string_lossy().into_owned();

                    if let Some(binary) = backend.binary_name() {
                        which::which(binary).ok()?;
                    }

                    let mut args = vec![
                        "x".to_owned(),
                        "--bun".to_owned(),
                        bridge_pkg.to_owned(),
                    ];
                    for extra in backend.bridge_extra_args() {
                        args.push((*extra).to_owned());
                    }

                    debug!(backend = ?backend, command = %bun_cmd, ?args, "Detected bridge-based agent");

                    Some(DetectedAgent {
                        id: backend.id(),
                        name: backend.display_name().into(),
                        agent_type: AgentType::Acp,
                        backend: Some(backend),
                        available: true,
                        source: AgentSource::Builtin,
                        command: Some(bun_cmd),
                        args,
                        env: vec![],
                    })
                } else {
                    let binary = backend.binary_name()?;
                    let path = which::which(binary).ok()?;
                    let command = path.to_string_lossy().into_owned();
                    let args = backend.args().unwrap_or(&["--experimental-acp"]);

                    debug!(backend = ?backend, %command, ?args, "Detected direct-CLI agent");

                    Some(DetectedAgent {
                        id: backend.id(),
                        name: backend.display_name().into(),
                        agent_type: AgentType::Acp,
                        backend: Some(backend),
                        available: true,
                        source: AgentSource::Builtin,
                        command: Some(command),
                        args: args.iter().map(|s| (*s).to_owned()).collect(),
                        env: vec![],
                    })
                }
            })
            .collect()
    }

    fn detect_openclaw() -> Option<DetectedAgent> {
        let path = which::which("openclaw").ok()?;
        let command = path.to_string_lossy().into_owned();

        debug!(%command, "Detected OpenClaw Gateway agent");

        Some(DetectedAgent {
            id: AgentType::OpenclawGateway.id(),
            name: "OpenClaw Gateway".into(),
            agent_type: AgentType::OpenclawGateway,
            backend: None,
            available: true,
            source: AgentSource::Builtin,
            command: Some(command),
            args: vec![],
            env: vec![],
        })
    }

    fn detect_nanobot() -> Option<DetectedAgent> {
        let path = which::which("nanobot").ok()?;
        let command = path.to_string_lossy().into_owned();

        debug!(%command, "Detected Nanobot agent");

        Some(DetectedAgent {
            id: AgentType::Nanobot.id(),
            name: "Nanobot".into(),
            agent_type: AgentType::Nanobot,
            backend: None,
            available: true,
            source: AgentSource::Builtin,
            command: Some(command),
            args: vec!["--experimental-acp".to_owned()],
            env: vec![],
        })
    }

    /// Merge all sources with priority: Custom > Extension > Builtin > Internal.
    fn merge(
        customs: Vec<DetectedAgent>,
        extensions: Vec<DetectedAgent>,
        builtins: Vec<DetectedAgent>,
        internals: Vec<DetectedAgent>,
    ) -> Vec<DetectedAgent> {
        let mut all = Vec::new();
        all.extend(customs);
        all.extend(extensions);
        all.extend(builtins);
        all.extend(internals);
        Self::deduplicate(all)
    }

    /// Deduplicate agents. First occurrence wins (priority order preserved by caller).
    /// Custom and Extension agents use their `id` as dedup key (multiple custom agents
    /// can share the same backend). Builtin/Internal dedup by backend (for ACP) or
    /// agent_type (for non-ACP).
    fn deduplicate(agents: Vec<DetectedAgent>) -> Vec<DetectedAgent> {
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for agent in agents {
            let key = match agent.source {
                AgentSource::Custom | AgentSource::Extension => agent.id.clone(),
                _ => match agent.backend {
                    Some(b) => format!("acp:{b:?}"),
                    None => format!("type:{:?}", agent.agent_type),
                },
            };
            if seen.insert(key) {
                result.push(agent);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_acp_agent(id: &str, backend: AcpBackend, source: AgentSource) -> DetectedAgent {
        DetectedAgent {
            id: id.into(),
            name: id.into(),
            agent_type: AgentType::Acp,
            backend: Some(backend),
            available: true,
            source,
            command: None,
            args: vec![],
            env: vec![],
        }
    }

    fn make_typed_agent(id: &str, agent_type: AgentType, source: AgentSource) -> DetectedAgent {
        DetectedAgent {
            id: id.into(),
            name: id.into(),
            agent_type,
            backend: None,
            available: true,
            source,
            command: None,
            args: vec![],
            env: vec![],
        }
    }

    #[test]
    fn merge_priority_custom_over_builtin() {
        let customs = vec![make_acp_agent(
            "custom:claude",
            AcpBackend::Claude,
            AgentSource::Custom,
        )];
        let builtins = vec![make_acp_agent(
            "builtin-claude",
            AcpBackend::Claude,
            AgentSource::Builtin,
        )];
        let merged = AgentRegistry::merge(customs, vec![], builtins, vec![]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].source, AgentSource::Custom);
    }

    #[test]
    fn merge_deduplicates_same_backend_builtins() {
        let builtins = vec![
            make_acp_agent("a", AcpBackend::Claude, AgentSource::Builtin),
            make_acp_agent("b", AcpBackend::Claude, AgentSource::Builtin),
        ];
        let merged = AgentRegistry::merge(vec![], vec![], builtins, vec![]);
        let claude_count = merged
            .iter()
            .filter(|a| a.backend == Some(AcpBackend::Claude) && a.source == AgentSource::Builtin)
            .count();
        assert_eq!(claude_count, 1);
        assert_eq!(merged[0].id, "a");
    }

    #[test]
    fn merge_keeps_internal_at_end() {
        let internals = vec![make_typed_agent(
            "aionrs",
            AgentType::Aionrs,
            AgentSource::Internal,
        )];
        let builtins = vec![make_acp_agent(
            "claude",
            AcpBackend::Claude,
            AgentSource::Builtin,
        )];
        let merged = AgentRegistry::merge(vec![], vec![], builtins, internals);
        assert_eq!(merged.last().unwrap().source, AgentSource::Internal);
    }

    #[test]
    fn dedup_non_acp_by_agent_type() {
        let agents = vec![
            make_typed_agent("oc1", AgentType::OpenclawGateway, AgentSource::Builtin),
            make_typed_agent("oc2", AgentType::OpenclawGateway, AgentSource::Builtin),
        ];
        let deduped = AgentRegistry::deduplicate(agents);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].id, "oc1");
    }

    #[tokio::test]
    async fn registry_initialize_and_get_all() {
        let registry = AgentRegistry::new();
        registry.initialize(vec![], vec![]).await;
        let agents = registry.get_all().await;
        assert!(agents.iter().any(|a| a.agent_type == AgentType::Aionrs));
    }

    #[tokio::test]
    async fn registry_refresh_customs() {
        let registry = AgentRegistry::new();
        registry.initialize(vec![], vec![]).await;

        let before = registry.get_all().await;
        assert_eq!(
            before
                .iter()
                .filter(|a| a.source == AgentSource::Custom)
                .count(),
            0
        );

        let customs = vec![make_acp_agent(
            "custom:test",
            AcpBackend::Claude,
            AgentSource::Custom,
        )];
        registry.refresh_customs(customs).await;

        let after = registry.get_all().await;
        assert_eq!(
            after
                .iter()
                .filter(|a| a.source == AgentSource::Custom)
                .count(),
            1
        );
    }
}
