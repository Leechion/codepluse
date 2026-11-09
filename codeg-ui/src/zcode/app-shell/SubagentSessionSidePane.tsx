import { memo, useMemo } from "react";
import type { MessageFileLinkTarget } from "../components/ai-elements/message";
import type { CodeViewerSource } from "../lib/codeViewer";
import type {
  OpenScopedSubagentSideTabRequest,
  OpenBackgroundBashSideTabRequest,
  SubagentSessionSidePaneTab,
} from "../lib/workspaceSidePane";
import type { PaneWorkspaceScope } from "../v4/paneLayoutStore";
import { SessionPane } from "../v4/SessionPane";
import { V4PaneConversationProvider } from "../v4/V4ConversationContext";

export const SubagentSessionSidePane = memo(function SubagentSessionSidePane({
  tab,
  focused,
  compactForRemoteControl = false,
  onOpenBrowserUrl,
  onOpenCodeViewer,
  onOpenFileLink,
  onOpenSubagentSession,
  onOpenBackgroundBash,
}: {
  tab: SubagentSessionSidePaneTab;
  focused: boolean;
  compactForRemoteControl?: boolean;
  onOpenBrowserUrl?: (url: string) => void;
  onOpenCodeViewer?: (source: CodeViewerSource) => void;
  onOpenFileLink?: (target: MessageFileLinkTarget) => void;
  onOpenBackgroundBash?: (request: OpenBackgroundBashSideTabRequest) => void;
  onOpenSubagentSession: (request: OpenScopedSubagentSideTabRequest) => void;
}) {
  const scope = useMemo<PaneWorkspaceScope>(
    () => ({
      workspacePath: tab.workspacePath,
      ...(tab.workspaceIdentity ? { workspaceIdentity: tab.workspaceIdentity } : {}),
      ...(tab.remoteSessionId ? { remoteSessionId: tab.remoteSessionId } : {}),
    }),
    [tab.remoteSessionId, tab.workspaceIdentity, tab.workspacePath],
  );

  return (
    <V4PaneConversationProvider scope={scope}>
      <SessionPane
        paneId={tab.id}
        sessionId={tab.childSessionId}
        openTrigger="subagent"
        rootSessionId={tab.rootSessionId}
        readOnly
        allowWorkspaceFileRewind
        compactForRemoteControl={compactForRemoteControl}
        focused={focused}
        telemetryVisible={focused}
        workspacePath={tab.workspacePath}
        workspaceIdentity={tab.workspaceIdentity}
        remoteSessionId={tab.remoteSessionId}
        onOpenBrowserUrl={onOpenBrowserUrl}
        onOpenCodeViewer={onOpenCodeViewer}
        onOpenFileLink={onOpenFileLink}
        onOpenSubagentSession={onOpenSubagentSession}
        onOpenBackgroundBash={onOpenBackgroundBash}
      />
    </V4PaneConversationProvider>
  );
});
