"use client"

/**
 * ZCodeWorkPanel —— ZCode「工作处理界面」复刻。
 *
 * 结构照 ZCode TaskList / TaskListItem（前置状态槽 + 标题行 + 元信息行 + diff），
 * 详情区照 TodoPanel / workflow-timeline（阶段 pills + 步骤轨 + 产物卡片），
 * 视觉遵循 DESIGN.md 的 4px 间距、radius 层级与 Zai Light/Dark 色板。
 *
 * 数据面接 codepluse 的真实 Go 契约：work_task_list / work_task_events。
 * 只展示真实 WorkTask：后端无任务时显示空态，加载失败时显示错误与重试，
 * 不再用任何写死的示例数据（避免「一直在进行中」的假状态）。
 */
import { useCallback, useEffect, useMemo, useState } from "react"
import {
  Activity,
  AlertCircle,
  Check,
  Circle,
  CircleCheck,
  Clock3,
  FileCode2,
  Folder,
  GitBranch,
  ListTodo,
  PanelLeftClose,
  RefreshCw,
  Search,
  Settings,
  Terminal,
  Wrench,
  X,
} from "lucide-react"
import { openSettingsWindow, workTaskEvents, workTaskList } from "@/lib/api"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import type { WorkTask, WorkTaskEvent, WorkTaskStatus } from "@/lib/types"

type StepState = "done" | "active" | "pending" | "error"
type StepKind = "tool" | "file" | "terminal" | "branch" | "check" | "note" | "wrench"
type FilterKey = "all" | "active" | "done"

interface StepView {
  label: string
  detail?: string
  state: StepState
  time?: string
  kind: StepKind
}

interface WorkView {
  id: number
  title: string
  status: WorkTaskStatus | string
  agent: string | null
  workspace: string
  time: string
  additions: number | null
  deletions: number | null
  filesChanged: number | null
  progress: string | null
  result: string | null
  branch: string | null
  phase: number
  steps: StepView[]
}

const PHASES = ["准备", "执行", "校验", "交付"] as const

const ACTIVE_STATUSES = new Set(["todo", "queued", "preparing", "running", "awaiting_input", "review", "merging"])
const RUNNING_STATUSES = new Set(["queued", "preparing", "running", "awaiting_input", "merging"])
const DONE_STATUSES = new Set(["done"])

function stateOf(status: string): "loading" | "success" | "error" | "warning" | "idle" {
  if (status === "done") return "success"
  if (status === "failed" || status === "canceled") return "error"
  if (status === "awaiting_input" || status === "review") return "warning"
  if (RUNNING_STATUSES.has(status)) return "loading"
  return "idle"
}

function phaseOf(status: string): number {
  if (status === "done") return 3
  if (status === "review" || status === "merging") return 2
  if (status === "running") return 1
  if (status === "failed" || status === "canceled") return 1
  return 0
}

function statusLabel(status: string): string {
  const labels: Record<string, string> = {
    todo: "待办",
    queued: "排队中",
    preparing: "准备中",
    running: "执行中",
    awaiting_input: "等待输入",
    review: "待验收",
    merging: "合并中",
    done: "已完成",
    failed: "失败",
    canceled: "已取消",
  }
  return labels[status] ?? status
}

