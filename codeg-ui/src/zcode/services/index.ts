// Descriptors & collection (browser-safe)
export { type ServiceDescriptor, createServiceDescriptor } from "./descriptors";
export { ServiceCollection, type ServiceExposurePolicy } from "./collection";
export {
  IModelSelectionService,
  IProviderSettingsService,
  type ModelSelectionView,
  type ModelSelectionViewInput,
  type ProviderSettingsProviderView,
  type ProviderSettingsView,
} from "./model-provider/providerFacadeServices";
export {
  createAccountRequestAuthService,
  type IAccountRequestAuthService,
  type AccountRequestAuthInput,
  type AccountRequestAuthMaterial,
  type AccountRequestAuthResolver,
} from "./model-provider/accountRequestAuthService";
export { IProviderProvisioningTargetService } from "./model-provider/providerProvisioning";
export {
  collectServiceMemoryDiagnostics,
  memoryDiagnosticsRegistry,
  registerMemoryDiagnosticsProvider,
} from "./memoryDiagnostics";

// Accessor
export type { IServiceAccessor } from "./accessor";
export {
  ConversationShareServiceError,
  createUnsupportedConversationShareService,
  IConversationShareService,
} from "./conversation-share/conversationShare";
export type {
  ConversationShareSelection,
  ConversationSharePublishProgress,
  ConversationShareImportProgress,
  ImportConversationShareInput,
  ImportConversationShareResult,
  ImportedConversationShare,
  ConversationShareServiceErrorKind,
  ConversationShareFailureIssue,
  ConversationShareFailureIssueCode,
  ConversationSharePreflightInput,
  ConversationSharePreflightResult,
  ConversationShareAllowedArtifact,
  ConversationShareTurnPreflightResult,
  PublishTextConversationInput,
} from "./conversation-share/conversationShare";
// Conversation share 的具体实现依赖 Node 文件系统，只能从 @zcode/services/node 引入；
// 根入口必须保持 browser-safe，避免 renderer 解析到 node:* 模块。
export {
  createConversationTelemetryService,
  type ConversationTelemetryWorkspaceTarget,
  type IConversationTelemetryService,
} from "./conversation-telemetry/conversationTelemetry";

// File service — IFileService is both a type (interface) and value (descriptor)
export { IFileService } from "./file/file";
export { IMediaPreviewService } from "./media-preview/mediaPreview";
export { ICloudContentService } from "./cloud-content/cloudContent";
export { IMarketingTouchService } from "./marketing-touch/marketingTouch";
export type { MediaPreviewPreparation } from "./media-preview/mediaPreview";

// Git service — IGitService is both a type (interface) and value (descriptor)
export { IGitService } from "./git/git";
export { IGitCheckpointService } from "./git/gitCheckpoint";

// System service — ISystemService is both a type (interface) and value (descriptor)
export { ISystemService } from "./system/system";

// Terminal service — ITerminalService is both a type (interface) and value (descriptor)
export { ITerminalService } from "./terminal/terminal";

// Setting service — ISettingService is both a type (interface) and value (descriptor)
export { ISettingService } from "./setting/setting";

// Credential service — ICredentialService is both a type (interface) and value (descriptor)
export { ICredentialService } from "./credential/credential";
export { ISourceServerWorkspaceService } from "./source-server-workspace/sourceServerWorkspace";
export {
  ISourceServerManagementService,
  type UpdateConnectionProfileInput,
} from "./source-server-management/sourceServerManagement";

// Broadcast service — IBroadcastService is both a type (interface) and value (descriptor)
export { IBroadcastService } from "./broadcast/broadcast";

// Onboarding 完成记录服务（本地持久化，后续上传服务器）
export { IOnboardingRecordService } from "./onboarding/onboardingRecord";
export type {
  CreateOnboardingRecordServiceOptions,
  OnboardingRecordServiceFactory,
} from "./onboarding/onboardingRecord";
// 这里只能导出 descriptor 和类型。根 index 会被 renderer 经 value import 拉进浏览器包，
// 若 value 导出 createOnboardingRecordService，会连带 fs/atomicFileUtils → @zcode/shared/node →
// node:timers/promises 整条 Node 链进浏览器，模块加载直接抛错导致整个应用黑屏。
// 工厂函数由 host 侧（node.ts）与测试从实现文件路径直接导入，与 createSettingService 同惯例。
export type {
  BroadcastClaimAcquireResult,
  BroadcastClaimLease,
  BroadcastMessage,
} from "./broadcast/broadcast";

