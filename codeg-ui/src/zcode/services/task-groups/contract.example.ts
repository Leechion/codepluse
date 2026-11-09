import type { ITaskGroupService } from "./contract";
export async function createGroupExample(service: ITaskGroupService) {
  return service.save({ title: "Research", color: "blue" });
}
