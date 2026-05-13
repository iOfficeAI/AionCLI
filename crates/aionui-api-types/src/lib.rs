//! All HTTP request/response DTOs shared across the API surface.
mod acp;
mod agent_build_extra;
mod agent_discovery;
mod assistant;
mod auth;
mod channel;
mod confirmation;
mod connection_test;
mod conversation;
mod cron;
mod custom_agent;
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
mod team_mcp;
mod websocket;

pub use acp::{
    AcpEnvResponse, AcpHealthCheckRequest, AcpHealthCheckResponse, AgentModeResponse, DetectCliRequest,
    DetectCliResponse, GetModelInfoResponse, ModelInfoEntry, ModelInfoPayload, ProbeModelRequest, SetModeRequest,
    SetModelRequest, SideQuestionRequest, SideQuestionResponse, TryConnectCustomAgentRequest,
    TryConnectCustomAgentResponse, WorkspaceBrowseQuery, WorkspaceEntry,
};
pub use agent_build_extra::{
    AcpBuildExtra, AcpModelInfo, AionrsBuildExtra, OpenClawBuildExtra, OpenClawGatewayConfig, RemoteBuildExtra,
    SlashCommandItem,
};
pub use agent_discovery::{AgentEnvEntry, AgentHandshake, AgentMetadata, AgentSource, AgentSourceInfo, BehaviorPolicy};
pub use assistant::{
    AssistantResponse, AssistantSource, CreateAssistantRequest, ImportAssistantsRequest, ImportAssistantsResult,
    ImportError, SetAssistantStateRequest, UpdateAssistantRequest,
};
pub use auth::{
    AuthStatusResponse, ChangePasswordRequest, LoginRequest, LoginResponse, PublicUser, QrLoginRequest,
    RefreshResponse, RefreshTokenRequest, UserInfoResponse, WebuiChangePasswordRequest, WebuiChangeUsernameRequest,
    WebuiChangeUsernameResponse, WebuiGenerateQrTokenResponse, WebuiResetPasswordResponse, WsTokenResponse,
};
pub use channel::{
    ApprovePairingRequest, BridgeResponse, ChannelSessionResponse, ChannelUserResponse, DisablePluginRequest,
    EnablePluginRequest, PairingRequestResponse, PairingRequestedPayload, PluginStatusChangedPayload,
    PluginStatusResponse, RejectPairingRequest, RevokeUserRequest, SyncChannelSettingsRequest, TestPluginExtraConfig,
    TestPluginRequest, TestPluginResponse, UserAuthorizedPayload,
};
pub use confirmation::{ApprovalCheckQuery, ApprovalCheckResponse, ConfirmRequest, ConfirmationListResponse};
pub use connection_test::TestBedrockConnectionRequest;
pub use conversation::{
    ActiveCountResponse, CloneConversationRequest, ConversationArtifactKind, ConversationArtifactListResponse,
    ConversationArtifactResponse, ConversationArtifactStatus, ConversationListResponse, ConversationResponse,
    CreateConversationRequest, ListConversationsQuery, ListMessagesQuery, MessageListResponse, MessageResponse,
    MessageSearchItem, MessageSearchResponse, SearchMessagesQuery, SendMessageRequest, SendMessageResponse,
    UpdateConversationArtifactRequest, UpdateConversationRequest,
};
pub use cron::{
    CreateCronJobRequest, CronAgentConfigDto, CronJobExecutedEvent, CronJobMetadataDto, CronJobPayloadDto,
    CronJobRemovedPayload, CronJobResponse, CronJobStateDto, CronJobTargetDto, CronScheduleDto, HasSkillResponse,
    ListCronJobsQuery, RunNowResponse, SaveCronSkillRequest, UpdateCronJobRequest,
};
pub use custom_agent::{
    CustomAgentAdvancedOverrides, CustomAgentUpsertRequest, DeleteCustomAgentResponse, SetEnabledRequest,
};
pub use extension::{
    DisableExtensionRequest, EnableExtensionRequest, ExtensionSummaryResponse, GetI18nRequest, GetPermissionsRequest,
    GetRiskLevelRequest, HubExtensionListItem, HubExtensionListResponse, HubOperationResponse, HubUpdateInfo,
    InstallExtensionRequest, PermissionDetailResponse, PermissionSummaryResponse,
};
pub use file::{
    BrowseDirectoryQuery, BrowseDirectoryResponse, BrowseEntry, CancelZipRequest, CopyFilesRequest, CopyFilesResponse,
    CreateTempFileRequest, DirOrFileResponse, FetchRemoteImageRequest, FileChangeInfoResponse, FileMetadataResponse,
    FileWatchRequest, GetFileMetadataRequest, GetFilesByDirRequest, GetImageBase64Request, ListWorkspaceFilesRequest,
    ReadFileBufferRequest, ReadFileRequest, RemoveEntryRequest, RenameRequest, RenameResponse, SnapshotBaselineRequest,
    SnapshotCompareResponse, SnapshotDiscardRequest, SnapshotInfoResponse, SnapshotMode, SnapshotStageRequest,
    SnapshotWorkspaceRequest, WorkspaceFlatFileResponse, WorkspaceOfficeWatchRequest, WriteFileRequest, ZipFileEntry,
    ZipRequest,
};
pub use lifecycle::{GitHubReleaseAsset, SystemInfoResponse, UpdateCheckRequest, UpdateCheckResult, UpdateReleaseInfo};
pub use mcp::{
    BatchImportMcpServersRequest, CreateMcpServerRequest, DetectedMcpServerResponse, McpAgentSyncResult, McpAuthMethod,
    McpConnectionTestResult, McpServerResponse, McpSyncResult, McpToolResponse, McpTransport, OAuthCheckStatusRequest,
    OAuthLoginRequest, OAuthLoginResponse, OAuthLogoutRequest, OAuthStatusResponse, RemoveFromAgentsRequest,
    SyncToAgentsRequest, TestMcpConnectionRequest, UpdateMcpServerRequest,
};
pub use office::{
    CellCoord, CellRange, ConversionResultDto, ConversionTarget, DetectStarOfficeRequest, DocumentConversionRequest,
    DocumentConversionResponse, ExcelSheetData, ExcelSheetImage, ExcelWorkbookData, GetSnapshotContentRequest,
    ListSnapshotsRequest, PptJsonData, PptSlideData, PreviewHistoryTargetDto, PreviewSnapshotInfoDto, PreviewState,
    PreviewStatusEvent, PreviewUrlResponse, SaveSnapshotRequest, SnapshotContentResponse, StarOfficeDetectResponse,
    StartPreviewRequest, StopPreviewRequest,
};
pub use provider::{
    BedrockAuthMethod, BedrockConfig, CreateProviderRequest, DetectProtocolRequest, DetectionSuggestion,
    FetchModelsAnonymousRequest, FetchModelsRequest, FetchModelsResponse, HealthStatus, KeyTestResult, ModelCapability,
    ModelHealthStatus, ModelInfo, ModelType, MultiKeyResult, ProtocolDetectionResponse, ProviderResponse,
    SuggestionType, UpdateProviderRequest,
};
pub use remote_agent::{
    CreateRemoteAgentRequest, HandshakeResponse, RemoteAgentListItem, RemoteAgentResponse,
    TestRemoteAgentConnectionRequest, UpdateRemoteAgentRequest,
};
pub use response::{ApiResponse, ErrorResponse};
pub use shell::{
    CheckToolInstalledRequest, CheckToolInstalledResponse, DeepgramSpeechToTextConfig, OpenAISpeechToTextConfig,
    OpenExternalRequest, OpenFileRequest, OpenFolderWithRequest, ShowItemInFolderRequest, SpeechToTextConfig,
    SpeechToTextProvider, SpeechToTextResult, ToolType,
};
pub use skill::{
    AddExternalPathRequest, BuiltinAutoSkillResponse, DeleteSkillRequest, ExportSkillRequest,
    ExternalSkillSourceResponse, ImportSkillRequest, ImportSkillResponse, MaterializeSkillsRequest,
    MaterializeSkillsResponse, MaterializedSkillRef, NamedPathResponse, ReadAssistantRuleRequest,
    ReadBuiltinResourceRequest, ReadSkillInfoRequest, ReadSkillInfoResponse, RemoveExternalPathRequest,
    ScanForSkillsRequest, ScanForSkillsResponse, ScannedSkillResponse, SkillListItemResponse, SkillPathsResponse,
    SkillSourceResponse, WriteAssistantRuleRequest,
};
pub use system::{
    ClientPreferencesResponse, SystemSettingsResponse, UpdateClientPreferencesRequest, UpdateSettingsRequest,
};
pub use team::{
    AddAgentRequest, CreateTeamRequest, RenameAgentRequest, RenameTeamRequest, SendAgentMessageRequest,
    SendTeamMessageRequest, TeamAgentInput, TeamAgentRemovedPayload, TeamAgentRenamedPayload, TeamAgentResponse,
    TeamAgentShutdownPayload, TeamAgentSpawnedPayload, TeamAgentStatusPayload, TeamListResponse, TeamMcpPhase,
    TeamMcpStatusPayload, TeamResponse, TeammateMessagePayload,
};
pub use team_mcp::{GuideMcpConfig, TeamMcpStdioConfig};
pub use websocket::WebSocketMessage;
