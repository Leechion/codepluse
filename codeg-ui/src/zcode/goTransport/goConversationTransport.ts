// GoConversationTransport: ConversationTransport over the codepluse Go backend.
// Synthesizes the v4 conversation protocol (snapshot/rows frames, CAS command
// acks) from the Go WS contract (acp://event, conversation turns, acp_prompt).
// The UI stack (SessionDataLayer/ProjectionStore/components) is unchanged —
// this file is the only seam.
//
// 关键协议对齐（缺一不可，恢复时务必保留）：
// 1. subscribe/resync 返回 { ack: { subscriptionId, mode, logEpoch } }（store 读
//    result.ack.subscriptionId，平铺形状会让投影永远停在 connecting）。
// 2. 所有帧经 transport.onFrame 单监听扇出（SessionDataLayer 按 frame.topic 路由
//    到各 store）；per-session listeners 不会被任何人注册，帧会全部丢失。
// 3. delta 项字段名是 op（row.appended/row.upserted/row.delta/state.updated），
//    不是 kind；kind 只用于帧层 payload 判别（"snapshot"/"deltas"）。
// 4. initial snapshot 必须在 activate(subscriptionId) 时才发：store 的代际防护
//    要求帧 subscriptionId 与已提交 ACK 一致，订阅期间提前发帧会被静默丢弃。
import type {
  CommandAck,
  CommandEnvelope,
  ConversationTopicFrame,
  ConversationResyncParams,
  SubscribeParams,
  TopicFrameDeliveryKind,
  V4ConversationResyncResult,
  V4ConversationRowsRangeParams,
  V4ConversationRowsRangeResult,
  V4ConversationSubscribeResult,
  CommandsQueryParams,
  CommandsQueryResult,
} from "@/zcode/shared/zcode-protocol-v4";
import type {
  AttachmentRef,
  QueueItem,
} from "@/zcode/shared/zcode-protocol-v4";
import type { ConversationTransport } from "@/zcode/v4/transport.js";
import type { AttachmentUploadOptions } from "@/zcode/v4/attachmentUploadTransaction";
import type { V4AttachmentPutParams, V4AttachmentPutResult } from "@/zcode/shared/zcode-protocol-v4";
import { conversationTopic } from "@/zcode/shared/zcode-protocol-v4";
import {
  acpCancel,
  acpConnect,
  acpFindConnectionForConversation,
  acpFork,
  acpPrompt,
  acpRespondPermission,
  acpSetConfigOption,
  deleteConversation,
  getFolderConversation,
  readWorkspaceFileBase64,
  updateConversationTitle,
  uploadAttachment,
} from "@/lib/api";
import { buildFileUri } from "@/lib/reference-link";
import { TurnBusyError } from "@/lib/turn-busy";
import type { AgentType } from "@/lib/types";
import { uuidv7 } from "../shared/uuid";

type FrameListener = (
  frame: ConversationTopicFrame,
  context?: { deliveryKind: TopicFrameDeliveryKind },
) => void;
type RestartListener = (reason?: "runtimeRestart" | "transportReplaced") => void;
type RekeyListener = (draftSessionId: string, realSessionId: string) => void;

