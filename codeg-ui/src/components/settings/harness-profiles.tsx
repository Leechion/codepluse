"use client"

import { useCallback, useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import {
  Bot,
  Layers,
  Pencil,
  Plus,
  RefreshCw,
  Trash2,
} from "lucide-react"
import { toast } from "sonner"
import { AgentIcon } from "@/components/agent-icon"
import { getAgentLabel } from "@/lib/custom-agents"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog"
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogCancel,
  AlertDialogAction,
} from "@/components/ui/alert-dialog"
import { SettingsSection } from "@/components/shared/settings-section"
import {
  acpListAgents,
  createHarnessInstance,
  updateHarnessInstance,
  deleteHarnessInstance,
  listHarnessInstances,
  listHarnessProviders,
  listHarnessConnections,
  getMainBotSettings,
  type HarnessConnectionSource,
  type HarnessInstance,
  type HarnessProviderInfo,
} from "@/lib/api"
import { saveMainBot } from "@/lib/main-bot"
import { toErrorMessage } from "@/lib/app-error"
import type { AcpAgentInfo } from "@/lib/types"
import {
  ConnectionFields,
  connectionValid,
  supportsAPI,
  agentTool,
  type ConnectionChoice,
} from "./connection-fields"
export { ProviderModelPicker } from "./provider-model-picker"

const emptyChoice: ConnectionChoice = {
  agentType: "",
  authMode: "api",
  providerId: "",
  model: "",
}
function sourceChoice(
  source: HarnessConnectionSource | undefined,
  agentType: string
): ConnectionChoice {
  const mode = source?.defaults.auth_mode
  return {
    agentType,
    authMode:
      mode === "subscription" ||
      (!mode &&
        (source?.account.configured || !supportsAPI(source?.tool || "")))
        ? "subscription"
        : "api",
    providerId: source?.defaults.provider_id || "",
    model: source?.defaults.model || "",
  }
}

