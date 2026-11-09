import { describe, expect, it } from "vitest";
import { conversationSnapshotSchema } from "./shared/zcode-protocol-v4";

// 复刻 goConversationTransport.snapshot() 的产出形状（goal/plan 修复后）。
function adapterSnapshot(): Record<string, unknown> {
  const now = Date.now();
  const row = { rowId: 1, turnId: "turn-1-1", entityId: "turn-1-1:1", createdAt: now, createdAtSeq: 1,
    kind: "turnHeader", origin: "userInput", state: "running", startedAt: now };
  return {
    protocolVersion: 1, sessionId: "18", logEpoch: "18", seq: 1, revision: 1,
    control: { phase: "completedSuccess", sessionEnded: true, canStop: false, stopState: "idle",
      stopTargetKind: "unknown", activeWorks: [], lastError: null, apiRetry: null },
    availability: { fork: { allowed: true }, compact: { allowed: false, reasonCode: "x" },
      switchModelConfig: { allowed: true }, setFollowupMode: { allowed: false, reasonCode: "x" },
      queueEdit: { allowed: true }, sendQueuedNow: { allowed: true },
      pauseGoal: { allowed: false, reasonCode: "x" }, resumeGoal: { allowed: false, reasonCode: "x" } },
    inputRouting: { mode: "startNow" },
    meta: { title: "t", titleSource: "default" },
    config: { provider: "codepluse", model: "", thought: "standard", thoughtLevels: [], followupMode: "queue", mode: "build" },
    modelTransition: null,
    goal: null, plan: null,
    usage: { contextWindow: null, cumulative: { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0 } },
    queue: { items: [], autoDrain: true },
    pendingInteractions: [], pendingCommands: [], backgroundWorks: [],
    workspaceHookAdmission: null,
    rows: { window: [row], totalCount: 1, firstRowId: 1 },
  };
}

describe("adapter snapshot vs v4 schema", () => {
  it("parses after goal/plan fix", () => {
    const result = conversationSnapshotSchema.safeParse(adapterSnapshot());
    expect(result.success).toBe(true);
  });
  it("would fail without goal/plan (regression guard)", () => {
    const snap = adapterSnapshot() as Record<string, unknown>;
    delete snap.goal; delete snap.plan;
    expect(conversationSnapshotSchema.safeParse(snap).success).toBe(false);
  });
});
