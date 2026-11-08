import type { IRemoteProjectService } from "./remote-projects/contract";
import type { ITaskGroupService } from "./task-groups/contract";
import type { WindowProjectDirectoryServices } from "./window-project-directory/contract";
import type { IProjectService } from "./project/contract";
import type { IGenUiService } from "./gen-ui/contract";
import { IOffPeakTaskService } from "./session/offPeakTask";
import type { IHighspeedCardService } from "./highspeed/highspeedCard";
import type { IPluginUiBridgeService } from "./plugin-ui-bridge/contract";
import type { IPluginUiSamplingService } from "./plugin-ui-bridge/samplingContract";
import type { IPluginUiAppToolsService } from "./plugin-ui-bridge/appToolsContract";
import type { IFileService } from "./file/file";
import type { IMediaPreviewService } from "./media-preview/mediaPreview";
import type { ICloudContentService } from "./cloud-content/cloudContent";
import type { IMarketingTouchService } from "./marketing-touch/marketingTouch";
import type { IGitService } from "./git/git";
import type { IGitCheckpointService } from "./git/gitCheckpoint";
import type { ISystemService } from "./system/system";
import type { ITerminalService } from "./terminal/terminal";
import type { ISettingService } from "./setting/setting";
import type { ICredentialService } from "./credential/credential";
import type { IBroadcastService } from "./broadcast/broadcast";
import type { IZCodeTaskService } from "./session/zcodeTaskService";
import type { IZCodeAgentService } from "./zcode-agent/zcodeAgent";
import type { IZCodeSessionService } from "./zcode-session/zcodeSession";
import type { ICuaPermissionService } from "./cua-permission-broker/cuaPermissionService";
import type { IBotsService } from "./bots/bots";
import type { IFileWatcherService } from "./fileWatcher/fileWatcher";
import type { IOAuthService } from "./oauth/oauth";
import type {
  IModelSelectionService,
  IProviderSettingsService,
} from "./model-provider/providerFacadeServices";
import type { IUsageStatsService } from "./usage-stats/usageStats";
import type { ICodingPlanSubscriptionService } from "./coding-plan-subscription/codingPlanSubscription";
import type { IClientConfigService } from "./client-config/clientConfig";
import type { IClientScenesService } from "./client-scenes/clientScenes";
import type { ISkillsService } from "./skills/skills";
import type { ISkillSyncService } from "./skill-sync/skillSync";
import type { IDeviceSyncStateService, IUserConfigSyncService } from "./data-sync";
import type { IProviderProvisioningStatusService } from "./provider-provisioning-status";
import type { IMcpSyncService } from "./mcp-sync/mcpSync";
import type { IPluginSyncService } from "./plugin-sync/pluginSync";
import type { IPluginsService } from "./plugins/plugins";
import type { IPluginManagementService } from "./plugins/pluginManagement";
import type { ISubagentsService } from "./subagents/subagents";
import type { ICommandsService } from "./commands/commands";
import type { IHooksService } from "./hooks/hooks";
import type { IMemoryService } from "./memory/memory";
import type { IOutputStyleService } from "./output-style/outputStyle";
import type { ISettingsSyncService } from "./settings-sync/settingsSync";
import type { IFeedbackService } from "./feedback/feedback";
import type { IPromptAttachmentTransferService } from "./prompt-attachment-transfer/promptAttachmentTransfer";
import type { IWindowControllerService } from "./window-controller/windowController";
import type { ISourceServerWorkspaceService } from "./source-server-workspace/sourceServerWorkspace";
import type { ISourceServerManagementService } from "./source-server-management/sourceServerManagement";
import type { IOnboardingRecordService } from "./onboarding/onboardingRecord";
import type { IConversationShareService } from "./conversation-share/conversationShare";

