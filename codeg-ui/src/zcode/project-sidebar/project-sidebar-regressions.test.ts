import { describe, expect, it } from "vitest";
import { countProjectTreeUnreadTasks, projectKey } from "./view";
import { buildProjectManagementRows } from "./project-management-view";
import {
  archiveTaskKey,
  projectKey as canonicalProjectKey,
  taskGroupKey,
} from "./identity-keys";
import { archiveTaskKey as archiveReExport } from "./archive-view";
import { taskGroupKey as groupReExport } from "./grouped-view";
import { getWorkspaceKey } from "../lib/workspaceKey";
import type {
  ProjectAvailability,
  ProjectTask,
  ProjectTree,
  RemoteProjectDirectory,
  VirtualProject,
} from "./contract";

const NOW = 1_800_000_000_000;
const DAY = 86_400_000;

function project(id: string, hostId: string): VirtualProject {
  return {
    id,
    hostId,
    name: id,
    nameOverridden: false,
    roots: [],
    position: 0,
    pinned: false,
    deleted: false,
    createdAt: NOW,
    updatedAt: NOW,
  };
}

function task(
  hostId: string,
  projectId: string | null,
  extra: Record<string, unknown> = {},
): ProjectTask {
  return {
    hostId,
    projectId,
    taskId: `${hostId}/${projectId}`,
    workspacePath: "E:/ws",
    pinned: false,
    archived: false,
    unreadAt: NOW,
    createdAt: NOW,
    updatedAt: NOW,
    ...extra,
  } as unknown as ProjectTask;
}

function tree(projects: VirtualProject[], tasks: ProjectTask[]): ProjectTree {
  return { projects, tasks };
}

describe("项目区：未读集合的复合身份", () => {
  it("hostId/id 含 :: 时不把未渲染项目的未读误计入角标", () => {
    // 旧实现用 `${hostId}::${id}` 拼接，(a, "b::c") 与 ("a::b", c) 会碰撞，
    // 于是未渲染项目 ("a::b", c) 的孤儿未读被当成项目 (a, "b::c") 的未读。
    const rendered = project("b::c", "a");
    const orphan = task("a::b", "c");
    const own = task("a", "b::c");

    expect(projectKey({ hostId: "a", id: "b::c" })).not.toBe(
      projectKey({ hostId: "a::b", id: "c" }),
    );
    expect(countProjectTreeUnreadTasks(tree([rendered], [orphan, own]))).toBe(1);
    expect(countProjectTreeUnreadTasks(tree([rendered], [orphan]))).toBe(0);
  });

  it("projectId 为 null 的本地孤儿任务仍计入，远程的不计入", () => {
    const rendered = project("p1", "local");
    expect(
      countProjectTreeUnreadTasks(tree([rendered], [task("local", null)])),
    ).toBe(1);
    expect(
      countProjectTreeUnreadTasks(tree([rendered], [task("remote-1", null)])),
    ).toBe(0);
  });
});

describe("项目区：项目管理闲置判定", () => {
  function rowsForUpdatedAt(updatedAt: number) {
    const p = project("p1", "local");
    const t = task("local", "p1", { unreadAt: undefined, updatedAt });
    const availability = new Map<string, ProjectAvailability>([
      [projectKey(p), { online: true, unresolved: false }],
    ]);
    return buildProjectManagementRows({
      tree: tree([p], [t]),
      availability,
      directory: { hosts: [] } as unknown as RemoteProjectDirectory,
      canRemoveEntries: false,
      now: NOW,
      hostLabel: () => "local",
    });
  }

  it("updatedAt=0 是占位时间戳，不判定为长期闲置", () => {
    const [row] = rowsForUpdatedAt(0);
    expect(row.sessions).toBe(1);
    expect(row.lastActiveAt).toBeNull();
    expect(row.reason).toBeUndefined();
    expect(row.idleMonths).toBeUndefined();
  });

  it("updatedAt 为 NaN 时同样不判定为闲置", () => {
    const [row] = rowsForUpdatedAt(Number.NaN);
    expect(row.lastActiveAt).toBeNull();
    expect(row.reason).toBeUndefined();
  });

  it("真实过期时间戳仍正常判定为闲置", () => {
    const [row] = rowsForUpdatedAt(NOW - 200 * DAY);
    expect(row.reason).toBe("idle");
    expect(row.idleMonths).toBe(6);
  });
});

describe("项目区：身份键的单一权威实现", () => {
  it("旧导出路径仍指向 identity-keys 的同一函数", () => {
    expect(projectKey).toBe(canonicalProjectKey);
    expect(archiveReExport).toBe(archiveTaskKey);
    expect(groupReExport).toBe(taskGroupKey);
  });

  it("projectKey 支持归档组使用的 null 项目 ID", () => {
    expect(canonicalProjectKey({ hostId: "local", id: null })).toBe('["local",null]');
  });

  it("三种键与被替换掉的内联写法逐字等价", () => {
    for (const identity of [undefined, "", "   ", "remote:host-1"]) {
      const t = task("local", "p1", { workspaceIdentity: identity });
      const inline = t.workspaceIdentity?.trim() || t.workspacePath;
      expect(getWorkspaceKey(t.workspacePath, t.workspaceIdentity)).toBe(inline);
      expect(archiveTaskKey(t)).toBe(JSON.stringify([t.hostId, inline, t.taskId]));
      expect(taskGroupKey(t)).toBe(JSON.stringify([inline, t.taskId]));
    }
  });
});
