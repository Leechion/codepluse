import type { ZCodeSessionStateSnapshot } from "@zcode/shared";
import { createServiceLogger } from "../logger/serviceLogger";
import { repairImportedClaudeSessionSnapshot } from "../session/claude-native/importedClaudeHistoryRepair";
import type { IZCodeAgentService } from "../zcode-agent/zcodeAgent";
import type {
  ZCodeSessionReadParams,
  ZCodeSessionResumeParams,
} from "./zcodeSession";

const logger = createServiceLogger("zcode-session-service");

export async function repairEmptyImportedClaudeSessionSnapshot(params: {
  agentService: IZCodeAgentService;
  snapshot: ZCodeSessionStateSnapshot;
  target: ZCodeSessionResumeParams | ZCodeSessionReadParams;
}): Promise<ZCodeSessionStateSnapshot> {
  const repaired = await repairImportedClaudeSessionSnapshot({
    snapshot: params.snapshot,
    target: {
      workspacePath: params.target.workspacePath,
      workspaceIdentity: params.target.workspaceIdentity,
      taskId: params.target.sessionId,
      ...("mcpServers" in params.target && params.target.mcpServers
        ? { mcpServers: params.target.mcpServers }
        : {}),
    },
    createSession: (input) => params.agentService.createSession(input),
    onRepair: (history) => {
      logger.warn(
        undefined,
        `[zcode-session-service] Claude 导入 session 历史异常，按 ${history.source} 回填 taskId=${params.target.sessionId}`,
      );
    },
  });
  return repaired ?? params.snapshot;
}
