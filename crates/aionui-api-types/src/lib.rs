//! All HTTP request/response DTOs shared across the API surface.
mod acp;
mod agent_discovery;
mod auth;
mod channel;
mod confirmation;
mod connection_test;
mod conversation;
mod cron;
mod extension;
mod file;
mod lifecycle;
mod mcp;
mod office;
mod provider;
mod remote_agent;
mod response;
mod shell;
mod skill;
mod system;
mod team;
mod websocket;

pub use acp::{
    AcpAgentInfo, AcpEnvResponse, AcpHealthCheckRequest, AcpHealthCheckResponse, AcpModeResponse,
    DetectCliRequest, DetectCliResponse, ProbeModelRequest, SetConfigOptionRequest, SetModeRequest,
    SetModelRequest, TestCustomAgentRequest, TestCustomAgentResponse,
};
pub use agent_discovery::{AgentSource, DetectedAgent, EnvVar};
pub use auth::{
    AuthStatusResponse, ChangePasswordRequest, LoginRequest, LoginResponse, PublicUser,
    QrLoginRequest, RefreshResponse, RefreshTokenRequest, UserInfoResponse, WsTokenResponse,
};
pub use channel::{
    ApprovePairingRequest, BridgeResponse, ChannelAgentConfig, ChannelModelConfig,
    ChannelSessionResponse, ChannelUserResponse, DisablePluginRequest, EnablePluginRequest,
    PairingRequestResponse, PairingRequestedPayload, PluginStatusChangedPayload,
    PluginStatusResponse, RejectPairingRequest, RevokeUserRequest, SyncChannelSettingsRequest,
    TestPluginExtraConfig, TestPluginRequest, TestPluginResponse, UserAuthorizedPayload,
};
pub use confirmation::{
    ApprovalCheckQuery, ApprovalCheckResponse, ConfirmRequest, ConfirmationListResponse,
};
pub use connection_test::{
    GeminiSubscriptionData, GeminiSubscriptionQuery, TestBedrockConnectionRequest,
};
pub use conversation::{
    CloneConversationRequest, ConversationListResponse, ConversationResponse,
    CreateConversationRequest, ListConversationsQuery, ListMessagesQuery, MessageListResponse,
    MessageResponse, MessageSearchItem, MessageSearchResponse, SearchMessagesQuery,
    SendMessageRequest, UpdateConversationRequest,
};
pub use cron::{
    CreateCronJobRequest, CronAgentConfigDto, CronJobExecutedEvent, CronJobMetadataDto,
    CronJobPayloadDto, CronJobRemovedPayload, CronJobResponse, CronJobStateDto, CronJobTargetDto,
    CronScheduleDto, HasSkillResponse, ListCronJobsQuery, RunNowResponse, SaveCronSkillRequest,
    UpdateCronJobRequest,
};
pub use extension::{
    DisableExtensionRequest, EnableExtensionRequest, ExtensionSummaryResponse, GetI18nRequest,
    GetPermissionsRequest, GetRiskLevelRequest, HubExtensionListItem, HubExtensionListResponse,
    HubOperationResponse, HubUpdateInfo, InstallExtensionRequest, PermissionDetailResponse,
    PermissionSummaryResponse,
};
pub use file::{
    CancelZipRequest, CopyFilesRequest, CopyFilesResponse, CreateTempFileRequest,
    DirOrFileResponse, FetchRemoteImageRequest, FileChangeInfoResponse, FileMetadataResponse,
    FileWatchRequest, GetFileMetadataRequest, GetFilesByDirRequest, GetImageBase64Request,
    ListWorkspaceFilesRequest, ReadFileBufferRequest, ReadFileRequest, RemoveEntryRequest,
    RenameRequest, RenameResponse, SnapshotBaselineRequest, SnapshotCompareResponse,
    SnapshotDiscardRequest, SnapshotInfoResponse, SnapshotMode, SnapshotStageRequest,
    SnapshotWorkspaceRequest, WorkspaceFlatFileResponse, WorkspaceOfficeWatchRequest,
    WriteFileRequest, ZipFileEntry, ZipRequest,
};
pub use lifecycle::{
    GitHubReleaseAsset, SystemInfoResponse, UpdateCheckRequest, UpdateCheckResult,
    UpdateReleaseInfo,
};
pub use mcp::{
    BatchImportMcpServersRequest, CreateMcpServerRequest, DetectedMcpServerResponse,
    McpAgentSyncResult, McpAuthMethod, McpConnectionTestResult, McpServerResponse, McpSyncResult,
    McpToolResponse, McpTransport, OAuthCheckStatusRequest, OAuthLoginRequest, OAuthLoginResponse,
    OAuthLogoutRequest, OAuthStatusResponse, RemoveFromAgentsRequest, SyncToAgentsRequest,
    TestMcpConnectionRequest, UpdateMcpServerRequest,
};
pub use office::{
    CellCoord, CellRange, ConversionResultDto, ConversionTarget, DetectStarOfficeRequest,
    DocumentConversionRequest, DocumentConversionResponse, ExcelSheetData, ExcelSheetImage,
    ExcelWorkbookData, GetSnapshotContentRequest, ListSnapshotsRequest, PptJsonData, PptSlideData,
    PreviewHistoryTargetDto, PreviewSnapshotInfoDto, PreviewState, PreviewStatusEvent,
    PreviewUrlResponse, SaveSnapshotRequest, SnapshotContentResponse, StarOfficeDetectResponse,
    StartPreviewRequest, StopPreviewRequest,
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
pub use response::{ApiResponse, ErrorResponse};
pub use shell::{
    CheckToolInstalledRequest, CheckToolInstalledResponse, DeepgramSpeechToTextConfig,
    OpenAISpeechToTextConfig, OpenExternalRequest, OpenFileRequest, OpenFolderWithRequest,
    ShowItemInFolderRequest, SpeechToTextConfig, SpeechToTextProvider, SpeechToTextResult,
    ToolType,
};
pub use skill::{
    AddExternalPathRequest, DeleteSkillRequest, ExportSkillRequest, ExternalSkillSourceResponse,
    ImportSkillRequest, ImportSkillResponse, NamedPathResponse, ReadAssistantRuleRequest,
    ReadBuiltinResourceRequest, ReadSkillInfoRequest, ReadSkillInfoResponse,
    RemoveExternalPathRequest, ScanForSkillsRequest, ScanForSkillsResponse, ScannedSkillResponse,
    SkillListItemResponse, SkillPathsResponse, WriteAssistantRuleRequest,
};
pub use system::{
    ClientPreferencesResponse, SystemSettingsResponse, UpdateClientPreferencesRequest,
    UpdateSettingsRequest,
};
pub use team::{
    AddAgentRequest, CreateTeamRequest, RenameAgentRequest, RenameTeamRequest,
    SendAgentMessageRequest, SendTeamMessageRequest, TeamAgentInput, TeamAgentRemovedPayload,
    TeamAgentRenamedPayload, TeamAgentResponse, TeamAgentSpawnedPayload, TeamAgentStatusPayload,
    TeamListResponse, TeamResponse,
};
pub use websocket::WebSocketMessage;
