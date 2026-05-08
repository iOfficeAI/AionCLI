use std::collections::HashMap;

use crate::shared_kernel::{ConfigKey, ConfigValue, ModeId, ModelId, SessionId};

/// Domain events emitted by the `AcpSession` aggregate.
///
/// These capture *intent* changes (user wants mode X) and *observation*
/// arrivals (CLI reported mode Y) separately — persistence consumers can
/// decide which to write to DB without re-interpreting UI stream events.
///
/// `context_usage_json` travels as a pre-serialised string so the event
/// type can keep `Eq` (SDK's `UsageUpdate` only derives `PartialEq`) and
/// so the persistence consumer can forward it to the DB without another
/// round-trip through serde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpSessionEvent {
    SessionOpened,
    SessionAssigned {
        session_id: SessionId,
    },
    DesiredModeChanged {
        mode: ModeId,
    },
    DesiredModelChanged {
        model: ModelId,
    },
    DesiredConfigChanged {
        selections: HashMap<ConfigKey, ConfigValue>,
    },
    ObservedModeSynced {
        mode: ModeId,
    },
    ObservedModelSynced {
        model: ModelId,
    },
    ObservedConfigSynced {
        selections: HashMap<ConfigKey, ConfigValue>,
    },
    ObservedContextUsageChanged {
        usage_json: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_equality() {
        let a = AcpSessionEvent::SessionAssigned {
            session_id: SessionId::new("s1"),
        };
        let b = AcpSessionEvent::SessionAssigned {
            session_id: SessionId::new("s1"),
        };
        assert_eq!(a, b);
    }

    #[test]
    fn event_debug_format() {
        let e = AcpSessionEvent::DesiredModeChanged {
            mode: ModeId::new("plan"),
        };
        let dbg = format!("{e:?}");
        assert!(dbg.contains("plan"));
    }
}
