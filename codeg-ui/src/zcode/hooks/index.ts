/**
 * Hooks barrel export
 *
 * 所有服务和平台操作的 hooks 统一从此导出。
 * 组件应通过 hooks 访问服务，不再直接使用 services.* 或 window.zcode.*。
 */

// 服务上下文
export { ServiceProvider, useServices } from "./useServices";
export { useBaseWorkspaceServices, useWorkspaceServices } from "./useWorkspaceServices";

// 平台操作上下文
export {
  PlatformProvider,
  usePlatform,
  useSelectDirectory,
  useConnectRemote,
} from "./usePlatform";

// 文件服务
export { useReaddir } from "./useFileService";

// 文件监视服务
export { useWatchedReaddir } from "./useFileWatcherService";

// 系统服务
export { useSystemInfo, useIntranetProbe } from "./useSystemService";
export { useWorkspaceHomePath } from "./useWorkspaceHomePath";

// 终端服务
export { useTerminal } from "./useTerminalService";

// 设置服务
export { useSettings, useRecentProjects } from "./useSettingService";
export { useSkills } from "./useSkills";
export { usePlugins } from "./usePlugins";

// Onboarding 完成记录服务（本地持久化，后续上传服务器）
export { useOnboardingRecordService } from "./useOnboardingRecordService";

// 通用确认弹窗
export { useConfirmDialog } from "./useConfirmDialog";
export { useAlertDialog } from "./useAlertDialog";

// 凭据服务
export { useCredentials, useAuthToken } from "./useCredentials";
export { useZCodeAgentService } from "./useZCodeAgentService";

// Git pane
export { useGitAutoRefresh } from "./useGitAutoRefresh";
export { useGitRepository } from "./useGitRepository";
export { useGitActions } from "./useGitActions";
// workspace provider 配置路径
export { useWorkspaceProviderConfigFile } from "./useWorkspaceProviderConfigFile";
export { useTaskNativeSessionLogFile } from "./useTaskNativeSessionLogFile";
export { useTaskSessionFilePath } from "./useTaskSessionFilePath";
export { useUsageStats } from "./useUsageStats";
