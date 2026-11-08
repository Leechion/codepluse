import type { GroupTaskRef, ProjectTask } from "./contract";
import { getWorkspaceKey } from "../lib/workspaceKey";

/**
 * 项目区复合身份键的唯一权威实现。
 * CONTRACT.md:17 要求跨 Host 的排序、选择和去重使用 Host + 项目 ID 复合身份；
 * 禁止用可碰撞的字符串拼接（分隔符本身可能出现在 hostId / id 里）。
 * id 允许 null：归档组用 [hostId, null] 表示未归类任务。
 */
export function projectKey(project: { hostId: string; id: string | null }): string {
  return JSON.stringify([project.hostId, project.id]);
}

/** 任务在 Host 树中的完整身份：Host + workspace 身份 + taskId。 */
export const archiveTaskKey = (task: ProjectTask): string =>
  JSON.stringify([
    task.hostId,
    getWorkspaceKey(task.workspacePath, task.workspaceIdentity),
    task.taskId,
  ]);

/** 分组视图中的成员身份：workspace 身份 + taskId（按分组契约不含 Host）。 */
export const taskGroupKey = (
  task: Pick<GroupTaskRef, "workspacePath" | "workspaceIdentity" | "taskId">,
): string =>
  JSON.stringify([
    getWorkspaceKey(task.workspacePath, task.workspaceIdentity),
    task.taskId,
  ]);