export function HarnessProfiles({
  mode = "all",
}: { mode?: "all" | "main" | "instances" | "card" } = {}) {
  const t = useTranslations("HarnessProfiles")
  const c = useTranslations("Connections")
  const [providers, setProviders] = useState<HarnessProviderInfo[]>([])
  const [sources, setSources] = useState<HarnessConnectionSource[]>([])
  const [agents, setAgents] = useState<AcpAgentInfo[]>([])
  const [instances, setInstances] = useState<HarnessInstance[]>([])
  const [main, setMain] = useState<ConnectionChoice>(emptyChoice)
  const [savedMain, setSavedMain] = useState("")
  /** Draft of the main agent's card editor: the row shows the SAVED choice, the
   *  dialog edits this and only writes it back on save. */
  const [mainDraft, setMainDraft] = useState<ConnectionChoice>(emptyChoice)
  const [mainEditorOpen, setMainEditorOpen] = useState(false)
  const [profile, setProfile] = useState<ConnectionChoice>(emptyChoice)
  const [name, setName] = useState("")
  const [editing, setEditing] = useState<HarnessInstance | null>(null)
  const [editorOpen, setEditorOpen] = useState(false)
  const [deleteTarget, setDeleteTarget] = useState<HarnessInstance | null>(null)
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)
  const [warning, setWarning] = useState("")
  const refresh = useCallback(async () => {
    const [p, s, a, i] = await Promise.all([
      listHarnessProviders(),
      listHarnessConnections(),
      acpListAgents(),
      listHarnessInstances(),
    ])
    setProviders(p.providers)
    setSources(s)
    setAgents(a.filter((a) => !a.instance && a.available && a.enabled))
    setInstances(i)
    setWarning(p.ccError || "")
    return { sources: s, agents: a }
  }, [])
  useEffect(() => {
    let disposed = false
    void Promise.all([refresh(), getMainBotSettings()])
      .then(([data, bot]) => {
        if (disposed) return
        const configured = data.agents.find(
          (a) => a.agent_type === bot.agent_type
        )
        const base =
          bot.base_agent_type ||
          configured?.base_agent_type ||
          bot.agent_type ||
          data.agents.find((a) => !a.instance && a.available && a.enabled)
            ?.agent_type ||
          ""
        const next = sourceChoice(
          data.sources.find((s) => s.agent_type === base),
          base
        )
        if (bot.auth_mode) next.authMode = bot.auth_mode
        if (bot.provider_id) {
          next.providerId = bot.provider_id
          next.authMode = "api"
        }
        if (bot.agent_type) next.model = bot.model || ""
        if (next.authMode === "subscription") next.providerId = ""
        setMain(next)
        setSavedMain(bot.agent_type ? JSON.stringify(next) : "")
      })
      .catch((e) => {
        if (!disposed) toast.error(toErrorMessage(e))
      })
      .finally(() => {
        if (!disposed) setLoading(false)
      })
    return () => {
      disposed = true
    }
  }, [refresh])
  const sourceFor = (choice: ConnectionChoice) =>
    sources.find((s) => s.agent_type === choice.agentType)
  const nativeOnly = (choice: ConnectionChoice) => {
    const s = sourceFor(choice)
    return !!s && !supportsAPI(s.tool) && !s.account.supported
  }
  const valid = (choice: ConnectionChoice) =>
    nativeOnly(choice) || connectionValid(choice, sourceFor(choice), providers)
  const run = async (action: () => Promise<unknown>, done: () => void) => {
    setBusy(true)
    try {
      await action()
      done()
      toast.success(t("saved"))
    } catch (e) {
      toast.error(toErrorMessage(e))
    } finally {
      setBusy(false)
    }
  }
  const onRefresh = () =>
    void refresh().catch((e) => toast.error(toErrorMessage(e)))
  const saveMain = (choice: ConnectionChoice, done: () => void) =>
    void run(
      () =>
        nativeOnly(choice)
          ? saveMainBot(choice.agentType, "", "")
          : saveMainBot(
              choice.agentType,
              choice.model.trim(),
              choice.authMode === "api" ? choice.providerId : "",
              choice.authMode
            ),
      done
    )
  // The card's summary reads off the SAVED choice, so a cancelled edit leaves the
  // row exactly as it was.
  const mainProvider = providers.find((p) => p.id === main.providerId)
  const mainSummary = main.agentType
    ? [
        main.authMode === "subscription"
          ? c("subscription")
          : mainProvider?.name || c("nativeConnection"),
        main.model.trim() || c("accountDefault"),
      ]
        .filter(Boolean)
        .join(" · ")
    : ""
  const fields = (
    value: ConnectionChoice,
    onChange: (value: ConnectionChoice) => void
  ) => (
    <ConnectionFields
      value={value}
      onChange={onChange}
      agents={
        editing && editorOpen
          ? agents.filter((a) => a.agent_type === profile.agentType)
          : agents
      }
      providers={providers}
      sources={sources}
      onRefresh={onRefresh}
    />
  )
  return (
    <div className="space-y-4">
      {warning && (
        <p role="status" className="text-xs text-muted-foreground">
          {warning}
        </p>
      )}
      {mode === "card" && (
        <SettingsSection
          icon={Bot}
          title={t("mainTitle")}
          description={t("mainDescription")}
        >
          {/* 与「子智能体配置」同一套界面语言：一行（图标 + 名称 + 当前
              供应商与模型摘要 + 编辑），改动在弹层里完成。 */}
          <div className="divide-y divide-border/60 rounded-xl border border-border/70 bg-muted/40">
            <div className="flex items-center gap-3 p-3">
              {main.agentType ? (
                <AgentIcon
                  agentType={main.agentType}
                  className="size-6 shrink-0"
                />
              ) : (
                <Bot
                  className="size-6 shrink-0 text-muted-foreground"
                  aria-hidden="true"
                />
              )}
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-medium">
                  {/* 首次读取要等连接目录扫一遍本机（冷启动可达数秒），
                      这期间说「未配置」会误导成真的没配。 */}
                  {loading
                    ? c("loading")
                    : main.agentType
                      ? getAgentLabel(main.agentType)
                      : t("mainUnset")}
                </div>
                <p className="mt-1 truncate text-xs text-muted-foreground">
                  {loading
                    ? t("mainDescription")
                    : main.agentType
                      ? mainSummary
                      : t("mainDescription")}
                </p>
              </div>
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={t("edit")}
                disabled={loading}
                onClick={() => {
                  setMainDraft(main)
                  setMainEditorOpen(true)
                }}
              >
                <Pencil className="size-4" />
              </Button>
            </div>
          </div>
          <Dialog
            open={mainEditorOpen}
            onOpenChange={(open) => {
              if (!busy) setMainEditorOpen(open)
            }}
          >
            <DialogContent className="max-w-xl">
              <DialogHeader>
                <DialogTitle>{t("mainTitle")}</DialogTitle>
              </DialogHeader>
              {fields(mainDraft, setMainDraft)}
              <DialogFooter>
                <Button
                  variant="ghost"
                  onClick={() => setMainEditorOpen(false)}
                  disabled={busy}
                >
                  {t("cancel")}
                </Button>
                <Button
                  disabled={busy || !valid(mainDraft)}
                  onClick={() =>
                    saveMain(mainDraft, () => {
                      setMain(mainDraft)
                      setSavedMain(JSON.stringify(mainDraft))
                      setMainEditorOpen(false)
                    })
                  }
                >
                  {t("saveMain")}
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        </SettingsSection>
      )}
      {mode !== "instances" && mode !== "card" && (
        <SettingsSection
          icon={Bot}
          title={t("mainTitle")}
          description={t("mainDescription")}
        >
          {fields(main, setMain)}
          <Button
            size="sm"
            disabled={
              loading ||
              busy ||
              !valid(main) ||
              JSON.stringify(main) === savedMain
            }
            onClick={() =>
              void run(
                () =>
                  nativeOnly(main)
                    ? saveMainBot(main.agentType, "", "")
                    : saveMainBot(
                        main.agentType,
                        main.model.trim(),
                        main.authMode === "api" ? main.providerId : "",
                        main.authMode
                      ),
                () => setSavedMain(JSON.stringify(main))
              )
            }
          >
            {t("saveMain")}
          </Button>
        </SettingsSection>
      )}
      {mode !== "main" && mode !== "card" && (
        <SettingsSection
          icon={Layers}
          title={t("subTitle")}
          description={c("subDescription")}
        >
          <div className="flex justify-end gap-1">
            <Button
              variant="ghost"
              size="sm"
              onClick={onRefresh}
              disabled={busy}
            >
              <RefreshCw className="size-3.5" />
              {t("refresh")}
            </Button>
            <Button
              size="sm"
              disabled={loading || !agents.length}
              onClick={() => {
                setEditing(null)
                setName("")
                setProfile(
                  sourceChoice(
                    sources.find((s) => s.agent_type === agents[0].agent_type),
                    agents[0].agent_type
                  )
                )
                setEditorOpen(true)
              }}
            >
              <Plus className="size-3.5" />
              {t("addProfile")}
            </Button>
          </div>
          {!loading && !instances.length && (
            <p className="py-5 text-center text-xs text-muted-foreground">
              {t("empty")}
            </p>
          )}
          {instances.map((row) => (
            <div
              key={row.agent_type}
              className="flex items-center justify-between gap-3 rounded-xl border p-3"
            >
              <div className="min-w-0">
                <div className="truncate text-sm font-medium">{row.name}</div>
                <p className="mt-1 truncate text-xs text-muted-foreground">
                  {row.auth_mode === "subscription"
                    ? c("subscription")
                    : providers.find((p) => p.id === row.provider_id)?.name ||
                      c("nativeConnection")}{" "}
                  · {row.default_model || c("accountDefault")}
                </p>
              </div>
              <div className="flex shrink-0 items-center gap-1">
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => {
                    const base =
                      row.base_agent_type ||
                      agents.find((a) => agentTool(a) === row.real_tool)
                        ?.agent_type ||
                      ""
                    setEditing(row)
                    setName(row.name)
                    setProfile({
                      agentType: base,
                      authMode: row.auth_mode || "api",
                      providerId: row.provider_id || "",
                      model: row.default_model || "",
                    })
                    setEditorOpen(true)
                  }}
                >
                  {t("edit")}
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={t("deleteProfile")}
                  onClick={() => setDeleteTarget(row)}
                >
                  <Trash2 className="size-3.5" />
                </Button>
              </div>
            </div>
          ))}
        </SettingsSection>
      )}
      <Dialog
        open={editorOpen}
        onOpenChange={(open) => {
          if (!busy) setEditorOpen(open)
        }}
      >
        <DialogContent className="max-w-xl">
          <DialogHeader>
            <DialogTitle>
              {editing ? t("editProfile") : t("addProfile")}
            </DialogTitle>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="profile-name">{t("profileName")}</Label>
              <Input
                id="profile-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </div>
            {fields(profile, setProfile)}
          </div>
          <DialogFooter>
            <Button
              variant="ghost"
              onClick={() => setEditorOpen(false)}
              disabled={busy}
            >
              {t("cancel")}
            </Button>
            <Button
              disabled={busy || !name.trim() || !valid(profile)}
              onClick={() =>
                void run(
                  async () => {
                    const source = sourceFor(profile)
                    const params = {
                      name: name.trim(),
                      providerId:
                        profile.authMode === "api" ? profile.providerId : "",
                      defaultModel: profile.model.trim(),
                      authMode: profile.authMode,
                    }
                    if (editing)
                      await updateHarnessInstance({
                        ...params,
                        agentType: editing.agent_type,
                      })
                    else
                      await createHarnessInstance({
                        ...params,
                        realTool: source?.tool || profile.agentType,
                      })
                    await refresh()
                  },
                  () => setEditorOpen(false)
                )
              }
            >
              {t("save")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <AlertDialog
        open={!!deleteTarget}
        onOpenChange={(open) => {
          if (!busy && !open) setDeleteTarget(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("deleteProfile")}</AlertDialogTitle>
            <AlertDialogDescription>
              {deleteTarget?.name}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={busy}>{t("cancel")}</AlertDialogCancel>
            <AlertDialogAction
              disabled={busy}
              onClick={(e) => {
                e.preventDefault()
                if (deleteTarget)
                  void run(
                    async () => {
                      await deleteHarnessInstance(deleteTarget.agent_type)
                      await refresh()
                    },
                    () => setDeleteTarget(null)
                  )
              }}
            >
              {t("deleteProfile")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
