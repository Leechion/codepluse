import type { WindowProjectDirectoryServices } from "./contract";

export async function windowProjectDirectoryExample(directory: WindowProjectDirectoryServices) {
  await directory.changesService.getCapabilities();
  return Promise.all([
    directory.projectService.readTree({ includeDeleted: true }),
    directory.remoteProjectService.readDirectory(),
  ]);
}
