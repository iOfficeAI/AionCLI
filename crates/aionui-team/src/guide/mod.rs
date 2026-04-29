//! Team guide module: capability descriptor + lead-facing MCP tool argument parsing.
pub mod capability;
pub mod handlers;

pub use handlers::{
    CreateTeamParams, build_create_team_request, format_create_team_response, handle_aion_create_team,
    parse_create_team_args,
};
