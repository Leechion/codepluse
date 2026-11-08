import type { Event } from "@zcode/rpc";
import { ServiceChannels } from "@zcode/shared";
import { createServiceDescriptor } from "../descriptors";
import type { IProjectService } from "../project/contract";
import type { IRemoteProjectService } from "../remote-projects/contract";

export interface IWindowProjectChangesService {
  getCapabilities(): Promise<{ version: 1 }>;
  onDidChange: Event<void>;
}

/** 整组选择，避免把窗口项目与 Source 成员目录或广播混用。 */
export interface WindowProjectDirectoryServices {
  projectService: IProjectService;
  remoteProjectService: IRemoteProjectService;
  changesService: IWindowProjectChangesService;
}

export const IWindowProjectService = createServiceDescriptor<IProjectService>(
  ServiceChannels.WindowProject,
);
export const IWindowRemoteProjectService = createServiceDescriptor<IRemoteProjectService>(
  ServiceChannels.WindowRemoteProjects,
);
export const IWindowProjectChangesService = createServiceDescriptor<IWindowProjectChangesService>(
  ServiceChannels.WindowProjectChanges,
);
