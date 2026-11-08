"use client";

/**
 * ZCodeChatPage —— 把 vendored ZCode v4 对话组件组装成 codeg-ui 的工作页面。
 *
 * 数据面：SessionDataLayer + GoConversationTransport（Go WS 契约 → v4 协议适配），
 * 视图面：ConversationTimeline + ConversationComposer + ConversationQueuePanel +
 * PermissionDialog（vendored 树）。
 * 会话列表来自 Go list_conversations；事件泵经 getTransport().subscribe("acp://event")
 * 桥接到 globalThis.__codepluseEventPump（goConversationTransport 读取）；
 * conversation://changed 驱动侧栏刷新；新建会话走 createSession draft 流，
 * conversation_linked 重 key 后由 transport.onSessionRekeyed 切换选中会话。
 */
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useTheme } from "next-themes";
import {
  DEFAULT_CODE_PREVIEW_SETTINGS,
  type CodePreviewSettings,
} from "@/zcode/lib/codePreviewSettings";
import { ZCodeIntlProvider } from "@/zcode/i18n/IntlProvider";
import type { Theme } from "@/zcode/useTheme";
import type {
  AttachmentPutFn,
} from "@/zcode/v4/composer/attachmentUpload";
import type {
  ComposerDraftContent,
  ComposerSubmissionDraftTransaction,
} from "@/zcode/v4/composer/useDraftConfigControl";
import {
  V4_DRAFT_SCOPE_ROOT,
  persistV4ComposerDraft,
  readV4ComposerDraft,
  type V4ComposerDraft,
} from "@/zcode/v4/composer/composerDraftStore";
import { ConversationComposer } from "@/zcode/v4/ConversationComposer";
import type {
  ConversationComposerSendOptions,
  ConversationComposerSendResult,
} from "@/zcode/v4/ConversationComposer";
import { ConversationTimeline } from "@/zcode/v4/ConversationTimeline";
import { ConversationQueuePanel } from "@/zcode/v4/ConversationQueuePanel";
import { ConversationDraftEmptyState } from "@/zcode/v4/ConversationDraftEmptyState";
import type { ConversationRowRenderContext } from "@/zcode/v4/conversationRowContext";
import { hasOlderRows } from "@/zcode/v4/conversationProjectionStore";
import {
  SessionDataLayer,
  type SessionLease,
} from "@/zcode/v4/sessionDataLayer";
import { useConversationProjection } from "@/zcode/v4/useConversationProjection";
import { createCommandEnvelope } from "@/zcode/v4/commandFactory";
import { pendingPermissionToLegacyRequest } from "@/zcode/v4/pendingInteractionAdapter";
import { PermissionDialog } from "@/zcode/PermissionDialog";
import { GoConversationTransport } from "@/zcode/goTransport/goConversationTransport";
import { WebProviderStubs } from "@/zcode/goTransport/webProviderStubs";
import {
  acpConnect,
  acpFindConnectionForConversation,
  listConversations,
} from "@/lib/api";
import type { AgentType, ConversationSummary } from "@/lib/types";
import { getTransport } from "@/lib/transport";

type PendingPermissionInteraction = NonNullable<
  ReturnType<typeof useConversationProjection>["snapshot"]
>["pendingInteractions"][number];

/** 把 Go 的 acp://event 事件流桥接给 goConversationTransport 的事件泵约定。 */
function installEventPump(): () => void {
  const globalWithPump = globalThis as {
    __codepluseEventPump?: (handler: (event: unknown) => void) => () => void;
  };
  globalWithPump.__codepluseEventPump = (handler) => {
    let unsubscribeApi: (() => void) | null = null;
    let cancelled = false;
    void getTransport()
      .subscribe("acp://event", (payload: unknown) => {
        handler(payload);
      })
      .then((off) => {
        if (cancelled) {
          off();
        } else {
          unsubscribeApi = off;
        }
      })
      .catch(() => {
        // transport 不可用时静默：发送路径会显式报错。
      });
    return () => {
      cancelled = true;
      unsubscribeApi?.();
    };
  };
  return () => {
    delete globalWithPump.__codepluseEventPump;
  };
}

function formatConversationTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleString(undefined, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function ZCodeChatPageInner() {
  const [conversations, setConversations] = useState<ConversationSummary[]>([]);
  const [listError, setListError] = useState<string | null>(null);
  /** 数字 = Go conversation id；draft 会话 = 适配器分配的字符串 id（createSession）。 */
  const [selectedSession, setSelectedSession] = useState<string | number | null>(null);
  const [creating, setCreating] = useState(false);
  const [connecting, setConnecting] = useState(false);

  // transport / dataLayer：页面级单例（生命周期 = 页面挂载）。
  const transportRef = useRef<GoConversationTransport | null>(null);
  if (transportRef.current === null) {
    transportRef.current = new GoConversationTransport();
  }
  const dataLayerRef = useRef<SessionDataLayer | null>(null);
  if (dataLayerRef.current === null) {
    dataLayerRef.current = new SessionDataLayer({ transport: transportRef.current });
  }
  const transport = transportRef.current;
  const dataLayer = dataLayerRef.current;

  // 事件泵：goConversationTransport.subscribe() 时读取该全局桥。
  useEffect(() => installEventPump(), []);

  // 会话列表。
  const refreshConversations = useCallback(() => {
    listConversations()
      .then((rows) => {
        setConversations(rows);
        setListError(null);
      })
      .catch((error) => {
        setListError(error instanceof Error ? error.message : String(error));
      });
  }, []);
  useEffect(() => {
    refreshConversations();
  }, [refreshConversations]);

  // conversation://changed：Go 侧 upsert/删除广播（含 draft 首条 prompt 建会话）→ 刷新侧栏。
  useEffect(() => {
    let cancelled = false;
    let off: (() => void) | null = null;
    void getTransport()
      .subscribe<unknown>("conversation://changed", () => {
        if (!cancelled) refreshConversations();
      })
      .then((unsubscribe) => {
        if (cancelled) unsubscribe();
        else off = unsubscribe;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      off?.();
    };
  }, [refreshConversations]);

  // draft → 真实会话重 key：切到 conversation_linked 返回的真实 id。
  useEffect(() => {
    return transport.onSessionRekeyed((draftId, realId) => {
      setSelectedSession((current) => (String(current) === draftId ? Number(realId) : current));
    });
  }, [transport]);

  // 当前会话摘要（workingDir / agentType 沿用 folder path 约定）。
  const selectedConversation = useMemo(
    () =>
      conversations.find(
        (row) => Number(row.id) === Number(selectedSession) && typeof selectedSession === "number",
      ) ?? null,
    [conversations, selectedSession],
  );
  const workspacePath = selectedConversation?.folder_path ?? "/";
  // 非纯数字的 string sessionId = 适配器合成的 draft 会话（createSession 分配）。
  const isDraftSession =
    typeof selectedSession === "string" && !/^\d+$/.test(selectedSession);

  // 打开真实会话前先确保 ACP 连接（与 conversation-detail-panel 同约定：
  // agentType 来自会话行，workingDir 沿用 folder path）。
  useEffect(() => {
    if (selectedSession == null || isDraftSession) return;
    const agentType = selectedConversation?.agent_type ?? "codex";
    const workingDir = selectedConversation?.folder_path ?? null;
    // 适配器的连接/新会话默认值跟随当前选中会话（draft 首发与队列提升共用）。
    transport.setSessionDefaults({ agentType: agentType as AgentType, workingDir });
    let cancelled = false;
    setConnecting(true);
    (async () => {
      try {
        const found = await acpFindConnectionForConversation(
          Number(selectedSession),
          undefined,
          agentType,
        );
        if (cancelled) return;
        if (found?.connection_id == null) {
          await acpConnect(agentType, workingDir ?? undefined);
        }
      } catch {
        // 连接失败不阻塞订阅：发送路径会再次尝试并显式报错。
      } finally {
        if (!cancelled) setConnecting(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [selectedSession, selectedConversation, isDraftSession, transport]);

  // 新建会话：createSession（sessionId=null）→ adapter 分配 draft 会话 → 切入 draft 视图。
  const handleCreateSession = useCallback(() => {
    if (creating) return;
    setCreating(true);
    (async () => {
      const envelope = createCommandEnvelope({
        type: "createSession",
        payload: { workspaceId: "codeg" },
        sessionId: null,
      });
      const ack = await transport.sendCommand(envelope);
      const sessionId =
        ack.status === "accepted" && ack.result?.type === "createSession"
          ? ack.result.sessionId
          : null;
      if (sessionId) setSelectedSession(sessionId);
    })()
      .catch(() => {
        // 新建失败静默；用户可重试按钮。
      })
      .finally(() => setCreating(false));
  }, [creating, transport]);

  // 会话切换 = 对新 sessionId 的投影订阅（SessionDataLayer topic store）。
  const sessionId = selectedSession != null ? String(selectedSession) : null;
  const [lease, setLease] = useState<SessionLease | null>(null);
  useEffect(() => {
    if (sessionId == null) {
      setLease(null);
      return;
    }
    const nextLease = dataLayer.acquire(sessionId);
    setLease(nextLease);
    return () => {
      nextLease.release();
    };
  }, [dataLayer, sessionId]);

  const state = useConversationProjection(lease);
  const snapshot = state.snapshot;

  // ── composer 薄适配：草稿面（ref 热路径 + scope 切换重置 + localStorage 持久化） ──
  const composerDraftScopeId = sessionId ?? V4_DRAFT_SCOPE_ROOT;
  const [composerDraft, setComposerDraft] = useState<V4ComposerDraft>(() => ({
    text: "",
    updatedAt: 0,
  }));
  const composerDraftRef = useRef(composerDraft);
  const composerDraftScopeToken = useMemo(
    () => Symbol(`zcode-chat-page-draft:${sessionId ?? "none"}`),
    [sessionId],
  );
  useEffect(() => {
    composerDraftRef.current = composerDraft;
  }, [composerDraft]);
  // 切会话（含 draft→真实会话 rekey）：渲染期同步重置草稿——composer 以 key=sessionId
  // 重挂载，必须带着新 scope 的草稿挂载，否则旧正文会残留一帧之后才被 effect 清掉。
  const prevDraftScopeRef = useRef(composerDraftScopeToken);
  if (prevDraftScopeRef.current !== composerDraftScopeToken) {
    prevDraftScopeRef.current = composerDraftScopeToken;
    const stored = readV4ComposerDraft(workspacePath, undefined, composerDraftScopeId);
    const next: V4ComposerDraft = {
      text: stored?.text ?? "",
      ...(stored?.editorStateJson ? { editorStateJson: stored.editorStateJson } : {}),
      ...(stored?.mention ? { mention: stored.mention } : {}),
      updatedAt: Date.now(),
    };
    composerDraftRef.current = next;
    setComposerDraft(next);
  }

  const persistDraftScope = useCallback(
    (content: ComposerDraftContent) => {
      try {
        persistV4ComposerDraft(workspacePath, undefined, composerDraftScopeId, {
          text: content.text ?? composerDraftRef.current.text,
          ...(content.editorStateJson !== undefined ? { editorStateJson: content.editorStateJson } : {}),
          ...(content.mention !== undefined ? { mention: content.mention } : {}),
        });
      } catch {
        // storage 不可用（隐私模式等）退化为内存草稿。
      }
    },
    [composerDraftScopeId, workspacePath],
  );

  const stageComposerContent = useCallback((content: ComposerDraftContent) => {
    // 输入热路径：只写 ref，不触发整页 re-render 或写盘。
    composerDraftRef.current = {
      ...composerDraftRef.current,
      ...content,
      updatedAt: Date.now(),
    };
  }, []);
  const commitComposerContent = useCallback(
    (content: ComposerDraftContent) => {
      const next: V4ComposerDraft = {
        ...composerDraftRef.current,
        ...content,
        updatedAt: Date.now(),
      };
      composerDraftRef.current = next;
      setComposerDraft(next);
      persistDraftScope(next);
    },
    [persistDraftScope],
  );
  const captureComposerContentLifecycleCommit = useCallback(() => {
    return (content: ComposerDraftContent) => commitComposerContent(content);
  }, [commitComposerContent]);
  const persistComposerContent = useCallback(commitComposerContent, [commitComposerContent]);
  const updateComposerContent = useCallback(commitComposerContent, [commitComposerContent]);
  const replaceComposerDraft = useCallback(
    (draft: Omit<V4ComposerDraft, "updatedAt">) => {
      const next: V4ComposerDraft = { ...draft, updatedAt: Date.now() };
      composerDraftRef.current = next;
      setComposerDraft(next);
      persistDraftScope(next);
    },
    [persistDraftScope],
  );
  const captureComposerSubmissionDraftTransaction = useCallback(
    (content: ComposerDraftContent): ComposerSubmissionDraftTransaction => {
      let claimed = false;
      let released = false;
      return {
        claim: () => {
          if (released || claimed) return false;
          claimed = true;
          return true;
        },
        restore: () => {
          if (!claimed || released) return { current: false, applied: false };
          claimed = false;
          commitComposerContent(content);
          return { current: true, applied: true };
        },
        finalize: () => {
          if (!claimed || released) return null;
          claimed = false;
          released = true;
          // 结算：只有草稿仍等于提交正文时才清空（这里没有并发编辑来源）。
          commitComposerContent({ text: "", editorStateJson: undefined, mention: undefined });
          return { current: true, applied: true };
        },
        release: () => {
          released = true;
        },
        matchesTarget: (scopeToken: symbol) => scopeToken === composerDraftScopeToken,
      };
    },
    [commitComposerContent, composerDraftScopeToken],
  );
  const attachmentPut = useCallback<AttachmentPutFn>(
    (params, options) => transport.attachmentPut(params, options),
    [transport],
  );

  // ── 发送 / 停止 / 模型切换 ──
  const dispatchCommand = useCallback(
    async (envelope: Parameters<typeof transport.sendCommand>[0]) => {
      const ack = await transport.sendCommand(envelope);
      return ack;
    },
    [transport],
  );

  const handleSendText = useCallback(
    async (
      text: string,
      options?: ConversationComposerSendOptions,
    ): Promise<ConversationComposerSendResult | void> => {
      if (sessionId == null) return "blocked";
      // held（choice）状态：无 disposition 的提交先要用户确认清空/保留队列。
      if (
        snapshot?.inputRouting.mode === "choice" &&
        !options?.heldQueueDisposition
      ) {
        return "confirmationRequired";
      }
      const envelope = createCommandEnvelope({
        type: "sendText",
        payload: {
          text,
          ...(options?.attachments?.length ? { attachments: options.attachments } : {}),
          ...(options?.requestedDelivery ? { requestedDelivery: options.requestedDelivery } : {}),
          ...(options?.heldQueueDisposition
            ? { heldQueueDisposition: options.heldQueueDisposition }
            : {}),
        },
        sessionId,
      });
      const ack = await dispatchCommand(envelope);
      return ack.status === "accepted" ? "sent" : "blocked";
    },
    [dispatchCommand, sessionId, snapshot],
  );

  const handleStop = useCallback(() => {
    if (sessionId == null) return;
    const envelope = createCommandEnvelope({
      type: "stop",
      payload: {},
      sessionId,
      baseRevision: snapshot?.revision ?? 0,
    });
    void dispatchCommand(envelope);
  }, [dispatchCommand, sessionId, snapshot]);

  const handleSelectModel = useCallback(
    (provider: string, model: string) => {
      if (sessionId == null) return;
      const envelope = createCommandEnvelope({
        type: "switchModelConfig",
        payload: { provider, model, thought: String(snapshot?.config.thought ?? "standard") },
        sessionId,
        baseRevision: snapshot?.revision ?? 0,
      });
      void dispatchCommand(envelope);
    },
    [dispatchCommand, sessionId, snapshot],
  );

  const handleSelectThought = useCallback(() => {
    // Go 后端暂无 thought 级别目录；保留空实现（vendor 工具栏可降级）。
  }, []);

  const handleSwitchMode = useCallback(
    (mode: string) => {
      if (sessionId == null) return;
      const envelope = createCommandEnvelope({
        type: "switchModelConfig",
        payload: { provider: "codepluse", model: String(snapshot?.config.model ?? ""), thought: String(snapshot?.config.thought ?? "standard") },
        sessionId,
        baseRevision: snapshot?.revision ?? 0,
      });
      (envelope.payload as { modeId?: string }).modeId = mode;
      void dispatchCommand(envelope);
    },
    [dispatchCommand, sessionId, snapshot],
  );

  // ── 队列命令（ConversationQueuePanel 回调 → CAS 命令） ──
  const dispatchQueueCommand = useCallback(
    async (
      type: "sendQueuedNow" | "editQueueItem" | "reorderQueueItem" | "deleteQueueItem" | "setAutoDrain",
      payload: Record<string, unknown>,
    ) => {
      if (sessionId == null) return;
      const envelope = createCommandEnvelope({
        type,
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        payload: payload as any,
        sessionId,
        baseRevision: snapshot?.revision ?? 0,
      });
      await dispatchCommand(envelope);
    },
    [dispatchCommand, sessionId, snapshot],
  );

  const handleQueueEditItem = useCallback(
    async (queueItemId: string) => {
      const item = snapshot?.queue.items.find((candidate) => candidate.queueItemId === queueItemId);
      await dispatchQueueCommand("deleteQueueItem", { queueItemId });
      if (item) {
        // 撤回：正文恢复到 composer 草稿（附件引用由 composer 侧上传流管理，无法原样回填）。
        replaceComposerDraft({ text: item.text });
      }
    },
    [dispatchQueueCommand, replaceComposerDraft, snapshot],
  );

  // ── 权限卡：pendingInteractions → PermissionDialog ──
  const pendingPermission = useMemo(
    () =>
      snapshot?.pendingInteractions.find(
        (interaction): interaction is PendingPermissionInteraction & {
          payload: Extract<
            PendingPermissionInteraction["payload"],
            { kind: "permission" }
          >;
        } => interaction.payload.kind === "permission",
      ) ?? null,
    [snapshot],
  );
  const [responding, setResponding] = useState(false);
  const [responseError, setResponseError] = useState<string | null>(null);
  const handlePermissionRespond = useCallback(
    async (requestId: string, optionId: string) => {
      if (sessionId == null) return;
      setResponding(true);
      setResponseError(null);
      try {
        const envelope = createCommandEnvelope({
          type: "resolveInteraction",
          payload: { interactionId: requestId, answer: { optionId } },
          sessionId,
        });
        // goConversationTransport 读 payload.optionId（schema 之外的对齐字段）。
        (envelope.payload as { optionId?: string }).optionId = optionId;
        const ack = await dispatchCommand(envelope);
        if (ack.status !== "accepted") {
          setResponseError(
            ack.status === "rejected" && "reason" in ack ? String(ack.reason) : "权限应答被拒绝",
          );
        }
      } catch (error) {
        setResponseError(error instanceof Error ? error.message : String(error));
      } finally {
        setResponding(false);
      }
    },
    [dispatchCommand, sessionId],
  );

  // ── timeline 行渲染上下文（theme 接 next-themes） ──
  const { resolvedTheme } = useTheme();
  const theme: Theme = resolvedTheme === "dark" ? "dark" : "light";
  const rowContext = useMemo<ConversationRowRenderContext>(
    () => ({
      workspacePath,
      theme,
      codePreviewSettings: DEFAULT_CODE_PREVIEW_SETTINGS as CodePreviewSettings,
      sessionId,
    }),
    [workspacePath, theme, sessionId],
  );

  const timelineRows = snapshot?.rows.window ?? [];
  const lastError = snapshot?.control.lastError ?? null;
  const queueState = snapshot?.queue ?? null;

  return (
    // 本页挂在 workspace 的 FolderWorkspaceShell 内，父容器已经扣掉底部 StatusBar。
    // 用 h-dvh 会按整个视口（含状态栏那 32px）撑高页面，把 composer 连同工具栏
    // 推到父容器之外，与状态栏重叠并被视口裁掉，只剩一条灰色输入行。
    <div className="flex h-full min-h-0 w-full overflow-hidden bg-background text-foreground">
      <aside className="flex w-72 shrink-0 flex-col border-r border-border">
        <div className="flex items-center justify-between px-4 py-3 text-sm font-semibold">
          <span>会话</span>
          <div className="flex items-center gap-1">
            <button
              type="button"
              className="rounded px-2 py-1 text-xs font-medium text-foreground hover:bg-accent disabled:opacity-50"
              onClick={handleCreateSession}
              disabled={creating}
            >
              {creating ? "创建中…" : "+ 新建会话"}
            </button>
            <button
              type="button"
              className="rounded px-2 py-1 text-xs text-muted-foreground hover:bg-accent"
              onClick={refreshConversations}
            >
              刷新
            </button>
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto">
          {listError != null ? (
            <p className="px-4 py-2 text-xs text-destructive">{listError}</p>
          ) : null}
          {conversations.length === 0 && listError == null ? (
            <p className="px-4 py-2 text-xs text-muted-foreground">暂无会话</p>
          ) : null}
          <ul className="space-y-0.5 px-2 pb-4">
            {isDraftSession ? (
              <li>
                <button
                  type="button"
                  className="w-full rounded border border-dashed border-border px-2 py-1.5 text-left text-sm"
                >
                  <span className="block truncate font-medium">新会话（未保存）</span>
                  <span className="block truncate text-xs text-muted-foreground">
                    发送第一条消息后落盘
                  </span>
                </button>
              </li>
            ) : null}
            {conversations.map((conversation) => {
              const id = Number(conversation.id);
              const active = id === Number(selectedSession) && !isDraftSession;
              return (
                <li key={String(conversation.id)}>
                  <button
                    type="button"
                    onClick={() => setSelectedSession(id)}
                    className={
                      "w-full rounded px-2 py-1.5 text-left text-sm hover:bg-accent " +
                      (active ? "bg-accent font-medium" : "")
                    }
                  >
                    <span className="block truncate">
                      {conversation.title || "未命名会话"}
                    </span>
                    <span className="block truncate text-xs text-muted-foreground">
                      {formatConversationTime(conversation.started_at)}
                      {conversation.agent_type ? ` · ${conversation.agent_type}` : ""}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
      </aside>

      <main className="flex min-w-0 flex-1 flex-col">
        <div className="flex items-center gap-2 border-b border-border px-4 py-2 text-sm">
          <span className="truncate font-medium">
            {isDraftSession
              ? "新会话（草稿）"
              : selectedConversation?.title ||
                (selectedSession != null ? `会话 ${selectedSession}` : "未选择会话")}
          </span>
          {connecting ? <span className="text-xs text-muted-foreground">连接中…</span> : null}
          {state.status === "error" && state.lastError ? (
            <span className="truncate text-xs text-destructive">{state.lastError}</span>
          ) : null}
        </div>

        {selectedSession == null ? (
          <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
            从左侧选择一个会话，或点击「新建会话」开始
          </div>
        ) : (
          <div className="relative flex min-h-0 flex-1 flex-col">
            <div className="min-h-0 flex-1">
              <ConversationTimeline
                rows={timelineRows}
                totalCount={snapshot?.rows.totalCount ?? 0}
                sessionKey={sessionId ?? "draft"}
                rowContext={rowContext}
                canLoadOlder={hasOlderRows(snapshot)}
                loadingOlder={state.loadingOlder}
                onLoadOlder={() => lease?.store.loadOlder()}
                emptyState={
                  isDraftSession ? (
                    <ConversationDraftEmptyState />
                  ) : (
                    <div className="p-6 text-center text-sm text-muted-foreground">
                      {connecting ? "正在连接…" : "暂无消息"}
                    </div>
                  )
                }
              />
            </div>
            <div className="shrink-0 border-t border-border">
              {queueState != null && sessionId != null ? (
                <ConversationQueuePanel
                  queue={queueState}
                  onSendNow={(queueItemId) => {
                    void dispatchQueueCommand("sendQueuedNow", { queueItemId });
                  }}
                  onEditItem={(queueItemId) => {
                    void handleQueueEditItem(queueItemId);
                  }}
                  onDeleteItem={(queueItemId) => {
                    void dispatchQueueCommand("deleteQueueItem", { queueItemId });
                  }}
                  onMoveItem={(queueItemId, beforeQueueItemId) => {
                    void dispatchQueueCommand("reorderQueueItem", {
                      queueItemId,
                      beforeQueueItemId,
                    });
                  }}
                  onResume={() => {
                    void dispatchQueueCommand("setAutoDrain", { autoDrain: true });
                  }}
                />
              ) : null}
              <ConversationComposer
                key={sessionId ?? "no-session"}
                snapshot={snapshot}
                sessionId={sessionId}
                composerDraft={composerDraft}
                composerDraftScopeToken={composerDraftScopeToken}
                stageComposerContent={stageComposerContent}
                captureComposerContentLifecycleCommit={captureComposerContentLifecycleCommit}
                captureComposerSubmissionDraftTransaction={
                  captureComposerSubmissionDraftTransaction
                }
                persistComposerContent={persistComposerContent}
                updateComposerContent={updateComposerContent}
                replaceComposerDraft={replaceComposerDraft}
                attachmentPut={attachmentPut}
                attachmentSessionId={sessionId}
                workspacePath={workspacePath}
                disabled={sessionId == null}
                error={lastError}
                onSendText={handleSendText}
                onStop={handleStop}
                onSelectModel={handleSelectModel}
                onSelectThought={handleSelectThought}
                onSwitchMode={handleSwitchMode}
              />
            </div>
            {pendingPermission != null ? (
              <div className="absolute inset-x-0 top-10 z-50 mx-auto max-w-xl px-4">
                <PermissionDialog
                  request={pendingPermissionToLegacyRequest(
                    sessionId ?? "",
                    pendingPermission as Parameters<typeof pendingPermissionToLegacyRequest>[1],
                  )}
                  responding={responding}
                  responseError={responseError ?? undefined}
                  workspacePath={workspacePath}
                  onRespond={(requestId, option) => {
                    void handlePermissionRespond(requestId, option.optionId);
                  }}
                />
              </div>
            ) : null}
          </div>
        )}
      </main>
    </div>
  );
}

export default function ZCodeChatPage({ children }: { children?: ReactNode }) {
  // ZCodeIntlProvider 为 vendor 组件提供 intl 文案上下文；
  // WebProviderStubs 补齐桌面宿主才有的 Platform/Services 上下文（web 安全桩）。
  return (
    <ZCodeIntlProvider initialLocale="zh-CN">
      <WebProviderStubs>
        <ZCodeChatPageInner />
        {children}
      </WebProviderStubs>
    </ZCodeIntlProvider>
  );
}
