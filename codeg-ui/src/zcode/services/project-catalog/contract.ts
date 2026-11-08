import type { IProjectService, VirtualProject } from "../project/contract";
import type {
  IRemoteProjectService,
  RemoteProjectDirectory,
} from "../remote-projects/contract";
export type { VirtualProject, RemoteProjectDirectory };
export interface ProjectCatalogPort {
  projectService: Pick<IProjectService, "readProjects">;
  remoteProjectService?: Pick<IRemoteProjectService, "readDirectory">;
}
export interface ProjectCatalogSnapshot {
  projects: VirtualProject[];
  directory: RemoteProjectDirectory;
}
