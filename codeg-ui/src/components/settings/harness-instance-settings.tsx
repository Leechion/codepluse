"use client"

/**
 * The harness section embedded on the Collaboration page.
 *
 * It used to be its own `/settings/harness-instances` page; both concepts are
 * about how agents are staffed, so they now live on one page in two cards:
 *
 *   * Main bot — the harness (built-in or instance) plus an optional model a
 *     new conversation lands on when nothing else decides it. This is the
 *     "main agent's model".
 *   * Harness instances — named copies of a built-in tool that pin their own
 *     provider and default model, so "Claude 2号" can sit on a DeepSeek
 *     gateway while "Claude 3号" uses the official one. Each instance is a
 *     delegation target with its own model, i.e. a per-sub-agent model.
 *
 * The component carries no page title or scroll area: CollaborationSettings
 * owns the single ScrollArea, so the two cards just stack in the page flow.
 * Backend commands and storage are unchanged.
 */

import { useCallback, useEffect, useMemo, useState } from "react"
import { Bot, Layers, Loader2, Plus, RefreshCw, Trash2 } from "lucide-react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"

import { AgentIcon } from "@/components/agent-icon"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Badge } from "@/components/ui/badge"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"
import { SettingsSection } from "@/components/shared/settings-section"
import {
  acpListAgents,
  createHarnessInstance,
  deleteHarnessInstance,
  listHarnessInstances,
  listHarnessProviders,
  updateHarnessInstance,
  type HarnessInstance,
  type HarnessProviderInfo,
} from "@/lib/api"
import { getAgentLabel } from "@/lib/custom-agents"
import { loadMainBot, saveMainBot } from "@/lib/main-bot"
import {
  isInstanceAgentType,
  type AgentType,
  type AcpAgentInfo,
} from "@/lib/types"

/** `Select` refuses an empty string value; "none" marks "no pin". */
const NO_PIN = "__none__"

function errorMessage(err: unknown): string {
  if (err instanceof Error && err.message) return err.message
  return String(err)
}

