import type { ProjectWorkspaceNavigation } from "./contract";
export async function openProjectExample(navigation: ProjectWorkspaceNavigation) {
  return navigation.prepare({ hostId: "remote:server:example", projectId: "project" });
}
