//! Connect-layer trait abstractions. Phase 1 of the conversation
//! refactor (see docs/superpowers/specs/2026-05-26-conversation-layer-refactor-design.md).

mod trait_def;

pub use trait_def::{
    ChunkPayload, ConnectorError, ConnectorEvent, ExitInfo, IAgentConnector, StopReason, ToolUsePayload, TurnSummary,
};