function formatTime(value?: string | null): string {
  if (!value) return "—"
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) return value
  const diff = Date.now() - parsed.getTime()
  if (diff >= 0 && diff < 60_000) return "刚刚"
  if (diff >= 0 && diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`
  return parsed.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false })
}

function defaultSteps(task: WorkTask): StepView[] {
  const state = stateOf(task.status)
  const steps: StepView[] = [
    { label: "任务创建", detail: task.title, state: "done", time: formatTime(task.created_at), kind: "note" },
    { label: "准备执行环境", detail: task.work_branch ?? task.base_branch ?? "工作区", state: state === "idle" ? "pending" : "done", kind: "branch" },
  ]
  if (task.latest_progress) {
    steps.push({ label: "执行中", detail: task.latest_progress, state: state === "loading" ? "active" : "done", kind: "wrench" })
  }
  if (task.result_summary) {
    steps.push({ label: "结果摘要", detail: task.result_summary, state: "done", kind: "check" })
  }
  if (state === "error") {
    steps.push({ label: "执行失败", detail: task.last_error ?? task.failure_reason ?? "请查看日志", state: "error", kind: "note" })
  } else if (state !== "idle" && state !== "loading") {
    steps.push({ label: "任务完成", detail: task.settled_at ? formatTime(task.settled_at) : undefined, state: "done", kind: "check" })
  }
  return steps
}

function fromApi(task: WorkTask, folderName: string | null): WorkView {
  return {
    id: task.id,
    title: task.title || "未命名任务",
    status: task.status,
    agent: task.agent_type ?? null,
    workspace: task.work_branch ?? task.base_branch ?? folderName ?? "工作区",
    time: formatTime(task.updated_at ?? task.created_at),
    additions: task.additions ?? null,
    deletions: task.deletions ?? null,
    filesChanged: task.files_changed ?? null,
    progress: task.latest_progress ?? null,
    result: task.result_summary ?? null,
    branch: task.work_branch ?? task.base_branch ?? null,
    phase: phaseOf(task.status),
    steps: defaultSteps(task),
  }
}

function eventLabel(event: WorkTaskEvent): string {
  const kind = (event.kind || "").trim()
  if (kind) return kind
  return event.actor ? `${event.actor} 事件` : "工作事件"
}

function eventDetail(event: WorkTaskEvent): string | undefined {
  const payload = event.payload
  if (!payload) return undefined
  for (const key of ["message", "detail", "text", "summary", "title"]) {
    const value = payload[key]
    if (typeof value === "string" && value.trim()) return value
  }
  return undefined
}

function eventsToSteps(events: WorkTaskEvent[], fallback: StepView[]): StepView[] {
  if (!events.length) return fallback
  return events.slice(-12).map((event, index) => ({
    label: eventLabel(event),
    detail: eventDetail(event),
    state: index === Math.min(events.length, 12) - 1 ? "active" : "done",
    time: formatTime(event.created_at),
    kind: (event.kind || "").includes("文件") ? "file" : (event.kind || "").includes("命令") ? "terminal" : "note",
  }))
}

function StepIcon({ kind }: { kind: StepKind }) {
  switch (kind) {
    case "file": return <FileCode2 size={9} strokeWidth={2} />
    case "terminal": return <Terminal size={9} strokeWidth={2} />
    case "branch": return <GitBranch size={9} strokeWidth={2} />
    case "wrench": return <Wrench size={9} strokeWidth={2} />
    case "check": return <Check size={9} strokeWidth={2.4} />
    default: return <Circle size={7} strokeWidth={2} />
  }
}

function StateDot({ state }: { state: ReturnType<typeof stateOf> }) {
  if (state === "loading") return <span className="zw-spinner" aria-label="执行中" />
  if (state === "success") return <CircleCheck size={14} style={{ color: "var(--zw-success)" }} aria-label="已完成" />
  if (state === "error") return <span className="zw-dot zw-dot--error" aria-label="失败" />
  if (state === "warning") return <AlertCircle size={13} style={{ color: "var(--zw-warning)" }} aria-label="待处理" />
  return <span className="zw-dot zw-dot--idle" aria-label="待办" />
}

export function ZCodeWorkPanel({ onCollapse }: { onCollapse?: () => void }) {
  const [items, setItems] = useState<WorkView[]>([])
  const [loadError, setLoadError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [reloadKey, setReloadKey] = useState(0)
  const [filter, setFilter] = useState<FilterKey>("all")
  const [query, setQuery] = useState("")
  const [selectedId, setSelectedId] = useState<number | null>(null)
  const [eventSteps, setEventSteps] = useState<Record<number, StepView[]>>({})
  // 工作区（文件夹）即任务所属的项目。左侧按工作区分组展示，切换后只列该工作区的任务。
  const folders = useAppWorkspaceStore((s) => s.folders)
  const [workspaceId, setWorkspaceId] = useState<number | null>(null)

  const folderNames = useMemo(() => {
    const map = new Map<number, string>()
    for (const f of folders) map.set(f.id, f.alias ?? f.name)
    return map
  }, [folders])

  // 工作树分支也归属到它所属的仓库，否则工作树任务会在列表里凭空多出一个工作区。
  const workspaceFolders = useMemo(
    () => folders.filter((f) => f.parent_id == null && f.kind === "regular"),
    [folders],
  )

  // 选中的工作区可能已被移除，回退到「全部」而不是留一个空列表。
  const activeWorkspaceId =
    workspaceId != null && !workspaceFolders.some((f) => f.id === workspaceId)
      ? null
      : workspaceId

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    void (async () => {
      try {
        const rows = await workTaskList(activeWorkspaceId)
        if (cancelled) return
        const mapped = Array.isArray(rows)
          ? rows.map((task) => fromApi(task, folderNames.get(task.folder_id) ?? null))
          : []
        setItems(mapped)
        setSelectedId((current) =>
          mapped.some((item) => item.id === current) ? current : (mapped[0]?.id ?? null),
        )
        setLoadError(null)
      } catch (error) {
        if (!cancelled) {
          setItems([])
          setSelectedId(null)
          setLoadError(
            error instanceof Error ? error.message : "任务列表加载失败",
          )
        }
      } finally {
        if (!cancelled) setLoading(false)
      }
    })()
    return () => { cancelled = true }
  }, [reloadKey, activeWorkspaceId, folderNames])

  const selected = useMemo(
    () => items.find((item) => item.id === selectedId) ?? items[0] ?? null,
    [items, selectedId],
  )

  useEffect(() => {
    if (!selected) return
    if (eventSteps[selected.id]) return
    let cancelled = false
    void workTaskEvents(selected.id)
      .then((events) => {
        if (!cancelled && Array.isArray(events)) {
          setEventSteps((current) => ({ ...current, [selected.id]: eventsToSteps(events, selected.steps) }))
        }
      })
      .catch(() => { /* 保留默认步骤 */ })
    return () => { cancelled = true }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected?.id])

  const refresh = useCallback(() => {
    setEventSteps({})
    setReloadKey((key) => key + 1)
  }, [])

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase()
    return items.filter((item) => {
      if (filter === "active" && !ACTIVE_STATUSES.has(item.status)) return false
      if (filter === "done" && !DONE_STATUSES.has(item.status)) return false
      if (needle && !`${item.title} ${item.agent ?? ""} ${item.workspace}`.toLowerCase().includes(needle)) return false
      return true
    })
  }, [items, filter, query])

  const groups = useMemo(() => [
    { key: "running", label: "进行中", rows: filtered.filter((item) => RUNNING_STATUSES.has(item.status)) },
    { key: "waiting", label: "等待 / 待办", rows: filtered.filter((item) => ["todo", "review", "awaiting_input"].includes(item.status) && !RUNNING_STATUSES.has(item.status)) },
    { key: "settled", label: "已结束", rows: filtered.filter((item) => ["done", "failed", "canceled"].includes(item.status)) },
  ], [filtered])

  const steps = selected ? eventSteps[selected.id] ?? selected.steps : []
  const activeCount = items.filter((item) => ACTIVE_STATUSES.has(item.status)).length

  return (
    <section className="zw-root zw-work" aria-label="工作处理">
      <div className="zw-work-header">
        <div className="zw-title">
          <span className="zw-title-icon"><Activity size={16} strokeWidth={2} /></span>
          <span>工作处理</span>
          <span className="zw-count">{activeCount}</span>
        </div>
        <div className="zw-header-actions">
          {loadError ? <span className="zw-load-flag">加载失败</span> : null}
          <button type="button" className="zw-icon-btn" onClick={refresh} disabled={loading} title="刷新任务" aria-label="刷新任务">
            <RefreshCw size={14} className={loading ? "zw-spinner" : ""} />
          </button>
          {onCollapse ? (
            <button type="button" className="zw-icon-btn" onClick={onCollapse} title="收起工作处理面板" aria-label="收起工作处理面板">
              <PanelLeftClose size={15} />
            </button>
          ) : null}
        </div>
      </div>

      <div className="zw-workspaces" aria-label="工作区">
        <div className="zw-section-label">
          <span>工作区</span>
        </div>
        <button
          type="button"
          className="zw-workspace-row"
          data-active={activeWorkspaceId === null}
          onClick={() => setWorkspaceId(null)}
        >
          <span className="zw-workspace-leading">
            <Activity size={13} strokeWidth={2} />
          </span>
          <span className="zw-workspace-name">全部工作区</span>
        </button>
        {workspaceFolders.map((folder) => (
          <button
            key={folder.id}
            type="button"
            className="zw-workspace-row"
            data-active={activeWorkspaceId === folder.id}
            onClick={() => setWorkspaceId(folder.id)}
          >
            <span className="zw-workspace-leading">
              <Folder size={13} strokeWidth={2} />
            </span>
            <span className="zw-workspace-name">{folder.alias ?? folder.name}</span>
          </button>
        ))}
      </div>

      <div className="zw-tabs" role="tablist" aria-label="任务筛选">
        {([
          ["all", "全部"],
          ["active", "进行中"],
          ["done", "已完成"],
        ] as const).map(([key, label]) => (
          <button
            key={key}
            type="button"
            role="tab"
            aria-selected={filter === key}
            data-active={filter === key}
            className="zw-tab"
            onClick={() => setFilter(key)}
          >
            {label}
          </button>
        ))}
      </div>

      <label className="zw-search">
        <Search size={13} />
        <input
          className="zw-search-input"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="搜索任务 / 工作区"
          aria-label="搜索任务"
        />
        {query ? (
          <button type="button" className="zw-icon-btn" style={{ width: 18, height: 18 }} onClick={() => setQuery("")} aria-label="清空搜索">
            <X size={11} />
          </button>
        ) : <span className="zw-kbd">⌘K</span>}
      </label>

      <div className="zw-scroll">
        {groups.map((group) => group.rows.length ? (
          <div className="zw-section" key={group.key}>
            <div className="zw-section-label">
              <span>{group.label}</span>
              <span>{group.rows.length}</span>
            </div>
            {group.rows.map((item) => {
              const state = stateOf(item.status)
              return (
                <button
                  key={item.id}
                  type="button"
                  className="zw-task-row"
                  data-active={selected?.id === item.id}
                  onClick={() => setSelectedId(item.id)}
                  title={item.title}
                >
                  <span className="zw-task-leading"><StateDot state={state} /></span>
                  <span className="zw-task-body">
                    <span className="zw-task-title">{item.title}</span>
                    <span className="zw-task-meta">
                      <span>{item.workspace}</span>
                      <span className="zw-meta-sep">·</span>
                      <span>{statusLabel(item.status)}</span>
                      <span className="zw-meta-sep">·</span>
                      <span>{item.time}</span>
                    </span>
                  </span>
                  <span className="zw-task-trailing">
                    {item.agent ? <span className="zw-badge zw-badge--agent">{item.agent}</span> : null}
                    {item.additions != null || item.deletions != null ? (
                      <span className="zw-diff">
                        {item.additions != null ? <span className="zw-diff-add">+{item.additions}</span> : null}
                        {item.deletions != null ? <span className="zw-diff-del">-{item.deletions}</span> : null}
                      </span>
                    ) : null}
                  </span>
                </button>
              )
            })}
          </div>
        ) : null)}
        {loadError ? (
          <div className="zw-empty">
            <AlertCircle size={22} />
            <span>任务列表加载失败</span>
            <span className="zw-empty-hint">{loadError}</span>
            <button type="button" className="zw-empty-retry" onClick={refresh}>
              重试
            </button>
          </div>
        ) : loading && items.length === 0 ? (
          <div className="zw-empty">
            <RefreshCw size={22} className="zw-model-spin" />
            <span>正在读取工作任务…</span>
          </div>
        ) : items.length === 0 ? (
          <div className="zw-empty">
            <ListTodo size={22} />
            <span>暂无工作任务</span>
            <span className="zw-empty-hint">
              在左侧「待办任务」中创建任务后，这里会显示执行进度与产物
            </span>
          </div>
        ) : filtered.length === 0 ? (
          <div className="zw-empty">
            <ListTodo size={22} />
            <span>没有匹配的工作任务</span>
          </div>
        ) : null}
      </div>

      {selected ? (
        <div className="zw-detail" aria-label="任务工作流">
          <div className="zw-detail-header">
            <span className="zw-detail-title">{selected.title}</span>
            <span className={`zw-badge zw-badge--${stateOf(selected.status) === "success" ? "success" : stateOf(selected.status) === "error" ? "error" : stateOf(selected.status) === "warning" ? "warning" : "running"}`}>
              {statusLabel(selected.status)}
            </span>
          </div>
          <div className="zw-detail-sub">
            {selected.progress ?? selected.result ?? (selected.branch ? `分支 ${selected.branch}` : "等待开始")}
          </div>

          <div className="zw-detail-scroll">
          <div className="zw-phases" aria-label="工作流阶段">
            {PHASES.map((phase, index) => (
              <span key={phase} style={{ display: "contents" }}>
                <span
                  className="zw-phase"
                  data-state={index < selected.phase ? "done" : index === selected.phase ? "active" : "pending"}
                >
                  {index < selected.phase ? <Check size={10} strokeWidth={2.5} /> : null}
                  {phase}
                </span>
                {index < PHASES.length - 1 ? <span className="zw-phase-line" /> : null}
              </span>
            ))}
          </div>

          <div className="zw-timeline">
            {steps.map((step, index) => (
              <div className="zw-tl-item" data-state={step.state} key={`${step.label}-${index}`}>
                <span className="zw-tl-rail">
                  <span className="zw-tl-node"><StepIcon kind={step.kind} /></span>
                </span>
                <div className="zw-tl-body">
                  <div className="zw-tl-label">
                    <span>{step.label}</span>
                    {step.time ? <span className="zw-tl-time">{step.time}</span> : null}
                  </div>
                  {step.detail ? <div className="zw-tl-detail">{step.detail}</div> : null}
                </div>
              </div>
            ))}
          </div>

          <div className="zw-artifact">
            <div className="zw-artifact-head"><GitBranch size={12} /> 工作产物</div>
            <div className="zw-artifact-grid">
              <div className="zw-metric">
                <div className="zw-metric-value">{selected.filesChanged ?? "—"}</div>
                <div className="zw-metric-label">变更文件</div>
              </div>
              <div className="zw-metric">
                <div className="zw-metric-value" style={{ color: "var(--zw-success)" }}>+{selected.additions ?? 0}</div>
                <div className="zw-metric-label">新增行</div>
              </div>
              <div className="zw-metric">
                <div className="zw-metric-value" style={{ color: "var(--zw-destructive)" }}>-{selected.deletions ?? 0}</div>
                <div className="zw-metric-label">删除行</div>
              </div>
            </div>
            <div className="zw-artifact-note">
              <Clock3 size={10} style={{ verticalAlign: -1, marginRight: 4 }} />
              {selected.agent ? `${selected.agent} · ` : ""}{selected.branch ?? selected.workspace} · 更新于 {selected.time}
            </div>
          </div>
          </div>
        </div>
      ) : null}

      {/* 设置入口：照上游 ZCode TaskList 的 footer 位置（列表底部整宽按钮）。 */}
      <div className="zw-footer">
        <button
          type="button"
          className="zw-settings-btn"
          onClick={() => {
            void openSettingsWindow("general").catch((error) => {
              console.error("[ZCodeWorkPanel] failed to open settings:", error)
            })
          }}
        >
          <Settings size={14} strokeWidth={2} />
          <span>设置</span>
        </button>
      </div>
    </section>
  )
}

export default ZCodeWorkPanel