/** UI 层消费的统一服务接口 */
export interface IServiceAccessor {
  readonly taskGroupService?: ITaskGroupService;
  readonly projectService?: IProjectService;
  readonly remoteProjectService?: IRemoteProjectService;
  readonly windowProjectDirectory?: WindowProjectDirectoryServices;
  readonly fileService: IFileService;
  readonly mediaPreviewService?: IMediaPreviewService;
  readonly cloudContentService?: ICloudContentService;
  readonly marketingTouchService?: IMarketingTouchService;
  readonly gitService: IGitService;
  readonly gitCheckpointService: IGitCheckpointService;
  readonly systemService: ISystemService;
  readonly terminalService: ITerminalService;
  readonly settingService: ISettingService;
  /** Onboarding 完成记录（本地持久化）；旧测试 double / 不支持的 host 可不提供。 */
  readonly onboardingRecordService?: IOnboardingRecordService;
  readonly credentialService: ICredentialService;
  readonly broadcastService: IBroadcastService;
  readonly zcodeTaskService: IZCodeTaskService;
  /** 窗口 Host 聚合面；旧 server wire 或测试 double 可暂不提供。 */
  readonly windowControllerService?: IWindowControllerService;
  readonly zcodeAgentService: IZCodeAgentService;
  readonly zcodeSessionService: IZCodeSessionService;
  // CUA 是 opt-in 内测特性：local macOS host 提供，远端 等 host 没有。可选避免连锁必填。
  readonly cuaPermissionService?: ICuaPermissionService;
  readonly conversationShareService: IConversationShareService;
  readonly botsService: IBotsService;
  readonly fileWatcherService: IFileWatcherService;
  readonly oauthService: IOAuthService;
  /** 当前 Environment 的 Provider 配置与设置视图。 */
  readonly providerSettingsService: IProviderSettingsService;
  /** 当前 Environment Registry 发布的唯一模型选择 View。 */
  readonly modelSelectionService: IModelSelectionService;
  /** Source Server 端 workspace 身份解析；本地 Host 也注册一份以保持合同一致。 */
  readonly sourceServerWorkspaceService: ISourceServerWorkspaceService;
  /** 仅桌面窗口 Host 的 base attachment 注册；远端 / Web 不提供。 */
  readonly sourceServerManagementService?: ISourceServerManagementService;
  /** 仅桌面本机 Host 注册：数据同步页的设备同步状态。 */
  readonly deviceSyncStateService?: IDeviceSyncStateService;
  /** 仅桌面本机 Host 注册：数据同步页模型提供商的 Provisioning lane 状态与重试。 */
  readonly providerProvisioningStatusService?: IProviderProvisioningStatusService;
  readonly usageStatsService: IUsageStatsService;
  readonly codingPlanSubscriptionService: ICodingPlanSubscriptionService;
  readonly clientConfigService: IClientConfigService;
  readonly clientScenesService: IClientScenesService;
  /** 闲时任务管理（独立服务面）。 */
  readonly offPeakTaskService: IOffPeakTaskService;
  readonly highspeedCardService: IHighspeedCardService;
  readonly skillsService: ISkillsService;
  readonly skillSyncService: ISkillSyncService;
  readonly mcpSyncService: IMcpSyncService;
  /** 插件 UI 桥；web 或旧 host 可不提供，UI 据此回退普通 MCP 卡片。 */
  readonly genUiService?: IGenUiService;
  readonly pluginUiBridgeService?: IPluginUiBridgeService;
  /** 页面 sampling；只有桌面本地任务开放。 */
  readonly pluginUiSamplingService?: IPluginUiSamplingService;
  /** App-Provided Tools；只有桌面本地 agent 时存在。 */
  readonly pluginUiAppToolsService?: IPluginUiAppToolsService;
  readonly pluginSyncService: IPluginSyncService;
  /** 数据同步：子智能体 / 命令 / 钩子共用的用户配置同步通道（按 category 区分）。 */
  readonly userConfigSyncService: IUserConfigSyncService;
  readonly pluginsService: IPluginsService;
  /** 设置页插件管理（UI 不再直触 zcodeAgentService 的 plugins/* 面） */
  readonly pluginManagementService: IPluginManagementService;
  readonly subagentsService: ISubagentsService;
  readonly commandsService: ICommandsService;
  readonly hooksService: IHooksService;
  readonly memoryService: IMemoryService;
  readonly outputStyleService: IOutputStyleService;
  readonly settingsSyncService: ISettingsSyncService;
  readonly feedbackService: IFeedbackService;
  readonly promptAttachmentTransferService: IPromptAttachmentTransferService;
}
