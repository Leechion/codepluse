import { useRemoteProjectPresentationFacts } from "../hooks/RemoteProjectPresentationContext";
import { parseRemoteWorkspaceIdentity } from "@zcode/shared";
import { cn } from "../components/lib/utils";
import { useRemoteDevicePresentationInput } from "../hooks/useSourceServerStatuses";
import { useZCodeIntl } from "../i18n/IntlProvider";
import { resolveRemoteConnectionPresentation } from "../lib/remoteConnectionPresentation";
import {
  resolveTaskSourceLabel,
  type TaskSourceAttributionInput,
} from "../lib/taskSourceAttribution";

export function RemoteTaskDeviceLabel({
  task,
  className,
}: {
  task: TaskSourceAttributionInput;
  className?: string;
}) {
  const { intl } = useZCodeIntl();
  const sourceServerId =
    task.sourceServerId ??
    parseRemoteWorkspaceIdentity(task.workspaceIdentity ?? "")?.sourceServerId;
  const input = useRemoteDevicePresentationInput(sourceServerId);
  const projectFacts = useRemoteProjectPresentationFacts(
    task.workspaceIdentity?.trim() || task.workspacePath,
  );
  const presentation = resolveRemoteConnectionPresentation({
    ...input,
    ...projectFacts,
  });
  const name = resolveTaskSourceLabel(task);
  if (!name) return null;
  return (
    <span
      data-task-source-label="true"
      className={cn(
        "inline-flex h-5 min-w-0 max-w-28 shrink-0 items-center gap-1 rounded-md bg-surface px-2 text-ui-xs font-normal text-remote-foreground-muted",
        className,
      )}
    >
      <span className="truncate">{name}</span>
      {input && presentation.status !== "online" ? (
        <span className={cn("truncate", presentation.foreground)}>
          {intl.formatMessage({ id: presentation.shortMessageId })}
        </span>
      ) : null}
    </span>
  );
}