// ZCode task wrapper service — task 列表/置顶/归档等 app 侧包装状态入口。
export { IZCodeTaskService } from "./session/zcodeTaskService";
export type {
  ZCodeArchivedTaskDeletionResult,
  ZCodeModelTrajectory,
  ZCodeModelTrajectoryCallSource,
  ZCodeModelTrajectoryCallSourceKind,
  ZCodeModelTrajectoryContentPart,
  ZCodeModelTrajectoryMessage,
  ZCodeModelTrajectoryRecord,
  ZCodeModelTrajectoryUsage,
  ZCodeTaskListKind,
  ZCodeTaskListQuery,
  ZCodeTaskListResult,
  ZCodeTaskListSortBy,
  ZCodeTaskListWorkspaceScope,
  ZCodeTaskReadyOutcome,
} from "./session/zcodeTaskService";
export type { ZCodeTaskListItem } from "./session/zcodeTaskListTypes";

export { IWindowControllerService } from "./window-controller/windowController";
export {
  IWindowProjectService,
  IWindowRemoteProjectService,
  IWindowProjectChangesService,
  type WindowProjectDirectoryServices,
} from "./window-project-directory/contract";
export {
  createWebRemoteProjectTreeActivityBridge,
  createWindowProjectChangesService,
  type WebRemoteProjectTask,
} from "./window-project-directory";
export type {
  WindowHostControllerFrame,
  WindowHostControllerMutation,
  WindowHostControllerTaskListItem,
  WindowHostControllerTaskListResult,
  WindowHostTaskUnreadSignal,
} from "./window-controller/windowController";

// ZCode agent service — IZCodeAgentService is both a type (interface) and value (descriptor)
export {
  IZCodeAgentService,
  type ZCodeAgentLocalRuntimeChildProcesses,
  ZCODE_AGENT_RUNTIME_UNAVAILABLE_CODE,
} from "./zcode-agent/zcodeAgent";
export {
  isZCodeAgentMcpStatusModeUnsupportedError,
  ZCODE_AGENT_MCP_STATUS_MODE_UNSUPPORTED_ERROR_CODE,
  ZCodeAgentMcpStatusModeUnsupportedError,
} from "./zcode-agent/zcodeAgentErrors";
export {
  createZCodeAgentConnectionScope,
  readTrustedZCodeAgentV4Connection,
} from "./zcode-agent/zcodeAgentConnectionScope";
export type {
  ZCodeAgentConnectionScope,
  ZCodeAgentV4ClientMode,
  ZCodeAgentV4ConnectionContext,
} from "./zcode-agent/zcodeAgentConnectionScope";
export type {
  ZCodeAgentAttachmentBeginParams,
  ZCodeAgentAttachmentChunkParams,
  ZCodeAgentAttachmentTerminalParams,
  ZCodeAgentCreateSessionParams,
  ZCodeAgentCuaPermissionObservation,
  ZCodeAgentInitializeResult,
  ZCodeAgentStorageStartupSnapshot,
  ZCodeAgentRuntimeLifecycleEvent,
  ZCodeAgentRuntimePolicy,
  ZCodeAgentReadSessionParams,
  ZCodeAgentRespondProviderRuntimeHeadersParams,
  ZCodeAgentProviderRuntimeHeadersRequest,
  ZCodeAgentResumeSessionParams,
  ZCodeAgentRunAutomationNowResult,
  ZCodeAgentSavedWorkflowTarget,
  ZCodeAgentSendPromptParams,
  ZCodeAgentServiceEvent,
  ZCodeAgentSessionSubscribeParams,
  ZCodeAgentSessionTarget,
  ZCodeAgentSetModeParams,
  ZCodeAgentSetModelParams,
  ZCodeAgentSetThoughtLevelParams,
  ZCodeAgentWorkspaceTarget,
} from "./zcode-agent/zcodeAgent";

