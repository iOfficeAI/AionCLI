mod acp;
mod auth;
mod confirmation;
mod connection_test;
mod conversation;
mod lifecycle;
mod provider;
mod remote_agent;
mod response;
mod system;
mod websocket;

pub use conversation::{
    CloneConversationRequest, ConversationListResponse, ConversationResponse,
    CreateConversationRequest, ListConversationsQuery, ListMessagesQuery, MessageListResponse,
    MessageResponse, MessageSearchItem, MessageSearchResponse, SearchMessagesQuery,
    SendMessageRequest, UpdateConversationRequest,
};
pub use confirmation::{
    ApprovalCheckQuery, ApprovalCheckResponse, ConfirmRequest, ConfirmationListResponse,
};
pub use auth::{
    AuthStatusResponse, ChangePasswordRequest, LoginRequest, LoginResponse, PublicUser,
    QrLoginRequest, RefreshResponse, RefreshTokenRequest, UserInfoResponse, WsTokenResponse,
};
pub use lifecycle::{
    GitHubReleaseAsset, SystemInfoResponse, UpdateCheckRequest, UpdateCheckResult,
    UpdateReleaseInfo,
};
pub use provider::{
    BedrockAuthMethod, BedrockConfig, CreateProviderRequest, DetectProtocolRequest,
    DetectionSuggestion, FetchModelsRequest, FetchModelsResponse, HealthStatus, KeyTestResult,
    ModelCapability, ModelHealthStatus, ModelInfo, ModelType, MultiKeyResult,
    ProtocolDetectionResponse, ProviderResponse, SuggestionType, UpdateProviderRequest,
};
pub use remote_agent::{
    CreateRemoteAgentRequest, HandshakeResponse, RemoteAgentListItem, RemoteAgentResponse,
    TestRemoteAgentConnectionRequest, UpdateRemoteAgentRequest,
};
pub use acp::{
    AcpAgentInfo, AcpEnvResponse, AcpHealthCheckRequest, AcpHealthCheckResponse,
    AcpModeResponse, DetectCliRequest, DetectCliResponse, ProbeModelRequest,
    SetConfigOptionRequest, SetModeRequest, SetModelRequest, TestCustomAgentRequest,
    TestCustomAgentResponse,
};
pub use response::{ApiResponse, ErrorResponse};
pub use system::{
    ClientPreferencesResponse, SystemSettingsResponse, UpdateClientPreferencesRequest,
    UpdateSettingsRequest,
};
pub use connection_test::{
    GeminiSubscriptionData, GeminiSubscriptionQuery, TestBedrockConnectionRequest,
};
pub use websocket::WebSocketMessage;
