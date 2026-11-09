// UI 侧 v4 数据层（M3 m3-ui-core，docs/v4-refactor/06-ui）。
// 纯新增：与旧 store/zcodeSessionStore* 并存，删除发生在竖切可用后（11-deletion-plan 波次 1）。
export {
  conversationTopic,
  type ConversationTransport,
} from "./transport";
export {
  ConversationProjectionStore,
  type ConversationStoreState,
  type ConversationStoreStatus,
  type OptimisticCommand,
} from "./conversationProjectionStore";
export {
  SessionDataLayer,
  type SessionDataLayerOptions,
  type SessionLease,
} from "./sessionDataLayer";
export {
  createAgentConversationTransport,
  type AgentConversationTransportTarget,
} from "./agentConversationTransport";
// ── sessions-index 读路径（列表活性/detail 数据源；持久行来自 tasks-index）──
export {
  applySessionsIndexFrame,
  SessionsIndexStore,
  EMPTY_SESSIONS_INDEX_STATE,
  type SessionsIndexState,
  type SessionsIndexStoreStatus,
} from "./sessionsIndexStore";
export {
  createAgentSessionsIndexTransport,
  type AgentSessionsIndexTransportTarget,
  type SessionsIndexTransport,
} from "./agentSessionsIndexTransport";
export {
  useSessionsIndex,
  type UseSessionsIndexResult,
} from "./useSessionsIndex";
export {
  LOCAL_SESSIONS_INDEX_ENDPOINT,
  acquireSessionsIndex,
  buildSessionsIndexEntryKey,
  releaseSessionsIndex,
  sessionsIndexRegistrySize,
  type SessionsIndexAgentService,
  type SessionsIndexScope,
} from "./sessionsIndexRegistry";
export {
  createCommandEnvelope,
  getV4ClientId,
  uuidv7,
} from "./commandFactory";
export {
  V4ConversationProvider,
  V4PaneConversationProvider,
  useV4Conversation,
  type V4ConversationContextValue,
  type V4PaneConversationProviderProps,
} from "./V4ConversationContext";
export { useConversationProjection } from "./useConversationProjection";
export { ConversationRowView } from "./ConversationRowView";
export { ConversationTimeline } from "./ConversationTimeline";
export { ConversationComposer } from "./ConversationComposer";
export { SessionPane, type SessionPaneProps } from "./SessionPane";
export { V4ChatPane, type V4ChatPaneProps } from "./V4ChatPane";
// ── 分屏二期：Layout/Focus 两层（二叉分割树）+ workspace 工作台宿主 ──
export {
  bindPaneSession,
  canAddPane,
  closePane,
  confirmRestoredPaneSession,
  countPanes,
  effectiveFocusedPaneId,
  focusPane,
  INITIAL_PANE_LAYOUT,
  leafPaneIds,
  MAX_WORKBENCH_PANES,
  openSessionInNewPane,
  paneWorkspaceKey,
  setSplitNodeRatio,
  splitPaneAt,
  usePaneLayoutStore,
  V4_LEGACY_SPLIT_PANE_ID,
  V4_PRIMARY_PANE_ID,
  type PaneBinding,
  type PaneLayoutNode,
  type PaneLayoutSnapshot,
  type PaneLayoutStore,
  type PaneWorkspaceScope,
  type SplitDirection,
} from "./paneLayoutStore";
export {
  acquireWorkspaceConnection,
  buildWorkspaceConnectionKey,
  LOCAL_WORKSPACE_CONNECTION_ENDPOINT,
  WORKSPACE_CONNECTION_KEEP_WARM_MS,
  workspaceConnectionRegistrySize,
  type WorkspaceConnectionAgentService,
  type WorkspaceConnectionLease,
  type WorkspaceConnectionScope,
} from "./workspaceConnectionRegistry";
export {
  V4WorkspaceChatArea,
  type V4WorkspaceChatAreaProps,
} from "./V4WorkspaceChatArea";
export { V4InteractionDialogs } from "./V4InteractionDialogs";
export {
  pendingPermissionToLegacyRequest,
  pendingUserInputToViewModel,
  type V4UserInputViewModel,
} from "./pendingInteractionAdapter";