// ZCode session service — app-facing session facade without ZCode Agent naming.
export { IZCodeSessionService } from "./zcode-session/zcodeSession";
export type {
  ZCodeSessionCreateParams,
  ZCodeSessionEventsParams,
  ZCodeSessionInitializeResult,
  ZCodeSessionListParams,
  ZCodeSessionMessagesParams,
  ZCodeSessionReadParams,
  ZCodeSessionResumeParams,
  ZCodeSessionServiceEvent,
  ZCodeSessionSetModeParams,
  ZCodeSessionSetModelParams,
  ZCodeSessionSetThoughtLevelParams,
  ZCodeSessionSubscribeParams,
  ZCodeTaskTarget,
  ZCodeSessionWorkspaceTarget,
} from "./zcode-session/zcodeSession";

// Bots service — IBotsService is both a type (interface) and value (descriptor).
export { IBotsService } from "./bots/bots";
export type {
  BotRemoteWorkspaceResolver,
  BotRemoteWorkspaceResolution,
  BotBindCodeResult,
  BotCreateBindCodeParams,
  BotListWorkspaceRefsParams,
  BotSaveBotParams,
  BotTestResult,
} from "./bots/bots";

// Hooks service — IHooksService is both a type (interface) and value (descriptor).
export { IHooksService } from "./hooks/hooks";

// Memory service — IMemoryService is both a type (interface) and value (descriptor).
export {
  IMemoryService,
  PROJECT_MEMORY_FILE_CHANGED_ERROR_CODE,
  PROJECT_MEMORY_PREVIEW_LIMIT_EXCEEDED_ERROR_CODE,
} from "./memory/memory";
export type { ProjectMemoryFileSummary, ProjectMemoryWorkspaceSummary } from "./memory/memory";

// OutputStyle service — IOutputStyleService is both a type (interface) and value (descriptor).
export { IOutputStyleService } from "./output-style/outputStyle";
export type { SessionRealtimePort } from "./session/sessionRealtimePort";

// FileWatcher service — IFileWatcherService is both a type (interface) and value (descriptor)
export { IFileWatcherService } from "./fileWatcher/fileWatcher";

// OAuth service — IOAuthService is both a type (interface) and value (descriptor)
export { IOAuthService } from "./oauth/oauth";

// UsageStats service — IUsageStatsService is both a type (interface) and value (descriptor)
export { IUsageStatsService } from "./usage-stats/usageStats";

// Storage（资源管理器「存储」tab）：数据类型在 @zcode/shared；这里只导出服务接口与卷分组纯函数
// StorageRootSpec 随契约再导出：desktop 的 Worker 协议按 contract.ts 的口径只从 services 入口取类型。
export type { IStorageService, StorageRootSpec } from "./storage/contract";

// CodingPlanSubscription service — ICodingPlanSubscriptionService is both a type (interface) and value (descriptor)
export {
  ICodingPlanSubscriptionService,
  type OffPeakClientConfig,
} from "./coding-plan-subscription/codingPlanSubscription";
export {
  IClientScenesService,
  type ClientSceneConfig,
  type ClientSceneItem,
  type ClientSceneOption,
  type ClientSceneResponseBody,
  type ClientScenesResponse,
} from "./client-scenes/clientScenes";
export { isValidCronExpr } from "./session/automationCronValidation";
// 闲时任务管理服务（与 automation 服务面独立）；接口/描述符 browser-safe。
export { IOffPeakTaskService } from "./session/offPeakTask";
export type { OffPeakUpdateTaskParams } from "./session/offPeakTask";

// Skills service — ISkillsService is both a type (interface) and value (descriptor)
export { ISkillsService } from "./skills/skills";
export { ISkillSyncService } from "./skill-sync/skillSync";
export { IDeviceSyncStateService, IUserConfigSyncService } from "./data-sync";
export type { IDataSyncCategoryService } from "./data-sync";
export { IProviderProvisioningStatusService } from "./provider-provisioning-status";
export { IMcpSyncService } from "./mcp-sync/mcpSync";
export { IPluginSyncService } from "./plugin-sync/pluginSync";
export {
  ICuaPermissionService,
  type CuaPermissionState,
  type CuaPermissionRestartOptions,
  type CuaPermissionStatus,
  type CuaPermissionStatusQueryOptions,
  type CuaPermissionStatusResult,
  type CuaPermissionStatusUnavailable,
  isCuaPermissionStatusAvailable,
} from "./cua-permission-broker/cuaPermissionService";
export {
  ICuaPipSessionService,
  type CuaPipSessionService,
} from "./cua-permission-broker/cuaPipSession";

