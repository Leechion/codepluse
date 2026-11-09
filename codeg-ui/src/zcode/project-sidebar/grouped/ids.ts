import type { ProjectTask as ZCodeTaskMeta } from "../contract";

import { buildTaskWorkspaceKey } from "../../lib/taskQueryCache";

function taskKey(
  task: Pick<ZCodeTaskMeta, "workspacePath" | "workspaceIdentity" | "taskId">,
): string {
  return `${buildTaskWorkspaceKey(task.workspacePath, task.workspaceIdentity)}\u0000${task.taskId}`;
}

export { taskKey };