export function HarnessInstanceSection({
  mode = "all",
}: {
  /** Which card(s) to render: the combined section (default, for the standalone
   *  route), the main agent only, or the harness instances only. Lets the
   *  collaboration page split them across two tabs without duplicating logic. */
  mode?: "all" | "main" | "instances"
} = {}) {
  const t = useTranslations("HarnessInstanceSettings")
  const [instances, setInstances] = useState<HarnessInstance[]>([])
  const [providers, setProviders] = useState<HarnessProviderInfo[]>([])
  const [builtinTypes, setBuiltinTypes] = useState<AgentType[]>([])
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)

  const [editorOpen, setEditorOpen] = useState(false)
  const [editing, setEditing] = useState<HarnessInstance | null>(null)
  const [formName, setFormName] = useState("")
  const [formTool, setFormTool] = useState<"claude" | "codex">("claude")
  const [formProvider, setFormProvider] = useState<string>(NO_PIN)
  const [formModel, setFormModel] = useState("")

  const [deleteTarget, setDeleteTarget] = useState<HarnessInstance | null>(null)

  const [mainBotType, setMainBotType] = useState<string>(NO_PIN)
  const [mainBotModel, setMainBotModel] = useState("")
  const [savedMainBot, setSavedMainBot] = useState<string>(NO_PIN)
  const [savedMainBotModel, setSavedMainBotModel] = useState("")

  const load = useCallback(async () => {
    // The three lists are independent; a partial failure must not blank the
    // rest (an instance row with no provider list still renders).
    const [inst, prov, agents] = await Promise.allSettled([
      listHarnessInstances(),
      listHarnessProviders(),
      acpListAgents(),
    ])
    if (inst.status === "fulfilled") setInstances(inst.value)
    if (prov.status === "fulfilled") setProviders(prov.value.providers ?? [])
    if (agents.status === "fulfilled") {
      setBuiltinTypes(
        agents.value
          .filter((a: AcpAgentInfo) => !isInstanceAgentType(a.agent_type))
          .map((a: AcpAgentInfo) => a.agent_type)
      )
    }
    if (inst.status === "rejected") toast.error(t("loadFailed"))
    setLoading(false)
  }, [t])

  useEffect(() => {
    void load()
  }, [load])

  useEffect(() => {
    // The picker shows the persisted value even when the module cache loaded
    // it before this section mounted.
    void loadMainBot()
      .then((bot) => {
        const value = bot?.agentType ?? NO_PIN
        setMainBotType(value)
        setSavedMainBot(value)
        setMainBotModel(bot?.model ?? "")
        setSavedMainBotModel(bot?.model ?? "")
      })
      .catch(() => {})
  }, [])

  const providerName = useCallback(
    (id: string) => providers.find((p) => p.id === id)?.name ?? id,
    [providers]
  )

  const claudeProviders = useMemo(
    () => providers.filter((p) => p.agent === "claude"),
    [providers]
  )

  const openCreate = useCallback(() => {
    setEditing(null)
    setFormName("")
    setFormTool("claude")
    setFormProvider(NO_PIN)
    setFormModel("")
    setEditorOpen(true)
  }, [])

  const openEdit = useCallback((row: HarnessInstance) => {
    setEditing(row)
    setFormName(row.name)
    setFormTool((row.real_tool ?? "claude") as "claude" | "codex")
    setFormProvider(row.provider_id || NO_PIN)
    setFormModel(row.default_model ?? "")
    setEditorOpen(true)
  }, [])

  const submit = useCallback(async () => {
    if (busy) return
    const name = formName.trim()
    if (!name) {
      toast.error(t("nameRequired"))
      return
    }
    setBusy(true)
    try {
      if (editing) {
        // The pin is only sent when the user actually touched it: an instance
        // pinned to a provider that has since been deleted must stay editable
        // (rename, change model), and re-sending the stale id would make every
        // save fail backend validation. Clearing shows as an explicit "" so
        // the pin actually un-pins.
        const pinTouched = formProvider !== (editing.provider_id || NO_PIN)
        await updateHarnessInstance({
          agentType: editing.agent_type,
          name,
          providerId: pinTouched
            ? formProvider === NO_PIN
              ? ""
              : formProvider
            : undefined,
          defaultModel: formModel.trim(),
        })
        toast.success(t("updated"))
      } else {
        await createHarnessInstance({
          name,
          realTool: formTool,
          providerId:
            formTool === "claude" && formProvider !== NO_PIN
              ? formProvider
              : undefined,
          defaultModel: formModel.trim() || undefined,
        })
        toast.success(t("created"))
      }
      setEditorOpen(false)
      await load()
    } catch (err) {
      toast.error(errorMessage(err))
    } finally {
      setBusy(false)
    }
  }, [busy, editing, formModel, formName, formProvider, formTool, load, t])

  const confirmDelete = useCallback(async () => {
    if (!deleteTarget || busy) return
    setBusy(true)
    try {
      await deleteHarnessInstance(deleteTarget.agent_type)
      toast.success(t("deleted"))
      setDeleteTarget(null)
      await load()
    } catch (err) {
      toast.error(errorMessage(err))
    } finally {
      setBusy(false)
    }
  }, [busy, deleteTarget, load, t])

  const saveBot = useCallback(async () => {
    if (busy) return
    setBusy(true)
    try {
      const agentType =
        mainBotType === NO_PIN ? null : (mainBotType as AgentType)
      await saveMainBot(agentType, mainBotModel.trim())
      setSavedMainBot(mainBotType)
      setSavedMainBotModel(mainBotModel.trim())
      toast.success(t("mainBotSaved"))
    } catch (err) {
      toast.error(errorMessage(err))
    } finally {
      setBusy(false)
    }
  }, [busy, mainBotModel, mainBotType, t])

  const botOptions = useMemo(
    () => [...builtinTypes, ...instances.map((i) => i.agent_type)],
    [builtinTypes, instances]
  )

  const instanceRow = (row: HarnessInstance) => {
    const pinned = row.provider_id
      ? providerName(row.provider_id)
      : row.real_tool === "claude"
        ? t("followGlobal")
        : t("codexNative")
    return (
      <div
        key={row.agent_type}
        className="flex items-center justify-between gap-3 rounded-md border px-3 py-2.5"
      >
        <div className="min-w-0 flex-1 flex items-center gap-3">
          <AgentIcon agentType={row.agent_type} className="h-5 w-5 shrink-0" />
          <div className="min-w-0 space-y-0.5">
            <div className="text-sm font-medium truncate">{row.name}</div>
            <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
              <span>
                {getAgentLabel(
                  row.real_tool === "claude" ? "claude_code" : "codex"
                )}
              </span>
              <span aria-hidden>·</span>
              <span className="truncate">{pinned}</span>
              {row.default_model && (
                <>
                  <span aria-hidden>·</span>
                  <span className="truncate font-mono">
                    {row.default_model}
                  </span>
                </>
              )}
            </div>
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {!row.available && (
            <Badge variant="secondary" className="text-3xs px-1.5 py-0">
              {t("disabled")}
            </Badge>
          )}
          <Button
            size="sm"
            variant="ghost"
            className="h-7 px-2 text-xs"
            onClick={() => openEdit(row)}
          >
            {t("edit")}
          </Button>
          <Button
            size="icon"
            variant="ghost"
            className="h-7 w-7 text-destructive"
            onClick={() => setDeleteTarget(row)}
            aria-label={t("delete")}
          >
            <Trash2 className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>
    )
  }

  return (
    <div className="space-y-4">
      {/* Main agent: the harness + model a new conversation starts on. */}
      {mode !== "instances" && (
      <SettingsSection
        icon={Bot}
        title={t("mainBotTitle")}
        description={t("mainBotDescription")}
      >
        <div className="flex flex-wrap items-end gap-2">
          <div className="space-y-1.5">
            <Label className="text-xs text-muted-foreground">
              {t("mainBotHarness")}
            </Label>
            <Select
              value={mainBotType}
              onValueChange={setMainBotType}
              disabled={loading}
            >
              <SelectTrigger className="h-8 w-56 text-xs">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={NO_PIN}>{t("mainBotNone")}</SelectItem>
                {botOptions.map((at) => (
                  <SelectItem key={at} value={at}>
                    {getAgentLabel(at)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-1.5">
            <Label className="text-xs text-muted-foreground">
              {t("mainBotModel")}
            </Label>
            <Input
              className="h-8 w-56 text-xs"
              value={mainBotModel}
              onChange={(e) => setMainBotModel(e.target.value)}
              placeholder={t("mainBotModelPlaceholder")}
            />
          </div>
          <Button
            size="sm"
            className="h-8 text-xs"
            onClick={saveBot}
            disabled={
              busy ||
              (mainBotType === savedMainBot &&
                mainBotModel.trim() === savedMainBotModel)
            }
          >
            {t("save")}
          </Button>
        </div>
        {mainBotType === NO_PIN && (
          <p className="text-3xs text-muted-foreground">
            {t("mainBotInactiveHint")}
          </p>
        )}
      </SettingsSection>
      )}

      {/* Per-sub-agent execution ends: named, pinned instances. */}
      {mode !== "main" && (
      <>
      <SettingsSection
        icon={Layers}
        title={t("sectionTitle")}
        description={t("instancesDescription")}
      >
        <div className="flex items-center justify-between gap-2">
          <span className="text-xs text-muted-foreground">
            {t("instancesCount", { count: instances.length })}
          </span>
          <div className="flex gap-1">
            <Button
              size="sm"
              variant="ghost"
              className="h-8 text-xs"
              onClick={() => {
                setLoading(true)
                void load()
              }}
            >
              <RefreshCw className="h-3.5 w-3.5 mr-1" />
              {t("refresh")}
            </Button>
            <Button size="sm" className="h-8 text-xs" onClick={openCreate}>
              <Plus className="h-3.5 w-3.5 mr-1" />
              {t("newInstance")}
            </Button>
          </div>
        </div>

        {loading ? (
          <div className="flex items-center justify-center py-8">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        ) : instances.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
            <Layers className="h-8 w-8 mb-2 opacity-40" />
            <span className="text-xs">{t("noInstances")}</span>
            <span className="text-3xs mt-1 max-w-sm text-center">
              {t("noInstancesHint")}
            </span>
          </div>
        ) : (
          <div className="space-y-2">{instances.map(instanceRow)}</div>
        )}
      </SettingsSection>

      <Dialog open={editorOpen} onOpenChange={setEditorOpen}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>
              {editing ? t("editTitle") : t("createTitle")}
            </DialogTitle>
            <DialogDescription>
              {editing
                ? t("editDescription", { name: editing.name })
                : t("createDescription")}
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-3">
            <div className="space-y-1.5">
              <Label htmlFor="instance-name" className="text-xs">
                {t("nameLabel")}
              </Label>
              <Input
                id="instance-name"
                value={formName}
                onChange={(e) => setFormName(e.target.value)}
                placeholder={t("namePlaceholder")}
              />
            </div>

            {!editing && (
              <div className="space-y-1.5">
                <Label className="text-xs">{t("baseToolLabel")}</Label>
                <Select
                  value={formTool}
                  onValueChange={(v) => {
                    setFormTool(v as "claude" | "codex")
                    setFormProvider(NO_PIN)
                  }}
                >
                  <SelectTrigger className="h-8 text-xs">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="claude">Claude Code</SelectItem>
                    <SelectItem value="codex">Codex</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            )}

            {(editing
              ? editing.real_tool === "claude"
              : formTool === "claude") && (
              <div className="space-y-1.5">
                <Label className="text-xs">{t("providerLabel")}</Label>
                <Select value={formProvider} onValueChange={setFormProvider}>
                  <SelectTrigger className="h-8 text-xs">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={NO_PIN}>
                      {t("providerFollow")}
                    </SelectItem>
                    {/* A pin on a provider that has since been deleted stays
                        visible under its raw id, so editing is honest about
                        what the instance still holds. */}
                    {formProvider !== NO_PIN &&
                      !claudeProviders.some((p) => p.id === formProvider) && (
                        <SelectItem value={formProvider}>
                          {`${formProvider} (${t("providerGone")})`}
                        </SelectItem>
                      )}
                    {claudeProviders.map((p) => (
                      <SelectItem key={p.id} value={p.id}>
                        {p.name}
                        {p.ccCurrent ? " ★" : ""}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-3xs text-muted-foreground">
                  {t("providerHint")}
                </p>
              </div>
            )}

            <div className="space-y-1.5">
              <Label htmlFor="instance-model" className="text-xs">
                {t("modelLabel")}
              </Label>
              <Input
                id="instance-model"
                value={formModel}
                onChange={(e) => setFormModel(e.target.value)}
                placeholder={t("modelPlaceholder")}
              />
              <p className="text-3xs text-muted-foreground">{t("modelHint")}</p>
            </div>
          </div>

          <DialogFooter>
            <Button variant="ghost" onClick={() => setEditorOpen(false)}>
              {t("cancel")}
            </Button>
            <Button onClick={submit} disabled={busy}>
              {busy ? (
                <Loader2 className="h-3.5 w-3.5 mr-1 animate-spin" />
              ) : null}
              {editing ? t("save") : t("create")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <AlertDialog
        open={!!deleteTarget}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("deleteConfirmTitle")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("deleteConfirmMessage", {
                name: deleteTarget?.name ?? "",
              })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("cancel")}</AlertDialogCancel>
            <AlertDialogAction onClick={confirmDelete} disabled={busy}>
              {t("delete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      </>
      )}
    </div>
  )
}