// Plugins service — IPluginsService is both a type (interface) and value (descriptor)
export { IPluginsService } from "./plugins/plugins";
// 设置页插件管理薄服务（UI 平台能力面不再直触 zcodeAgentService）
export { IPluginManagementService, withPluginTelemetry } from "./plugins/pluginManagement";

// Subagents service — ISubagentsService is both a type (interface) and value (descriptor)
export { ISubagentsService } from "./subagents/subagents";

// Commands service — ICommandsService is both a type (interface) and value (descriptor)
export { ICommandsService } from "./commands/commands";

export { ISettingsSyncService } from "./settings-sync/settingsSync";

export { IFeedbackService } from "./feedback/feedback";
export type { FeedbackUploadProgress } from "./feedback/feedback";
export { IPromptAttachmentTransferService } from "./prompt-attachment-transfer/promptAttachmentTransfer";
export type {
  PromptAttachmentStageParams,
  PromptAttachmentStageResult,
  PromptAttachmentTransferPhase,
  PromptAttachmentTransferProgress,
} from "./prompt-attachment-transfer/promptAttachmentTransfer";
export type {
  CreateFeedbackTicketInput,
  FeedbackAttachment,
  FeedbackAttachmentKind,
  FeedbackComment,
  FeedbackDeviceInfo,
  FeedbackListQuery,
  FeedbackListResult,
  FeedbackReporter,
  FeedbackTicketDetail,
  FeedbackTicketFramework,
  FeedbackTicketModule,
  FeedbackTicketSeverity,
  FeedbackTicketStatus,
  FeedbackTicketSummary,
  FeedbackTicketType,
} from "@zcode/shared";

export { IProjectService, LEGACY_SINGLE_ROOT_PROJECT_PREFIX } from "./project/contract";
export type {
  ProjectTree,
  ProjectTask,
  ProjectTaskRef,
  VirtualProject,
  ProjectRoot,
  SaveProjectInput,
  ProjectDirectories,
} from "./project/contract";

export { IRemoteProjectService } from "./remote-projects/contract";
export type {
  RemoteProjectHost,
  RemoteProjectEntry,
  RemoteProjectCache,
  RemoteProjectDirectory,
} from "./remote-projects/contract";

export { readProjectCatalog, projectSelectionId } from "./project-catalog";
export type { ProjectCatalogPort, ProjectCatalogSnapshot } from "./project-catalog/contract";

// 插件 UI 桥：描述符、参数类型与工厂；App-Provided Tools 另有独立描述符。
export {
  createPluginUiBridgeService,
  createPluginUiAppToolsService,
  createPluginUiSamplingService,
  IPluginUiSamplingService,
  type PluginUiSamplingParams,
  type PluginUiCancelSamplingParams,
  PluginUiBridgeError,
  IPluginUiAppToolsService,
  type PluginUiAppToolInstanceParams,
  type PluginUiAppToolCallParams,
  type PluginUiAppToolAcceptedResult,
  type PluginUiRegisterAppToolsParams,
  type PluginUiRegisterAppToolsResult,
  type PluginUiResolveAppToolCallParams,
} from "./plugin-ui-bridge";
export {
  IPluginUiBridgeService,
  type PluginUiCallToolParams,
  type PluginUiCallToolResult,
  type PluginUiPrepareSandboxParams,
  type PluginUiReadResourceParams,
  type PluginUiReadResourceResult,
  type PluginUiPluginScope,
  type PluginUiListResourcesParams,
  type PluginUiListResourcesResult,
  type PluginUiListResourceTemplatesResult,
  type PluginUiResourceSubscriptionParams,
  type PluginUiSurfaceEntry,
  type PluginUiWorkspaceTarget,
} from "./plugin-ui-bridge/contract";
export { IClientConfigService } from "./client-config/clientConfig";

export { IHighspeedCardService } from "./highspeed/highspeedCard";
export {
  HighspeedCardService,
  createHighspeedHttpTransport,
  buildHighspeedTurnExecution,
} from "./highspeed/highspeedCardService";
export { createHighspeedMockTransport } from "./highspeed/highspeedMockTransport";

export { IGenUiService } from "./gen-ui/contract";
export type { GenUiPrepareParams } from "./gen-ui/contract";

export { ITaskGroupService } from "./task-groups/contract";
export type {
  TaskGroup,
  TaskGroupColor,
  TaskGroupStructure,
  GroupTaskRef,
  GroupNode,
  MoveGroupNode,
} from "./task-groups/contract";
