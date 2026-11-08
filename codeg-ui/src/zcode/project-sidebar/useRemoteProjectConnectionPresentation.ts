import { toast } from "../components/ui/toast";
import { useZCodeIntl } from "../i18n/IntlProvider";
import { formatRemoteConnectionError } from "../lib/remoteConnectionPresentation";
import { getRemoteConnectionFailureCode } from "../lib/remoteWorkspaceServiceError";
import { resolveWorkspaceKey } from "@zcode/shared";
import { useTabStore } from "../store/TabStoreProvider";
import { isWorkspaceTab } from "../store/tabStore";
import { useRemoteWorkspaceSessionStore } from "../store/remoteWorkspaceSessionStore";
import {
  useRemoteProjectPresentationFacts,
  useRemoteProjectReconnect,
} from "../hooks/RemoteProjectPresentationContext";
import { useRemoteDevicePresentationInput } from "../hooks/useSourceServerStatuses";
import { useOptionalRemoteWorkspaceController } from "../remote/useRemoteWorkspaceController";
import { resolveRemoteConnectionPresentation } from "../lib/remoteConnectionPresentation";
import { getWorkspaceSourceServerId } from "../conversation-workspace-target/sourceIdentity";

export function useRemoteProjectConnectionPresentation(scope: {
  workspacePath: string;
  workspaceIdentity?: string;
}) {
  const { intl } = useZCodeIntl();
  const workspaceKey = resolveWorkspaceKey(scope);
  const tab = useTabStore((state) =>
    state.tabs.find(
      (entry) => isWorkspaceTab(entry) && resolveWorkspaceKey(entry) === workspaceKey,
    ),
  );
  const workspaceTab = tab && isWorkspaceTab(tab) ? tab : null;
  const sourceId = getWorkspaceSourceServerId(workspaceTab ?? scope);
  const device = useRemoteDevicePresentationInput(sourceId ?? undefined);
  const facts = useRemoteProjectPresentationFacts(workspaceKey);
  const attached = useRemoteWorkspaceSessionStore((state) => {
    const id = state.sessionIdByWorkspaceIdentity[workspaceKey] ?? workspaceTab?.remoteSessionId;
    return Boolean(id && state.sessionsById[id]?.services);
  });
  const controller = useOptionalRemoteWorkspaceController();
  const onReconnect = useRemoteProjectReconnect();
  const presentation = resolveRemoteConnectionPresentation({ ...device, ...facts, attached });
  return {
    tab: workspaceTab,
    deviceName: device?.displayName ?? workspaceTab?.label ?? scope.workspacePath,
    presentation,
    busy:
      facts.reconnecting ||
      ["bootstrapping", "retrying", "connecting", "updating"].includes(device?.state ?? ""),
    error: facts.projectError ?? device?.error,
    reconnect: async () => {
      try {
        if (onReconnect) await onReconnect(workspaceKey);
        else await controller?.dispatch({ type: "reconnect-workspace", workspaceKey });
      } catch (error) {
        toast(
          formatRemoteConnectionError({
            fallback: String(error),
            reasonCode: getRemoteConnectionFailureCode(error),
            formatMessage: intl.formatMessage,
          }),
        );
      }
    },
  };
}
