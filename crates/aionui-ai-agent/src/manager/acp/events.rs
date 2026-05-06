use std::collections::HashMap;

use crate::shared_kernel::{ConfigKey, ConfigValue, ModeId, ModelId, SessionId};

/// Domain events emitted by the `AcpSession` aggregate.
///
/// These capture *intent* changes (user wants mode X) and *observation*
/// arrivals (CLI reported mode Y) separately — persistence consumers can
/// decide which to write to DB without re-interpreting UI stream events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpSessionEvent {
    SessionAssigned {
        session_id: SessionId,
    },
    SessionOpened,
    DesiredModeChanged {
        mode_id: ModeId,
    },
    DesiredConfigChanged {
        selections: HashMap<ConfigKey, ConfigValue>,
    },
    ObservedModeSynced {
        mode_id: ModeId,
    },
    ObservedModelSynced {
        model_id: ModelId,
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
            mode_id: ModeId::new("plan"),
        };
        let dbg = format!("{e:?}");
        assert!(dbg.contains("plan"));
    }
}
