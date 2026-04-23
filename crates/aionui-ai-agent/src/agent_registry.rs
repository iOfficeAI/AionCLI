use std::collections::HashSet;

use aionui_api_types::{AgentSource, DetectedAgent};
use aionui_common::AcpBackend;
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
        self.agents.read().await.iter().find(|a| a.id == id).cloned()
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
            id: AcpBackend::Aionrs.id(),
            name: "Aion CLI".into(),
            backend: AcpBackend::Aionrs,
            available: true,
            source: AgentSource::Internal,
            command: None,
            args: vec![],
            env: vec![],
        }
    }

    fn detect_builtin_agents() -> Vec<DetectedAgent> {
        AcpBackend::CLI_BACKENDS
            .iter()
            .filter_map(|&backend| {
                let binary = backend.cli_binary_name()?;
                let path = which::which(binary).ok()?;
                let command = path.to_string_lossy().into_owned();

                debug!(backend = ?backend, %command, "Detected builtin agent");

                Some(DetectedAgent {
                    id: backend.id(),
                    name: backend.display_name().into(),
                    backend,
                    available: true,
                    source: AgentSource::Builtin,
                    command: Some(command),
                    args: vec![],
                    env: vec![],
                })
            })
            .collect()
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
    /// can share the same backend). Builtin/Internal dedup by backend.
    fn deduplicate(agents: Vec<DetectedAgent>) -> Vec<DetectedAgent> {
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for agent in agents {
            let key = match agent.source {
                AgentSource::Custom | AgentSource::Extension => agent.id.clone(),
                _ => format!("{:?}", agent.backend),
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

    fn make_agent(id: &str, backend: AcpBackend, source: AgentSource) -> DetectedAgent {
        DetectedAgent {
            id: id.into(),
            name: id.into(),
            backend,
            available: true,
            source,
            command: None,
            args: vec![],
            env: vec![],
        }
    }

    #[test]
    fn merge_priority_custom_over_builtin() {
        let customs = vec![make_agent(
            "custom:claude",
            AcpBackend::Claude,
            AgentSource::Custom,
        )];
        let builtins = vec![make_agent(
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
            make_agent("a", AcpBackend::Claude, AgentSource::Builtin),
            make_agent("b", AcpBackend::Claude, AgentSource::Builtin),
        ];
        let merged = AgentRegistry::merge(vec![], vec![], builtins, vec![]);
        let claude_count = merged
            .iter()
            .filter(|a| a.backend == AcpBackend::Claude && a.source == AgentSource::Builtin)
            .count();
        assert_eq!(claude_count, 1);
        assert_eq!(merged[0].id, "a");
    }

    #[test]
    fn merge_keeps_internal_at_end() {
        let internals = vec![make_agent(
            "aionrs",
            AcpBackend::Aionrs,
            AgentSource::Internal,
        )];
        let builtins = vec![make_agent(
            "claude",
            AcpBackend::Claude,
            AgentSource::Builtin,
        )];
        let merged = AgentRegistry::merge(vec![], vec![], builtins, internals);
        assert_eq!(merged.last().unwrap().source, AgentSource::Internal);
    }

    #[tokio::test]
    async fn registry_initialize_and_get_all() {
        let registry = AgentRegistry::new();
        registry.initialize(vec![], vec![]).await;
        let agents = registry.get_all().await;
        assert!(agents.iter().any(|a| a.backend == AcpBackend::Aionrs));
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

        let customs = vec![make_agent(
            "custom:test",
            AcpBackend::Custom,
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
