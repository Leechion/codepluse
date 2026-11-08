import type { IZCodeSessionService } from "@zcode/services";
import { useServices } from "./useServices";
import { useWorkspaceServices } from "./useWorkspaceServices";

export function useZCodeSessionService(
  workspacePath?: string,
  preferredRemoteSessionId?: string | null,
  workspaceIdentity?: string | null,
): IZCodeSessionService {
  const services = workspacePath
    ? useWorkspaceServices(workspacePath, preferredRemoteSessionId, workspaceIdentity)
    : useServices();
  return services.zcodeSessionService;
}