interface LiveTurnState {
  turnId: string;
  headerRowId: number;
  userRowId: number;
  assistantRowId: number | null;
  assistantText: string;
  reasoningRowId: number | null;
  reasoningText: string;
  toolRows: Map<number, RowLite>;
  running: boolean;
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type RowLite = any;

type SessionPhase =
  | "draft"
  | "running"
  | "completedSuccess"
  | "completedInterrupted"
  | "error";

function nowIso(): string {
  return new Date().toISOString();
}

function base64ToBytes(dataBase64: string): Uint8Array {
  const binary = atob(dataBase64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

function base64ByteLength(dataBase64: string): number {
  if (!dataBase64) return 0;
  const padding = dataBase64.endsWith("==") ? 2 : dataBase64.endsWith("=") ? 1 : 0;
  return Math.floor((dataBase64.length * 3) / 4) - padding;
}

/** file://ref（或裸绝对路径）→ OS 绝对路径（Windows 盘符去前导斜杠）。 */
function fileRefToOsPath(ref: string): string {
  let value = ref.startsWith("file://") ? ref.slice("file://".length) : ref;
  try {
    value = decodeURIComponent(value);
  } catch {
    // 保留原样（buildFileUri 只对普通路径段编码，绝大多数情况可直解）。
  }
  if (/^\/[A-Za-z]:\//.test(value)) value = value.slice(1);
  return value;
}

function guessMimeFromName(name: string): string {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  const table: Record<string, string> = {
    png: "image/png",
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    gif: "image/gif",
    webp: "image/webp",
    svg: "image/svg+xml",
    pdf: "application/pdf",
    txt: "text/plain",
    md: "text/plain",
    mp4: "video/mp4",
    webm: "video/webm",
  };
  return table[ext] ?? "application/octet-stream";
}

/** AttachmentRef（file:// ref 或本地路径）→ Go acp_prompt 块。 */
function attachmentRefToPromptBlock(ref: AttachmentRef): RowLite {
  const uri = ref.ref.startsWith("file://") ? ref.ref : buildFileUri(ref.ref);
  if (ref.mime?.startsWith("image/")) {
    // data 留空 + file:// uri：Go 端 imageData 会从 uploads 目录回读字节
    // （attachments.go），与旧 composer 的 stripUploadedImagePayloads 契约一致。
    return { type: "image", data: "", mime_type: ref.mime, uri };
  }
  // Go prompt 循环原生支持 resource_link（connections.go resourceLink），
  // 非图片文件走资源链接，不折损为纯文本。
  return { type: "resource_link", uri, name: ref.fileName, mime_type: ref.mime };
}

function attachmentRefsToPromptBlocks(refs: readonly AttachmentRef[] | undefined): RowLite[] {
  if (!refs || refs.length === 0) return [];
  return refs.map(attachmentRefToPromptBlock);
}

/**
 * Adapter state per conversation id. Rows are synthesized:
 *   turnHeader → userInput → (reasoning)? → assistantText(streaming) → toolCall*
 * from Go events; persisted turns hydrate history on subscribe.
 * draft 会话（conversationId=0）在首条 sendText 时经 acp_prompt(conversationId=null)
 * 由 Go 创建真实会话，conversation_linked 事件到达后重 key。
 */
class GoConversationSession {
  conversationId: number;
  sessionId: string;
  subscriptionId: string;
  logEpoch: string;
  seq = 0;
  revision = 0;
  rowSeq = 0;
  rows: RowLite[] = [];
  turns = new Map<number, LiveTurnState>();
  pendingInteractions: RowLite[] = [];
  phase: SessionPhase = "completedSuccess";
  lastError: RowLite | null = null;
  title = "";
  /** draft 会话标记：首条 prompt 后 conversation_linked 重 key。 */
  draft = false;
  /** draft 会话发起连接用的执行端 / 工作目录（页面通过 transport 默认值注入）。 */
  agentType: AgentType | null = null;
  workingDir: string | null = null;
  /** 水化代际：并发 hydrate 只有最新一次生效（防 rekey+subscribe 竞态重复追加）。 */
  hydrateEpoch = 0;
  // ── queue（inputRouting 三态）──
  queue: QueueItem[] = [];
  autoDrain = true;
  pauseReason: "stopped" | "manual" | "error" | undefined;
  admissionSeq = 0;
  /** transport 级帧扇出（SessionDataLayer 按帧 topic 分发给各 store）。 */
  private emitFrame: (frame: ConversationTopicFrame, deliveryKind: TopicFrameDeliveryKind) => void;
  private dirty = false;

  constructor(
    conversationId: number,
    title: string,
    emitFrame: (frame: ConversationTopicFrame, deliveryKind: TopicFrameDeliveryKind) => void,
  ) {
    this.conversationId = conversationId;
    this.sessionId = String(conversationId);
    this.subscriptionId = "sub-" + conversationId + "-" + Math.random().toString(36).slice(2, 8);
    this.logEpoch = String(conversationId);
    this.title = title;
    this.emitFrame = emitFrame;
  }

  nextRowId(): number {
    this.rowSeq += 1;
    return this.rowSeq;
  }

  nextAdmissionSeq(): number {
    this.admissionSeq += 1;
    return this.admissionSeq;
  }

  topic(): string {
    return conversationTopic(this.sessionId);
  }

  get isRunning(): boolean {
    return this.phase === "running";
  }

  /** held：turn 结束 + 队列非空 + autoDrain=false → 下一输入需要用户裁决。 */
  get isHeld(): boolean {
    return (
      !this.isRunning &&
      this.phase !== "draft" &&
      this.queue.length > 0 &&
      !this.autoDrain
    );
  }

  inputRoutingMode(): "startNow" | "enqueue" | "choice" {
    if (this.isHeld) return "choice";
    if (this.isRunning) return "enqueue";
    return "startNow";
  }

  queueState(): RowLite {
    const state: RowLite = { items: this.queue, autoDrain: this.autoDrain };
    if (this.pauseReason) state.pauseReason = this.pauseReason;
    return state;
  }

  pushFrame(payload: RowLite, deliveryKind: TopicFrameDeliveryKind = "online"): void {
    this.dirty = true;
    const frame = this.buildFrame(payload);
    // SessionDataLayer 在 transport.onFrame 上单监听并按 frame.topic 扇入各 store。
    this.emitFrame(frame, deliveryKind);
  }

  private buildFrame(payload: RowLite): ConversationTopicFrame {
    this.seq += 1;
    return {
      topic: this.topic(),
      subscriptionId: this.subscriptionId,
      fromSeq: this.seq,
      toSeq: this.seq,
      sentAt: nowIso(),
      payload,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;
  }

  /** Emit a fresh full snapshot frame (subscribe/resync/recovery). */
  emitSnapshot(deliveryKind: TopicFrameDeliveryKind = "online"): void {
    this.pushFrame({ kind: "snapshot", snapshot: this.snapshot() }, deliveryKind);
  }

  /** 队列 / inputRouting / control 变化时的轻量 delta。 */
  emitState(patch: RowLite): void {
    this.pushFrame({ kind: "deltas", deltas: [{ op: "state.updated", patch }] });
  }

  snapshot(): RowLite {
    return {
      protocolVersion: 1,
      sessionId: this.sessionId,
      logEpoch: this.logEpoch,
      seq: this.seq,
      revision: this.revision,
      control: {
        phase: this.phase,
        sessionEnded:
          this.phase === "completedSuccess" || this.phase === "completedInterrupted",
        canStop: this.phase === "running",
        stopState: this.phase === "running" ? "stoppable" : "idle",
        stopTargetKind: "unknown",
        activeWorks:
          this.phase === "running" ? [{ kind: "primaryTurn", startedAt: nowIso() }] : [],
        lastError: this.lastError,
        apiRetry: null,
      },
      availability: {
        fork: { allowed: true },
        compact: { allowed: false, reasonCode: "not-supported" },
        switchModelConfig: { allowed: true },
        setFollowupMode: { allowed: false, reasonCode: "not-supported" },
        queueEdit: { allowed: true },
        sendQueuedNow: { allowed: true },
        pauseGoal: { allowed: false, reasonCode: "not-supported" },
        resumeGoal: { allowed: false, reasonCode: "not-supported" },
      },
      inputRouting: { mode: this.inputRoutingMode() },
      meta: { title: this.title, titleSource: "default" },
      config: {
        provider: "codepluse",
        model: "",
        thought: "standard",
        thoughtLevels: [],
        followupMode: "queue",
        mode: "build",
      },
      modelTransition: null,
      // goal/plan 是必填 nullable 键（无 default）：缺失会让整份 snapshot 被 zod 拒收。
      goal: null,
      plan: null,
      usage: {
        contextWindow: null,
        cumulative: { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0 },
      },
      queue: this.queueState(),
      pendingInteractions: this.pendingInteractions,
      pendingCommands: [],
      backgroundWorks: [],
      workspaceHookAdmission: null,
      rows: {
        window: this.rows,
        totalCount: this.rows.length,
        firstRowId: this.rows.length > 0 ? this.rows[0].rowId : null,
      },
    };
  }

  // ── row synthesis from Go events ──

  ensureTurn(toolNameHint = ""): LiveTurnState {
    void toolNameHint;
    // One live turn at a time; a completed turn is closed by turn_complete.
    for (const turn of this.turns.values()) {
      if (turn.running) return turn;
    }
    const turnId = "turn-" + (this.turns.size + 1) + "-" + Date.now();
    const headerRowId = this.nextRowId();
    this.rows.push({
      kind: "turnHeader", rowId: headerRowId, turnId, entityId: turnId, createdAt: nowIso(),
      createdAtSeq: headerRowId, origin: "user", state: "running", outputTokens: null,
      modelDurationMs: null,
    });
    const turn: LiveTurnState = {
      turnId, headerRowId, userRowId: -1, assistantRowId: null, assistantText: "",
      reasoningRowId: null, reasoningText: "", toolRows: new Map(), running: true,
    };
    this.turns.set(headerRowId, turn);
    return turn;
  }

  closeRunningTurn(stopReason: string): void {
    for (const turn of this.turns.values()) {
      if (!turn.running) continue;
      turn.running = false;
      const header = this.rows.find((r) => r.rowId === turn.headerRowId);
      if (header) {
        header.state = stopReason === "end_turn" ? "completedSuccess" : "completedInterrupted";
      }
      if (turn.assistantRowId !== null) {
        const assistant = this.rows.find((r) => r.rowId === turn.assistantRowId);
        if (assistant && assistant.state === "streaming") assistant.state = "done";
      }
    }
  }
}

type SessionKey = string;

interface AttachmentCacheEntry {
  dataBase64: string;
  mime: string;
  bytes: number;
}

export class GoConversationTransport implements ConversationTransport {
  private sessions = new Map<SessionKey, GoConversationSession>();
  private frameListeners = new Set<FrameListener>();
  private restartListeners = new Set<RestartListener>();
  private rekeyListeners = new Set<RekeyListener>();
  private unsubscribeApi: (() => void) | null = null;
  private connections = new Map<SessionKey, string>();
  /** connectionId → sessionKey：draft 首 prompt 后 conversation_linked 反查用。 */
  private connectionToSession = new Map<string, SessionKey>();
  /** 已上传附件的本地缓存：attachmentRead/Range 无服务端下载端点时回放字节。 */
  private attachmentCache = new Map<string, AttachmentCacheEntry>();
  private agentType: AgentType = "codex";
  private workingDir: string | null = null;

  constructor(options?: { agentType?: AgentType; workingDir?: string }) {
    if (options?.agentType) this.agentType = options.agentType;
    if (options?.workingDir) this.workingDir = options.workingDir;
    this.bindGoEvents();
  }

  /** transport 级帧扇出：所有 session 的帧都经这里推给 onFrame 监听者。 */
  private dispatchFrame = (
    frame: ConversationTopicFrame,
    deliveryKind: TopicFrameDeliveryKind,
  ): void => {
    for (const listener of this.frameListeners) {
      try {
        listener(frame, { deliveryKind });
      } catch {
        // a broken listener must not kill the pump
      }
    }
  };

  /** 页面注入新会话默认执行端 / 工作目录（draft → acp_connect 参数）。 */
  setSessionDefaults(options: { agentType?: AgentType; workingDir?: string | null }): void {
    if (options.agentType) this.agentType = options.agentType;
    this.workingDir = options.workingDir ?? null;
  }

  /** draft 会话被 conversation_linked 重 key 后通知页面切换选中的会话。 */
  onSessionRekeyed(listener: RekeyListener): () => void {
    this.rekeyListeners.add(listener);
    return () => this.rekeyListeners.delete(listener);
  }

  /** Subscribe to the Go event pump once; fan events into live sessions. */
  private bindGoEvents(): void {
    if (this.unsubscribeApi) return;
    const transportApi = (globalThis as RowLite).__codepluseEventPump;
    if (!transportApi) return;
    this.unsubscribeApi = transportApi((event: RowLite) => {
      this.handleGoEvent(event);
    });
  }

  /** Find or open the ACP connection behind a conversation. */
  private async ensureConnection(conversationId: number, sessionId: string): Promise<string> {
    const cached = this.connections.get(sessionId);
    if (cached) return cached;
    if (conversationId > 0) {
      const found = await acpFindConnectionForConversation(conversationId, undefined, this.agentType);
      if (found?.connection_id) {
        this.connections.set(sessionId, found.connection_id);
        this.connectionToSession.set(found.connection_id, sessionId);
        return found.connection_id;
      }
    }
    const session = this.sessions.get(sessionId);
    const agentType = session?.agentType ?? this.agentType;
    const workingDir = session?.draft ? (session.workingDir ?? this.workingDir) : null;
    const connectionId = await acpConnect(agentType, workingDir ?? undefined);
    this.connections.set(sessionId, connectionId);
    this.connectionToSession.set(connectionId, sessionId);
    return connectionId;
  }

  // ── ConversationTransport ──

  async subscribe(params: SubscribeParams): Promise<V4ConversationSubscribeResult> {
    this.bindGoEvents();
    const sessionId = params.topic.startsWith("conversation/")
      ? params.topic.slice("conversation/".length)
      : params.topic;
    const conversationId = Number(sessionId);
    let session = this.sessions.get(sessionId);
    if (!session) {
      session = new GoConversationSession(
        Number.isNaN(conversationId) || conversationId <= 0 ? 0 : conversationId,
        "会话",
        this.dispatchFrame,
      );
      this.sessions.set(sessionId, session);
    }
    if (!session.draft) {
      await this.hydrateHistory(session);
    }
    return {
      ack: {
        subscriptionId: session.subscriptionId,
        mode: "snapshot",
        logEpoch: session.logEpoch,
      },
    };
  }

  /**
   * store 写入 ACK subscriptionId 后调用；此处才推 initial snapshot——
   * store 的代际防护要求帧的 subscriptionId 与已提交的 ACK 一致，
   * 提前发帧会被丢弃（订阅后永远等不到首帧）。
   */
  activate(subscriptionId: string): void {
    for (const session of this.sessions.values()) {
      if (session.subscriptionId === subscriptionId) {
        session.emitSnapshot("initial");
        return;
      }
    }
  }

  async resync(params: ConversationResyncParams): Promise<V4ConversationResyncResult> {
    // ConversationResyncParams 以 subscriptionId 反查（topic 由 host registry 持有）。
    let session: GoConversationSession | undefined;
    for (const candidate of this.sessions.values()) {
      if (candidate.subscriptionId === params.subscriptionId) {
        session = candidate;
        break;
      }
    }
    if (session) session.emitSnapshot("recovery");
    return {
      ack: {
        subscriptionId: session?.subscriptionId ?? "resync-unknown",
        mode: "snapshot",
        logEpoch: session?.logEpoch ?? "0",
      },
    };
  }

  async unsubscribe(subscriptionId: string): Promise<void> {
    for (const [key, session] of this.sessions) {
      if (session.subscriptionId === subscriptionId) this.sessions.delete(key);
    }
  }

  async sendCommand(envelope: CommandEnvelope): Promise<CommandAck> {
    const session = this.findSessionByEnvelope(envelope);
    const reject = (message: string, reasonCode?: string): CommandAck =>
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      ({
        status: "rejected", reason: message, ...(reasonCode ? { reasonCode } : {}),
        revisionAtDecision: session?.revision ?? 0,
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
      } as any);
    // createSession 的 sessionId 为 null，不经 session 表。
    if (envelope.type === "createSession") {
      return this.handleCreateSession(envelope);
    }
    if (!session) return reject("会话未订阅");
    session.revision += 1;
    try {
      switch (envelope.type) {
        case "sendText":
          return await this.handleSendText(session, envelope);
        case "stop": {
          const connectionId = await this.ensureConnection(Number(session.sessionId), session.sessionId);
          await acpCancel(connectionId);
          session.closeRunningTurn("interrupted");
          session.phase = "completedInterrupted";
          // ZCode 语义：stop 暂停队列（autoDrain=false），恢复走 setAutoDrain/Resume。
          session.autoDrain = false;
          session.pauseReason = "stopped";
          session.emitState({
            control: { phase: "completedInterrupted", sessionEnded: true, canStop: false, stopState: "idle" },
            queue: session.queueState(),
            inputRouting: { mode: session.inputRoutingMode() },
          });
          return ack(envelope, session.revision);
        }
        case "resolveInteraction": {
          const payload = envelope.payload as { interactionId: string; answer?: { optionId?: string; freeText?: string }; optionId?: string; option?: string; feedback?: string };
          const connectionId = await this.ensureConnection(Number(session.sessionId), session.sessionId);
          const optionId = payload.answer?.optionId ?? payload.optionId ?? payload.option ?? "";
          await acpRespondPermission(connectionId, payload.interactionId, optionId);
          session.pendingInteractions = session.pendingInteractions.filter(
            (i) => i.interactionId !== payload.interactionId,
          );
          session.emitState({ pendingInteractions: session.pendingInteractions });
          return ack(envelope, session.revision);
        }
        case "switchModelConfig": {
          const payload = envelope.payload as { model?: string; modeId?: string };
          const connectionId = await this.ensureConnection(Number(session.sessionId), session.sessionId);
          if (payload.model) await acpSetConfigOption(connectionId, "model", payload.model);
          if (payload.modeId) await acpSetConfigOption(connectionId, "mode", payload.modeId);
          return ack(envelope, session.revision);
        }
        case "renameSession": {
          const payload = envelope.payload as { title?: string };
          if (payload.title) {
            await updateConversationTitle(Number(session.sessionId), payload.title);
            session.title = payload.title;
            session.emitState({ meta: { title: payload.title, titleSource: "custom" } });
          }
          return ack(envelope, session.revision);
        }
        case "deleteSession":
          if (!session.draft) {
            await deleteConversation(Number(session.sessionId));
          }
          this.connections.delete(session.sessionId);
          this.sessions.delete(session.sessionId);
          return ack(envelope, session.revision);
        case "sendQueuedNow":
          return await this.handleSendQueuedNow(session, envelope);
        case "editQueueItem":
          return this.handleEditQueueItem(session, envelope);
        case "reorderQueueItem":
          return this.handleReorderQueueItem(session, envelope);
        case "deleteQueueItem":
          return this.handleDeleteQueueItem(session, envelope);
        case "setAutoDrain":
          return await this.handleSetAutoDrain(session, envelope);
        case "forkAssistant":
          return await this.handleForkAssistant(session, envelope);
        default:
          return reject("Go 后端尚未实现该命令：" + envelope.type);
      }
    } catch (error) {
      return reject(error instanceof Error ? error.message : String(error));
    }
  }

  private rejectAck(
    session: GoConversationSession | undefined,
    message: string,
    reasonCode?: string,
  ): CommandAck {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return {
      status: "rejected",
      reason: message,
      ...(reasonCode ? { reasonCode } : {}),
      revisionAtDecision: session?.revision ?? 0,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;
  }

  // ── sendText：inputRouting 三态裁决（任务3）──

  private async handleSendText(
    session: GoConversationSession,
    envelope: CommandEnvelope,
  ): Promise<CommandAck> {
    const payload = envelope.payload as {
      text?: string;
      blocks?: RowLite[];
      attachments?: AttachmentRef[];
      requestedDelivery?: "startNow" | "queue" | "guide";
      heldQueueDisposition?: "clearQueueAndSend" | "keepQueueAndSend";
    };
    const attachments = payload.attachments ?? [];
    const requested = payload.requestedDelivery ?? "startNow";
    const heldDisposition = payload.heldQueueDisposition;

    // held（choice）状态：页面在无 disposition 时会先返回 confirmationRequired；
    // 这里兜底拒绝，避免静默改变队列语义。
    if (session.isHeld && !heldDisposition) {
      return this.rejectAck(session, "队列已暂停，需要确认清空或保留后发送", "heldQueueConfirmationRequired");
    }

    if (heldDisposition === "clearQueueAndSend") {
      session.queue = [];
    }

    const direct = !session.isRunning && requested !== "queue" && requested !== "guide";
    if (direct) {
      const sent = await this.dispatchPrompt(session, envelope, payload.text ?? "", attachments);
      if (sent) {
        return ackResult(envelope, session.revision, {
          type: "inputAccepted",
          delivery: "startNow",
          inputId: envelope.commandId,
        });
      }
      // dispatchPrompt 失败（turn_in_progress 等）→ 回落入队路径。
    }

    if (requested === "guide") {
      // Go 后端无 guide 语义；降级为直发/入队由后续 turn 处理——先入队并标注。
    }

    const queueItem = this.enqueueQueueItem(session, envelope, payload.text ?? "", attachments, requested);
    // 空闲 + autoDrain：显式入队的输入立即提升（与 turn_complete 后自动排水一致）。
    if (requested !== "guide") this.maybeAutoDrain(session);
    return ackResult(envelope, session.revision, {
      type: "inputAccepted",
      delivery: "queue",
      inputId: queueItem.queueItemId,
    });
  }

  private enqueueQueueItem(
    session: GoConversationSession,
    envelope: CommandEnvelope,
    text: string,
    attachments: readonly AttachmentRef[],
    requested: "startNow" | "queue" | "guide",
  ): QueueItem {
    // queueItemSchema = conversationInputIntent + dispatch.state ∈ queued/reserved/promoting。
    const item: QueueItem = {
      sourceCommandId: envelope.commandId,
      queueItemId: "q-" + uuidv7(),
      clientId: envelope.clientId,
      kind: "sendText",
      text,
      attachments: [...attachments],
      delivery: { requested, admitted: "queue" },
      order: { admissionSeq: session.nextAdmissionSeq() },
      steer: { state: "notRequested" },
      dispatch: { state: "queued" },
      admittedAt: Date.now(),
    };
    session.queue = [...session.queue, item];
    session.emitState({
      queue: session.queueState(),
      inputRouting: { mode: session.inputRoutingMode() },
    });
    return item;
  }

  /**
   * 直发一条 prompt（真实/草稿会话通）。乐观插入 turnHeader+userInput 行后调
   * acp_prompt；draft 会话先 acpConnect（Go 端首条 prompt 创建真实会话）。
   * 返回 false = turn_in_progress（调用方回落入队）。
   */
  private async dispatchPrompt(
    session: GoConversationSession,
    envelope: CommandEnvelope,
    text: string,
    attachments: readonly AttachmentRef[],
  ): Promise<boolean> {
    const blocks: RowLite[] = [];
    if (text.trim().length > 0) blocks.push({ type: "text", text });
    blocks.push(...attachmentRefsToPromptBlocks(attachments));

    const turn = session.ensureTurn();
    const userRowId = session.nextRowId();
    turn.userRowId = userRowId;
    session.rows.push({
      kind: "userInput", rowId: userRowId, turnId: turn.turnId, entityId: "u" + userRowId,
      createdAt: nowIso(), createdAtSeq: userRowId, text,
      origin: { kind: "userComposer", sourceCommandId: envelope.commandId },
      attachments: [...attachments], conversationQuotes: [], epilogueStart: null,
    });
    session.phase = "running";
    session.pushFrame({
      kind: "deltas",
      deltas: [
        { op: "row.appended", row: session.rows[session.rows.length - 1] },
        {
          op: "state.updated",
          patch: {
            control: { phase: "running", sessionEnded: false, canStop: true, stopState: "stoppable" },
            inputRouting: { mode: session.inputRoutingMode() },
            queue: session.queueState(),
          },
        },
      ],
    });

    let connectionId: string;
    try {
      connectionId = await this.ensureConnection(session.conversationId, session.sessionId);
      const conversationId = session.draft ? null : Number(session.sessionId);
      await acpPrompt(
        connectionId,
        blocks as never,
        null,
        conversationId,
        envelope.commandId,
      );
      return true;
    } catch (error) {
      // 失败（turn_in_progress / 连接失败 / 执行端不可用）：撤回乐观行，
      // 交由调用方回落入队或显式报错，避免留下幽灵输入行。
      session.rows = session.rows.filter(
        (row) => row.rowId !== userRowId && row.rowId !== turn.headerRowId,
      );
      this.removeTurn(session, turn.turnId);
      session.phase = session.queue.length > 0 && !session.autoDrain ? "completedInterrupted" : "completedSuccess";
      session.emitState({
        control: { phase: session.phase, sessionEnded: true, canStop: false, stopState: "idle" },
        inputRouting: { mode: session.inputRoutingMode() },
      });
      if (error instanceof TurnBusyError) return false;
      throw error;
    }
  }

  private removeTurn(session: GoConversationSession, turnId: string): void {
    for (const [headerRowId, turn] of session.turns) {
      if (turn.turnId === turnId) {
        session.turns.delete(headerRowId);
        return;
      }
    }
  }

  // ── 队列命令（任务3）──

  private async promoteQueueItem(
    session: GoConversationSession,
    item: QueueItem,
  ): Promise<void> {
    item.dispatch.state = "promoting";
    session.emitState({ queue: session.queueState() });
    const envelope: CommandEnvelope = {
      commandId: item.sourceCommandId,
      clientId: item.clientId,
      sessionId: session.sessionId,
      type: "sendText",
      payload: { text: item.text, attachments: item.attachments },
      issuedAt: Date.now(),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;
    try {
      // 提升前 stop 当前 turn（sendQueuedNow 语义）；dispatchPrompt 内部处理
      // 乐观行、连接与 acp_prompt，失败（含 turn_in_progress）时统一回队。
      const connectionId = await this.ensureConnection(session.conversationId, session.sessionId);
      if (session.isRunning) await acpCancel(connectionId);
      const sent = await this.dispatchPrompt(session, envelope, item.text, item.attachments);
      if (sent) {
        session.queue = session.queue.filter((candidate) => candidate.queueItemId !== item.queueItemId);
        session.emitState({
          queue: session.queueState(),
          inputRouting: { mode: session.inputRoutingMode() },
        });
        return;
      }
      throw new Error("turn_in_progress");
    } catch (error) {
      // 提升失败：回到队首，等下一轮 turn_complete / sendQueuedNow 再试。
      item.dispatch.state = "queued";
      if (!session.queue.some((candidate) => candidate.queueItemId === item.queueItemId)) {
        session.queue = [item, ...session.queue];
      }
      session.emitState({ queue: session.queueState() });
      void error;
    }
  }

  private maybeAutoDrain(session: GoConversationSession): void {
    if (!session.autoDrain || session.isRunning || session.queue.length === 0) return;
    const item = session.queue[0];
    void this.promoteQueueItem(session, item);
  }

  private async handleSendQueuedNow(
    session: GoConversationSession,
    envelope: CommandEnvelope,
  ): Promise<CommandAck> {
    const payload = envelope.payload as { queueItemId: string };
    const item = session.queue.find((candidate) => candidate.queueItemId === payload.queueItemId);
    if (!item) return this.rejectAck(session, "队列项不存在", "queue.itemNotFound");
    session.queue = session.queue.filter((candidate) => candidate.queueItemId !== item.queueItemId);
    session.emitState({ queue: session.queueState() });
    // sendQueuedNow 语义 = stop 当前 + 立即消费该项。
    try {
      const connectionId = await this.ensureConnection(session.conversationId, session.sessionId);
      if (session.isRunning) await acpCancel(connectionId);
    } catch {
      // cancel 失败不阻塞提升尝试；prompt 会以 turn_in_progress 失败并回队。
    }
    await this.promoteQueueItem(session, item);
    return ack(envelope, session.revision);
  }

  private handleEditQueueItem(
    session: GoConversationSession,
    envelope: CommandEnvelope,
  ): CommandAck {
    const payload = envelope.payload as { queueItemId: string; newText: string };
    const item = session.queue.find((candidate) => candidate.queueItemId === payload.queueItemId);
    if (!item) return this.rejectAck(session, "队列项不存在", "queue.itemNotFound");
    item.text = payload.newText;
    session.emitState({ queue: session.queueState() });
    return ack(envelope, session.revision);
  }

  private handleReorderQueueItem(
    session: GoConversationSession,
    envelope: CommandEnvelope,
  ): CommandAck {
    const payload = envelope.payload as { queueItemId: string; beforeQueueItemId: string | null };
    const index = session.queue.findIndex((candidate) => candidate.queueItemId === payload.queueItemId);
    if (index < 0) return this.rejectAck(session, "队列项不存在", "queue.itemNotFound");
    const [item] = session.queue.splice(index, 1);
    if (payload.beforeQueueItemId == null) {
      session.queue.push(item);
    } else {
      const before = session.queue.findIndex((candidate) => candidate.queueItemId === payload.beforeQueueItemId);
      session.queue.splice(before < 0 ? session.queue.length : before, 0, item);
    }
    session.emitState({ queue: session.queueState() });
    return ack(envelope, session.revision);
  }

  private handleDeleteQueueItem(
    session: GoConversationSession,
    envelope: CommandEnvelope,
  ): CommandAck {
    const payload = envelope.payload as { queueItemId: string };
    session.queue = session.queue.filter((candidate) => candidate.queueItemId !== payload.queueItemId);
    session.emitState({
      queue: session.queueState(),
      inputRouting: { mode: session.inputRoutingMode() },
    });
    return ack(envelope, session.revision);
  }

  private async handleSetAutoDrain(
    session: GoConversationSession,
    envelope: CommandEnvelope,
  ): Promise<CommandAck> {
    const payload = envelope.payload as { autoDrain: boolean };
    session.autoDrain = payload.autoDrain;
    if (payload.autoDrain) session.pauseReason = undefined;
    session.emitState({
      queue: session.queueState(),
      inputRouting: { mode: session.inputRoutingMode() },
    });
    if (payload.autoDrain) this.maybeAutoDrain(session);
    return ack(envelope, session.revision);
  }

  // ── draft 会话（任务2）──

  private handleCreateSession(envelope: CommandEnvelope): CommandAck {
    const payload = envelope.payload as {
      sessionId?: string;
      workspaceId?: string;
      firstInput?: { text?: string; attachments?: AttachmentRef[] };
      config?: { provider?: string; model?: string };
    };
    const draftId = payload.sessionId ?? "draft-" + uuidv7();
    if (this.sessions.has(draftId)) {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      return {
        status: "accepted",
        commandId: envelope.commandId,
        revisionAtDecision: 0,
        result: { type: "createSession", sessionId: draftId },
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
      } as any;
    }
    const session = new GoConversationSession(0, "新会话", this.dispatchFrame);
    session.sessionId = draftId;
    session.draft = true;
    session.phase = "draft";
    session.agentType = this.agentType;
    session.workingDir = this.workingDir;
    this.sessions.set(draftId, session);
    this.sessions.get(draftId)!.emitSnapshot("initial");
    // firstInput 缺省：页面在 draft 会话里继续用 sendText 发起（走 dispatchPrompt draft 路径）。
    return ackResult(envelope, 0, { type: "createSession", sessionId: draftId });
  }

  // ── fork（任务5）：Go 原生 acp_fork（点分叉，当前会话行重指 fork）──

  private async handleForkAssistant(
    session: GoConversationSession,
    envelope: CommandEnvelope,
  ): Promise<CommandAck> {
    const payload = envelope.payload as { target?: { rowId?: number } };
    const rowId = Number(payload.target?.rowId ?? NaN);
    const row = session.rows.find((candidate) => candidate.rowId === rowId);
    if (!row || row.kind !== "assistantText") {
      return this.rejectAck(session, "只能从 assistant 消息分叉", "fork.invalidTarget");
    }
    // hydrateHistory 在 assistant 行上记 sourceTurnId（Go turn.id / 原生 message id）。
    const forkFromTurnId = String(row.sourceTurnId ?? "");
    if (!forkFromTurnId) {
      return this.rejectAck(session, "该消息缺少可靠的原生分叉位置（等待轮次落库后重试）", "fork.noNativeTurn");
    }
    try {
      const connectionId = await this.ensureConnection(session.conversationId, session.sessionId);
      const result = await acpFork(
        connectionId,
        Number(session.sessionId),
        null,
        forkFromTurnId,
      );
      // acp_fork 语义：当前会话行重指 fork（turns 截到分叉点），原会话保留为 sibling。
      session.rows = [];
      session.turns.clear();
      await this.hydrateHistory(session);
      session.emitSnapshot("recovery");
      return ackResult(envelope, session.revision, {
        type: "forkAssistant",
        sessionId: String(result.forkedSessionId ?? ""),
      });
    } catch (error) {
      if (error instanceof TurnBusyError) {
        return this.rejectAck(session, "轮次进行中，稍后再试", "fork.turnInProgress");
      }
      return this.rejectAck(session, error instanceof Error ? error.message : String(error), "fork.failed");
    }
  }

  async queryCommands(_params: CommandsQueryParams): Promise<CommandsQueryResult> {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return { commands: [], hasMore: false } as any;
  }

  async rowsRange(params: V4ConversationRowsRangeParams): Promise<V4ConversationRowsRangeResult> {
    const session = this.findSession(params.sessionId ?? "");
    if (!session) {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      return { rows: [], firstRowId: null } as any;
    }
    const before = params.beforeRowId ?? Number.MAX_SAFE_INTEGER;
    const limit = params.limit ?? 100;
    const slice = session.rows.filter((r) => r.rowId < before).slice(-limit);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return { rows: slice, firstRowId: session.rows.length > 0 ? session.rows[0].rowId : null } as any;
  }

  onFrame(listener: FrameListener): () => void {
    this.frameListeners.add(listener);
    return () => this.frameListeners.delete(listener);
  }

  onAssemblyFault(listener: (fault: RowLite) => void): () => void {
    // Snapshot resync recovers everything; faults are surfaced via resync only.
    void listener;
    return () => undefined;
  }

  onRuntimeRestart(listener: RestartListener): () => void {
    this.restartListeners.add(listener);
    return () => this.restartListeners.delete(listener);
  }

  // Optional capabilities: the UI degrades gracefully when they resolve empty.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async plans(): Promise<any> {
    return { plans: [] };
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async fileChanges(): Promise<any> {
    return { files: [], worktreeChangedFiles: [] };
  }
  async fileRewindPreview(): Promise<any> {
    // Go 侧无按轮次文件撤销端点（评估结论见任务报告）；预览永远为空 =
    // UI 的文件回退入口保持不可用。
    return { safeFiles: [], unsafeFiles: [] };
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async workflowRunEvents(): Promise<any> {
    return { events: [], hasMore: false };
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async workflowRuns(): Promise<any> {
    return { runs: [] };
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async workflowRunArtifacts(): Promise<any> {
    return { artifacts: [], hasMore: false };
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async workflowRunArtifactData(): Promise<any> {
    return { entries: [], hasMore: false };
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async workflowRunArtifactRead(): Promise<any> {
    return {
      bytes: new Uint8Array(),
      mediaType: "application/octet-stream",
      totalBytes: 0,
      nextOffset: null,
    };
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async workflowRunWorkspace(): Promise<any> {
    return { entries: [], hasMore: false };
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async workflowRunNodeResult(): Promise<any> {
    return null;
  }

  // ── 附件（任务1）──

  /**
   * V4AttachmentPutParams（base64 字节）→ POST /api/upload_attachment（multipart）
   * → ref = file://<uploads 绝对路径>。Go 端 imageData 只接受 uploads 目录内的
   * file:// uri，ref 同时作为 AttachmentRef.ref 供 sendText 转换与 userInput chips 展示。
   */
  async attachmentPut(
    params?: V4AttachmentPutParams,
    options?: AttachmentUploadOptions,
  ): Promise<V4AttachmentPutResult> {
    if (!params) return { ref: "" };
    if (options?.signal?.aborted) {
      const error = new Error("Attachment upload canceled");
      error.name = "AbortError";
      throw error;
    }
    const bytes = base64ToBytes(params.dataBase64);
    // File 构造器在浏览器环境可用；codeg-ui 为 web 页面（Go 服务端同源上传）。
    const file = new File([bytes as BlobPart], params.fileName, { type: params.mime });
    options?.onProgress?.({ phase: "uploading", uploadedBytes: 0, totalBytes: bytes.byteLength });
    const uploaded = await uploadAttachment(file, params.sessionId);
    options?.onProgress?.({
      phase: "committing",
      uploadedBytes: bytes.byteLength,
      totalBytes: bytes.byteLength,
    });
    const ref = buildFileUri(uploaded.path);
    this.attachmentCache.set(ref, {
      dataBase64: params.dataBase64,
      mime: params.mime || uploaded.mimeType || guessMimeFromName(params.fileName),
      bytes: uploaded.size || bytes.byteLength,
    });
    options?.onProgress?.({
      phase: "committing",
      uploadedBytes: bytes.byteLength,
      totalBytes: bytes.byteLength,
    });
    return { ref };
  }

  /**
   * 已发送附件读取：优先回放本页上传时缓存的字节；缓存未命中（刷新后恢复的
   * 历史消息）经 read_workspace_file_base64 读回（uploads 目录位于服务 Root 之下，
   * 传空 rootPath 以服务默认根解析）。Go 端无独立附件下载端点。
   */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async attachmentRead(params?: any): Promise<any> {
    const ref = String(params?.ref ?? "");
    const cached = this.attachmentCache.get(ref);
    if (cached) {
      return { bytes: base64ToBytes(cached.dataBase64), mediaType: cached.mime };
    }
    if (!ref) {
      return { bytes: new Uint8Array(), mediaType: "application/octet-stream" };
    }
    const path = fileRefToOsPath(ref);
    const dataBase64 = await readWorkspaceFileBase64("", path);
    return {
      bytes: base64ToBytes(dataBase64),
      mediaType: params?.mediaType || guessMimeFromName(path),
    };
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async attachmentReadRange(params?: any): Promise<any> {
    const full = await this.attachmentRead(params);
    const offset = Number(params?.offset ?? 0);
    const limit = Number(params?.limit ?? Number.MAX_SAFE_INTEGER);
    const slice = full.bytes.subarray(offset, offset + limit);
    return {
      bytes: slice,
      mediaType: full.mediaType,
      totalBytes: full.bytes.byteLength,
      nextOffset: offset + limit < full.bytes.byteLength ? offset + limit : null,
    };
  }

  // ── Go event ingestion ──

  /** Translate one Go acp://event into v4 deltas for the owning session. */
  handleGoEvent(event: RowLite): void {
    const connectionId = String(event.connection_id ?? "");
    const conversationId = Number(event.conversation_id ?? 0);
    const type = String(event.type ?? "");
    let session = this.sessions.get(conversationId > 0 ? String(conversationId) : "");

    if (type === "conversation_linked") {
      this.handleConversationLinked(event, connectionId, conversationId);
      return;
    }
    if (!session) {
      const mapped = this.connectionToSession.get(connectionId);
      session = mapped ? this.sessions.get(mapped) : undefined;
    }
    if (!session || session.draft) return;
    session.revision += 1;

    switch (type) {
      case "content_delta":
      case "thinking":
      case "tool_call":
      case "tool_call_update": {
        const turn = session.ensureTurn();
        if (type === "content_delta") {
          if (turn.assistantRowId === null) {
            turn.assistantRowId = session.nextRowId();
            turn.assistantText = "";
            session.rows.push({
              kind: "assistantText", rowId: turn.assistantRowId, turnId: turn.turnId,
              entityId: "a" + turn.assistantRowId, createdAt: nowIso(), createdAtSeq: turn.assistantRowId,
              text: "", state: "streaming", model: null, feedback: null,
            });
            session.pushFrame({
              kind: "deltas",
              deltas: [{ op: "row.appended", row: session.rows[session.rows.length - 1] }],
            });
          }
          const delta = String(event.text ?? "");
          turn.assistantText += delta;
          const row = session.rows.find((r) => r.rowId === turn.assistantRowId);
          if (row) row.text = turn.assistantText;
          session.pushFrame({
            kind: "deltas",
            deltas: [{ op: "row.delta", rowId: turn.assistantRowId, path: "text", delta }],
          });
        } else if (type === "thinking") {
          if (turn.reasoningRowId === null) {
            turn.reasoningRowId = session.nextRowId();
            session.rows.push({
              kind: "reasoning", rowId: turn.reasoningRowId, turnId: turn.turnId,
              entityId: "r" + turn.reasoningRowId, createdAt: nowIso(), createdAtSeq: turn.reasoningRowId,
              text: "", state: "streaming",
            });
          }
          const delta = String(event.text ?? "");
          turn.reasoningText += delta;
          const row = session.rows.find((r) => r.rowId === turn.reasoningRowId);
          if (row) row.text = turn.reasoningText;
          session.pushFrame({
            kind: "deltas",
            deltas: [{ op: "row.delta", rowId: turn.reasoningRowId, path: "text", delta }],
          });
        } else {
          const toolCallId = String(event.tool_call_id ?? "");
          let rowId = turn.toolRows.get(hashId(toolCallId));
          if (rowId === undefined) {
            rowId = session.nextRowId();
            turn.toolRows.set(hashId(toolCallId), rowId);
            session.rows.push({
              kind: "toolCall", rowId, turnId: turn.turnId, entityId: toolCallId,
              createdAt: nowIso(), createdAtSeq: rowId, toolCallId, toolName: String(event.title ?? "tool"),
              status: String(event.status ?? "running"), inputText: "", input: event.raw_input ?? null,
              output: null, display: null, progress: null, approvalInteractionId: null,
              error: null, outputPreview: null,
            });
            session.pushFrame({
              kind: "deltas",
              deltas: [{ op: "row.appended", row: session.rows[session.rows.length - 1] }],
            });
          } else {
            const row = session.rows.find((r) => r.rowId === rowId);
            if (row) {
              row.status = String(event.status ?? row.status);
              if (event.raw_output !== undefined) row.output = event.raw_output;
              if (event.raw_input !== undefined) row.input = event.raw_input;
            }
            session.pushFrame({
              kind: "deltas",
              deltas: [{ op: "row.upserted", row: session.rows.find((r) => r.rowId === rowId) }],
            });
          }
        }
        break;
      }
      case "plan_update": {
        // Rendered as plan guidance on the latest assistant row's turn.
        session.pushFrame({
          kind: "deltas",
          deltas: [{ op: "state.updated", patch: { plan: { items: event.entries ?? [], updatedAt: Date.now() } } }],
        });
        break;
      }
      case "permission_request": {
        const interactionId = String(event.request_id ?? "");
        const pending = {
          interactionId,
          kind: "permission",
          anchorRowId: null,
          createdAt: nowIso(),
          payload: {
            kind: "permission",
            toolCallId: String(event.tool_call?.tool_call_id ?? ""),
            toolName: String(event.tool_call?.title ?? event.tool_call?.tool_name ?? "tool"),
            summary: String(event.tool_call?.title ?? "权限请求"),
            detail: event.options ?? null,
            options: (event.options ?? []).map((o: RowLite) => ({
              optionId: String(o.option_id),
              label: String(o.name),
              kind: permissionKind(o.kind),
            })),
          },
        };
        session.pendingInteractions = [...session.pendingInteractions, pending];
        session.emitState({ pendingInteractions: session.pendingInteractions });
        break;
      }
      case "permission_resolved": {
        const requestId = String(event.request_id ?? "");
        session.pendingInteractions = session.pendingInteractions.filter(
          (i) => i.interactionId !== requestId,
        );
        session.emitState({ pendingInteractions: session.pendingInteractions });
        break;
      }
      case "turn_complete": {
        session.closeRunningTurn(String(event.stop_reason ?? "end_turn"));
        session.phase = "completedSuccess";
        session.emitState({
          control: { phase: "completedSuccess", sessionEnded: true, canStop: false, stopState: "idle" },
          inputRouting: { mode: session.inputRoutingMode() },
        });
        // autoDrain：turn 结束自动提升下一条队列项。
        this.maybeAutoDrain(session);
        break;
      }
      case "error": {
        session.lastError = {
          code: "agent_error", message: String(event.message ?? "未知错误"),
          recoverable: true, at: nowIso(), source: "runtime",
        };
        session.phase = "error";
        // ZCode 语义：error 暂停队列（pauseReason=error），恢复需 setAutoDrain(true)。
        session.autoDrain = false;
        session.pauseReason = "error";
        session.emitState({
          control: { phase: "error", sessionEnded: true, lastError: session.lastError },
          queue: session.queueState(),
          inputRouting: { mode: session.inputRoutingMode() },
        });
        break;
      }
      case "status_changed":
      case "session_notice":
      case "conversation_status_changed":
      default:
        break;
    }
  }

  /**
   * conversation_linked：Go 在 prompt(conversationId=null) 创建真实会话后广播。
   * 把 draft 会话重 key 到真实 id，重水化历史（含刚发出的 user turn），再向页面
   * 发 rekey 通知切换选中会话。
   */
  private handleConversationLinked(
    event: RowLite,
    connectionId: string,
    conversationId: number,
  ): void {
    void event;
    if (!(conversationId > 0)) return;
    const realId = String(conversationId);
    if (this.sessions.has(realId)) return;
    const draftKey =
      this.connectionToSession.get(connectionId) ??
      [...this.sessions.entries()].find(([, candidate]) => candidate.draft)?.[0];
    if (!draftKey) return;
    const draft = this.sessions.get(draftKey);
    if (!draft || !draft.draft) return;
    this.sessions.delete(draftKey);
    this.connectionToSession.set(connectionId, realId);
    draft.draft = false;
    draft.conversationId = conversationId;
    draft.sessionId = realId;
    draft.logEpoch = realId;
    draft.phase = "running";
    draft.rows = [];
    draft.turns.clear();
    this.sessions.set(realId, draft);
    void this.hydrateHistory(draft).then(() => {
      draft.emitSnapshot("recovery");
    });
    for (const listener of this.rekeyListeners) {
      try {
        listener(draftKey, realId);
      } catch {
        // 页面监听异常不影响适配器状态。
      }
    }
  }

  private async hydrateHistory(session: GoConversationSession): Promise<void> {
    // 并发水化去重：rekey 与 subscribe 可能同时触发；只有最新一次的结果生效，
    // 旧调用在 await 返回后丢弃，避免历史行重复追加。
    const epoch = ++session.hydrateEpoch;
    try {
      const conversationId = Number(session.sessionId);
      if (!Number.isFinite(conversationId) || conversationId <= 0) return;
      // 全量水化：订阅/rekey/分叉后都以权威历史重建行窗，避免重复追加。
      session.rows = [];
      session.turns.clear();
      const detail = await getFolderConversation(conversationId);
      if (epoch !== session.hydrateEpoch) return;
      if (detail?.summary?.title) session.title = detail.summary.title;
      const list = detail?.turns ?? [];
      for (const turn of list) {
        const turnId = String(turn.id ?? "turn-" + session.nextRowId());
        const headerRowId = session.nextRowId();
        session.rows.push({
          kind: "turnHeader", rowId: headerRowId, turnId, entityId: turnId,
          createdAt: String(turn.timestamp ?? nowIso()), createdAtSeq: headerRowId,
          origin: "user", state: "completedSuccess", outputTokens: null, modelDurationMs: null,
        });
        for (const block of (turn.blocks ?? []) as RowLite[]) {
          const rowId = session.nextRowId();
          const type = String(block.type ?? "text");
          if (type === "text") {
            const text = String(block.text ?? "");
            const isUser = String(turn.role) === "user";
            session.rows.push(
              isUser
                ? { kind: "userInput", rowId, turnId, entityId: "u" + rowId, createdAt: String(turn.timestamp ?? nowIso()), createdAtSeq: rowId, text, origin: { kind: "userComposer" }, attachments: [], conversationQuotes: [], epilogueStart: null }
                : { kind: "assistantText", rowId, turnId, entityId: "a" + rowId, createdAt: String(turn.timestamp ?? nowIso()), createdAtSeq: rowId, text, state: "done", model: turn.model ?? null, feedback: null, sourceTurnId: String(block.source_turn_id ?? turn.source_turn_id ?? "") },
            );
          } else if (type === "thinking") {
            session.rows.push({
              kind: "reasoning", rowId, turnId, entityId: "r" + rowId,
              createdAt: String(turn.timestamp ?? nowIso()), createdAtSeq: rowId,
              text: String(block.text ?? ""), state: "done",
            });
          } else if (type === "tool_call") {
            session.rows.push({
              kind: "toolCall", rowId, turnId, entityId: String(block.tool_call_id ?? "t" + rowId),
              createdAt: String(turn.timestamp ?? nowIso()), createdAtSeq: rowId,
              toolCallId: String(block.tool_call_id ?? ""), toolName: String(block.title ?? block.tool_name ?? "tool"),
              status: "success", inputText: "", input: (block.raw_input as RowLite) ?? null,
              output: (block.raw_output as RowLite) ?? null, display: null, progress: null,
              approvalInteractionId: null, error: null, outputPreview: null,
            });
          } else if (type === "image" && turn.role === "user") {
            // 历史图片块 → userInput 附件 chip（file:// uri 才能回读；内联 data 无 uri 时跳过）。
            const uri = String(block.uri ?? "");
            if (uri) {
              const dataBase64 = String(block.data ?? "");
              const ref: AttachmentRef = {
                ref: uri.startsWith("file://") ? uri : buildFileUri(uri),
                fileName: fileRefToOsPath(uri).split(/[\\/]/).pop() || "image",
                mime: String(block.mime_type ?? "image/png"),
                bytes: base64ByteLength(dataBase64),
              };
              session.rows.push({
                kind: "userInput", rowId, turnId, entityId: "u" + rowId,
                createdAt: String(turn.timestamp ?? nowIso()), createdAtSeq: rowId, text: "",
                origin: { kind: "userComposer" }, attachments: [ref],
                conversationQuotes: [], epilogueStart: null,
              });
            }
          }
        }
      }
    } catch {
      // History hydration is best effort; the live stream still works.
    }
  }

  private findSession(topic: string): GoConversationSession | undefined {
    for (const session of this.sessions.values()) {
      if (session.topic() === topic) return session;
    }
    return undefined;
  }

  private findSessionByEnvelope(envelope: CommandEnvelope): GoConversationSession | undefined {
    const sessionId = (envelope as RowLite).sessionId;
    if (sessionId) {
      const direct = this.sessions.get(String(sessionId));
      if (direct) return direct;
    }
    return undefined;
  }
}

function ack(envelope: CommandEnvelope, revision: number): CommandAck {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return { status: "accepted", commandId: envelope.commandId, revisionAtDecision: revision } as any;
}

function ackResult(envelope: CommandEnvelope, revision: number, result: RowLite): CommandAck {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return { status: "accepted", commandId: envelope.commandId, revisionAtDecision: revision, result } as any;
}

function permissionKind(kind: string): "allowOnce" | "allowAlways" | "deny" | "custom" {
  if (kind === "allow_always") return "allowAlways";
  if (kind === "deny" || kind === "reject_once") return "deny";
  return "allowOnce";
}

function hashId(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) {
    h = (h * 31 + id.charCodeAt(i)) | 0;
  }
  return Math.abs(h);
}

export function createGoConversationTransport(): ConversationTransport {
  return new GoConversationTransport();
}
