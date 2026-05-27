//! Connect-layer trait abstractions.

mod trait_def;

pub use trait_def::{
    ChunkPayload, ConnectorError, ConnectorEvent, ExitInfo, IAgentConnector, StopReason, ToolUsePayload, TurnSummary,
};
