"use client"

import { useCallback, useEffect, useMemo, useState } from "react"
import { useTranslations } from "next-intl"
import { Bot, Layers, Plus, RefreshCw, Trash2 } from "lucide-react"
import { toast } from "sonner"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
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
  listHarnessProviderModels,
  getMainBotSettings,
  type HarnessInstance,
  type HarnessProviderInfo,
} from "@/lib/api"
import { getTransport } from "@/lib/transport"
import { saveMainBot } from "@/lib/main-bot"
import { toErrorMessage } from "@/lib/app-error"
import type { AgentType } from "@/lib/types"

type Tool = "claude" | "codex" | "dsh"
const TOOLS: Record<Tool, string> = {
  claude: "Claude Code",
  codex: "Codex",
  dsh: "DeepSeek Harness",
}
const WIRE: Record<Tool, AgentType> = {
  claude: "claude_code",
  codex: "codex",
  dsh: "deepseek",
}
const EMPTY = "__none__"
type Profile = { tool: Tool; provider: string; model: string }
/**
 * A harness that manages its own provider and model (anything the app cannot
 * pin: a custom ACP agent, a built-in outside the three rewriteable tools).
 * It cannot carry a model profile, but it can still be the main agent.
 */
type BareHarness = { agentType: AgentType; name: string }
function initial(): Profile {
  return { tool: "claude", provider: "", model: "" }
}

export function ProviderModelPicker({
  provider,
  model,
  onChange,
}: {
  provider: string
  model: string
  onChange: (model: string) => void
}) {
  const t = useTranslations("HarnessProfiles")
  const [catalog, setCatalog] = useState<{
    key: string
    models: { id: string; displayName?: string }[]
    warning?: string
  }>({ key: "", models: [] })
  const [retry, setRetry] = useState(0)
  const [manual, setManual] = useState(false)
  const key = provider + ":" + retry
  useEffect(() => {
    if (!provider) return
    let cancelled = false
    void listHarnessProviderModels(provider)
      .then((result) => {
        if (!cancelled)
          setCatalog({ key, models: result.models, warning: result.warning })
      })
      .catch((error) => {
        if (!cancelled)
          setCatalog({ key, models: [], warning: toErrorMessage(error) })
      })
    return () => {
      cancelled = true
    }
  }, [provider, key])
  const models = catalog.key === key ? catalog.models : []
  const choices =
    model && !models.some((m) => m.id === model)
      ? [{ id: model }, ...models]
      : models
  const loading = !!provider && catalog.key !== key
  return (
    <div className="space-y-1.5">
      <Label>{t("model")}</Label>
      <div className="flex items-center gap-1.5">
        {manual ? (
          <Input
            aria-label={t("model")}
            value={model}
            disabled={!provider}
            onChange={(e) => onChange(e.target.value)}
            placeholder={t("modelId")}
          />
        ) : (
          <Select
            value={model || EMPTY}
            onValueChange={(v) => {
              if (v && v !== EMPTY) onChange(v)
            }}
            disabled={!provider}
          >
            <SelectTrigger className="min-w-0 flex-1" aria-label={t("model")}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={EMPTY} disabled>
                {loading ? t("loadingModels") : t("chooseModel")}
              </SelectItem>
              {choices.map((m) => (
                <SelectItem key={m.id} value={m.id}>
                  {m.displayName || m.id}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          disabled={!provider || loading}
          aria-label={t("refreshModels")}
          onClick={() => setRetry((n) => n + 1)}
        >
          <RefreshCw
            className={loading ? "size-3.5 animate-spin" : "size-3.5"}
          />
        </Button>
      </div>
      <Button
        type="button"
        variant="link"
        className="h-auto p-0 text-2xs"
        onClick={() => setManual((v) => !v)}
      >
        {manual ? t("chooseModel") : t("modelId")}
      </Button>
      {catalog.key === key && catalog.warning && (
        <p className="text-2xs text-muted-foreground">{catalog.warning}</p>
      )}
    </div>
  )
}

function NewProvider({
  tool,
  onSaved,
}: {
  tool: Tool
  onSaved: () => Promise<void>
}) {
  const t = useTranslations("HarnessProfiles")
  const [form, setForm] = useState({ name: "", url: "", key: "", model: "" })
  const [busy, setBusy] = useState(false)
  return (
    <details className="rounded-lg border border-border/60 p-3">
      <summary className="cursor-pointer text-xs text-muted-foreground">
        {t("addProvider")}
      </summary>
      <div className="mt-3 grid gap-2">
        {(["name", "url", "key", "model"] as const).map((field) => (
          <div key={field} className="space-y-1">
            <Label>{t(field === "model" ? "defaultModel" : field)}</Label>
            <Input
              type={field === "key" ? "password" : "text"}
              autoComplete="off"
              aria-label={t(field === "model" ? "defaultModel" : field)}
              value={form[field]}
              onChange={(e) =>
                setForm((old) => ({ ...old, [field]: e.target.value }))
              }
            />
          </div>
        ))}
        <Button
          type="button"
          size="sm"
          disabled={
            busy ||
            !form.name.trim() ||
            !form.url.trim() ||
            !form.key.trim() ||
            !form.model.trim()
          }
          onClick={() => {
            setBusy(true)
            void getTransport()
              .call("codepluse_provider_save", { ...form, agent: tool })
              .then(async () => {
                setForm({ name: "", url: "", key: "", model: "" })
                await onSaved()
                toast.success(t("providerSaved"))
              })
              .catch((error) => toast.error(toErrorMessage(error)))
              .finally(() => setBusy(false))
          }}
        >
          {t("saveProvider")}
        </Button>
      </div>
    </details>
  )
}

function ProfileFields({
  value,
  onChange,
  providers,
  tools,
  hideHarness = false,
  onProvidersChanged,
}: {
  value: Profile
  onChange: (p: Profile) => void
  providers: HarnessProviderInfo[]
  tools: Tool[]
  /** The main-agent card lists every harness itself, so the pinned-tool
   *  subset does not repeat the choice. */
  hideHarness?: boolean
  onProvidersChanged: () => Promise<void>
}) {
  const t = useTranslations("HarnessProfiles")
  const available = providers.filter(
    (p) => p.agent === value.tool && p.hasKey && p.url && p.api !== false
  )
  const options = tools.includes(value.tool) ? tools : [value.tool, ...tools]
  return (
    <div className="space-y-3">
      <div className={hideHarness ? "grid gap-3" : "grid gap-3 sm:grid-cols-2"}>
        {!hideHarness && (
          <div className="space-y-1.5">
            <Label>{t("harness")}</Label>
            <Select
              value={value.tool}
              onValueChange={(v) => {
                if (v && v in TOOLS)
                  onChange({ tool: v as Tool, provider: "", model: "" })
              }}
            >
              <SelectTrigger aria-label={t("harness")}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {options.map((tool) => (
                  <SelectItem key={tool} value={tool}>
                    {TOOLS[tool]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        )}
        <div className="space-y-1.5">
          <div className="flex items-center justify-between gap-2">
            <Label>{t("provider")}</Label>
            <button
              type="button"
              className="text-muted-foreground hover:text-foreground"
              aria-label={`${t("refresh")} · ${t("provider")}`}
              onClick={() =>
                void onProvidersChanged().catch((error) =>
                  toast.error(toErrorMessage(error))
                )
              }
            >
              <RefreshCw className="size-3" />
            </button>
          </div>
          <Select
            value={value.provider || EMPTY}
            onValueChange={(v) => {
              const p = available.find((p) => p.id === v)
              onChange({
                ...value,
                provider: p?.id || "",
                model: p?.model || "",
              })
            }}
          >
            <SelectTrigger aria-label={t("provider")}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={EMPTY} disabled>
                {t("chooseProvider")}
              </SelectItem>
              {value.provider &&
                !available.some((p) => p.id === value.provider) && (
                  <SelectItem value={value.provider} disabled>
                    {t("missingProvider")}
                  </SelectItem>
                )}
              {available.map((p) => (
                <SelectItem key={p.id} value={p.id}>
                  {p.name}
                  {p.source ? ` · ${p.source}` : ""}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>
      <ProviderModelPicker
        provider={value.provider}
        model={value.model}
        onChange={(model) => onChange({ ...value, model })}
      />
      <p className="text-2xs text-muted-foreground">{t("apiOnly")}</p>
      <NewProvider
        key={value.tool}
        tool={value.tool}
        onSaved={onProvidersChanged}
      />
    </div>
  )
}

/**
 * The harness list the main-agent card offers: the tools whose provider can be
 * pinned, then every other harness the registry reports as runnable — newly
 * added custom agents included, so 协作 never lags behind Settings → Agents.
 */
function MainHarnessPicker({
  tools,
  bare,
  value,
  onChange,
}: {
  tools: Tool[]
  bare: BareHarness[]
  value: string
  onChange: (harness: string) => void
}) {
  const t = useTranslations("HarnessProfiles")
  // A selection whose harness went away (disabled, uninstalled) stays listed
  // by its wire, so opening the card never silently rewrites the saved main
  // agent to whatever happens to be first.
  const listed =
    tools.includes(value as Tool) || bare.some((b) => b.agentType === value)
  return (
    <div className="space-y-1.5">
      <Label>{t("harness")}</Label>
      <Select value={value} onValueChange={(v) => v && onChange(v)}>
        <SelectTrigger aria-label={t("harness")}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {tools.map((tool) => (
            <SelectItem key={tool} value={tool}>
              {TOOLS[tool]}
            </SelectItem>
          ))}
          {bare.map((row) => (
            <SelectItem key={row.agentType} value={row.agentType}>
              {row.name}
            </SelectItem>
          ))}
          {!listed && <SelectItem value={value}>{value}</SelectItem>}
        </SelectContent>
      </Select>
    </div>
  )
}

export function HarnessProfiles({
  mode = "all",
}: { mode?: "all" | "main" | "instances" } = {}) {
  const t = useTranslations("HarnessProfiles")
  const [providers, setProviders] = useState<HarnessProviderInfo[]>([])
  const [catalogWarning, setCatalogWarning] = useState("")
  const [instances, setInstances] = useState<HarnessInstance[]>([])
  const [tools, setTools] = useState<Tool[]>([])
  const [bare, setBare] = useState<BareHarness[]>([])
  const [main, setMain] = useState<Profile>(initial)
  /** Agent type when the main agent is a harness with no provider to pin. */
  const [mainBare, setMainBare] = useState<string | null>(null)
  const [savedMain, setSavedMain] = useState("")
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)
  const [editing, setEditing] = useState<HarnessInstance | null>(null)
  const [editorOpen, setEditorOpen] = useState(false)
  const [name, setName] = useState("")
  const [profile, setProfile] = useState<Profile>(initial)
  const [deleteTarget, setDeleteTarget] = useState<HarnessInstance | null>(null)
  const refreshProviders = useCallback(async () => {
    const result = await listHarnessProviders()
    setProviders(result.providers)
    setCatalogWarning(result.ccError || "")
  }, [])
  const refreshInstances = useCallback(
    async () => setInstances(await listHarnessInstances()),
    []
  )
  const load = useCallback(async () => {
    const [p, i, agents, bot] = await Promise.all([
      listHarnessProviders(),
      listHarnessInstances(),
      acpListAgents(),
      getMainBotSettings(),
    ])
    const live = agents.filter((a) => !a.instance && a.available && a.enabled)
    const toolOf = (a: (typeof agents)[number]) =>
      a.real_tool ||
      ({ claude_code: "claude", deepseek: "dsh" } as Record<string, string>)[
        a.agent_type
      ] ||
      a.agent_type
    const toolsLive = Array.from(new Set(live.map(toolOf))).filter(
      (tool): tool is Tool => tool in TOOLS
    )
    // Everything else the harness registry offers: a custom ACP agent
    // manages its own credentials, so no provider profile can be built for
    // it — but it is still a harness the collaboration side must see, and it
    // can run conversations as the main agent.
    const bareRows = live
      .filter((a) => !(toolOf(a) in TOOLS))
      .map((a) => ({ agentType: a.agent_type, name: a.name }))
    const configured = agents.find((a) => a.agent_type === bot.agent_type)
    const base = bot.base_agent_type || bot.agent_type
    const tool = (
      configured
        ? toolOf(configured)
        : (
            { claude_code: "claude", deepseek: "dsh" } as Record<string, string>
          )[base] || base
    ) as Tool
    const next = {
      tool: tool in TOOLS ? tool : toolsLive[0] || "claude",
      provider: bot.provider_id || configured?.provider_id || "",
      model: bot.model || configured?.default_model || "",
    }
    // A main bot that names one of those harnesses is stored bare: no
    // provider, no model — the backend keeps the harness wire itself, so
    // `agent_type` is what to restore.
    const bareTarget =
      tool in TOOLS || !bot.agent_type ? null : (bot.agent_type as AgentType)
    setProviders(p.providers)
    setCatalogWarning(p.ccError || "")
    setInstances(i)
    setTools(toolsLive)
    setBare(bareRows)
    setMain(next)
    setMainBare(bareTarget)
    setSavedMain(JSON.stringify(bareTarget ?? next))
  }, [])
  useEffect(() => {
    let cancelled = false
    void load()
      .then(() => undefined)
      .catch((error) => {
        if (!cancelled) toast.error(toErrorMessage(error))
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [load])
  const run = async (action: () => Promise<unknown>, done: () => void) => {
    setBusy(true)
    try {
      await action()
      done()
      toast.success(t("saved"))
    } catch (error) {
      toast.error(toErrorMessage(error))
    } finally {
      setBusy(false)
    }
  }
  const valid = (p: Profile) =>
    tools.includes(p.tool) &&
    !!p.model.trim() &&
    providers.some(
      (v) =>
        v.id === p.provider &&
        v.agent === p.tool &&
        v.hasKey &&
        v.url &&
        v.api !== false
    )
  const grouped = useMemo(
    () =>
      Object.keys(TOOLS).map((tool) => ({
        tool: tool as Tool,
        rows: instances.filter((i) => i.real_tool === tool),
      })),
    [instances]
  )
  // One signature for both shapes of the main choice: a pinned provider/model
  // profile, or the bare harness wire when there is nothing to pin.
  const mainValue = mainBare ?? main
  const mainSignature = JSON.stringify(mainValue)
  const mainIsValid = mainBare !== null || valid(main)
  return (
    <div className="space-y-4">
      {catalogWarning && (
        <p role="status" className="text-xs text-destructive">
          {catalogWarning}
        </p>
      )}
      {mode !== "instances" && (
        <SettingsSection
          icon={Bot}
          title={t("mainTitle")}
          description={t("mainDescription")}
        >
          <MainHarnessPicker
            tools={tools}
            bare={bare}
            value={mainBare ?? main.tool}
            onChange={(harness) => {
              if (harness in TOOLS) {
                setMainBare(null)
                setMain({ tool: harness as Tool, provider: "", model: "" })
              } else {
                setMainBare(harness)
              }
            }}
          />
          {mainBare === null ? (
            <ProfileFields
              value={main}
              onChange={setMain}
              providers={providers}
              tools={tools}
              hideHarness
              onProvidersChanged={refreshProviders}
            />
          ) : (
            // Not a missing form: this harness reads its own credentials and
            // picks its own model, so the only thing to save is "new
            // conversations start here".
            <p className="text-2xs text-muted-foreground">{t("bareHarness")}</p>
          )}
          <Button
            size="sm"
            disabled={
              loading || busy || !mainIsValid || mainSignature === savedMain
            }
            onClick={() =>
              void run(
                () =>
                  mainBare
                    ? saveMainBot(mainBare as AgentType, "", "")
                    : saveMainBot(
                        WIRE[main.tool],
                        main.model.trim(),
                        main.provider
                      ),
                () => setSavedMain(mainSignature)
              )
            }
          >
            {t("saveMain")}
          </Button>
        </SettingsSection>
      )}
      {mode !== "main" && (
        <SettingsSection
          icon={Layers}
          title={t("subTitle")}
          description={t("subDescription")}
        >
          <div className="flex justify-end gap-1">
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              onClick={() =>
                void load().catch((e) => toast.error(toErrorMessage(e)))
              }
            >
              <RefreshCw className="size-3.5" />
              {t("refresh")}
            </Button>
            <Button
              size="sm"
              disabled={loading || tools.length === 0}
              onClick={() => {
                setEditing(null)
                setName("")
                setProfile({
                  tool: tools[0] || "claude",
                  provider: "",
                  model: "",
                })
                setEditorOpen(true)
              }}
            >
              <Plus className="size-3.5" />
              {t("addProfile")}
            </Button>
          </div>
          {!loading && instances.length === 0 && (
            <p className="py-5 text-center text-xs text-muted-foreground">
              {t("empty")}
            </p>
          )}
          {/* Harnesses outside the three provider-pinnable tools are
              delegation targets already — the backend offers them as they are.
              They only have no provider/model combination to save here, and
              saying so keeps a newly added agent from looking skipped. */}
          {!loading && bare.length > 0 && (
            <p className="text-2xs text-muted-foreground">
              {t("bareDelegation", {
                harnesses: bare.map((row) => row.name).join("、"),
              })}
            </p>
          )}
          {grouped
            .filter((g) => g.rows.length > 0)
            .map((group) => (
              <div key={group.tool} className="space-y-2">
                <h3 className="text-xs font-medium">{TOOLS[group.tool]}</h3>
                {group.rows.map((row) => (
                  <div
                    key={row.agent_type}
                    className="flex items-center justify-between gap-3 rounded-lg border p-3"
                  >
                    <div className="min-w-0">
                      <div className="truncate text-sm font-medium">
                        {row.name}
                      </div>
                      <div className="truncate text-xs text-muted-foreground">
                        {providers.find((p) => p.id === row.provider_id)
                          ?.name || t("missingProvider")}{" "}
                        · {row.default_model || "—"}
                      </div>
                    </div>
                    <div className="flex shrink-0 gap-1">
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => {
                          setEditing(row)
                          setName(row.name)
                          setProfile({
                            tool: row.real_tool as Tool,
                            provider: row.provider_id || "",
                            model: row.default_model || "",
                          })
                          setEditorOpen(true)
                        }}
                      >
                        {t("edit")}
                      </Button>
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        aria-label={`${t("delete")} ${row.name}`}
                        onClick={() => setDeleteTarget(row)}
                      >
                        <Trash2 className="size-3.5" />
                      </Button>
                    </div>
                  </div>
                ))}
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
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>
              {editing ? t("editProfile") : t("addProfile")}
            </DialogTitle>
          </DialogHeader>
          <div className="space-y-3">
            <div className="space-y-1.5">
              <Label htmlFor="profile-name">{t("profileName")}</Label>
              <Input
                id="profile-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </div>
            <ProfileFields
              value={profile}
              onChange={setProfile}
              providers={providers}
              tools={editing ? [profile.tool] : tools}
              onProvidersChanged={refreshProviders}
            />
          </div>
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={busy}
              onClick={() => setEditorOpen(false)}
            >
              {t("cancel")}
            </Button>
            <Button
              disabled={busy || !name.trim() || !valid(profile)}
              onClick={() =>
                void run(
                  async () => {
                    if (editing) {
                      if (editing.real_tool !== profile.tool)
                        throw new Error(t("immutableHarness"))
                      await updateHarnessInstance({
                        agentType: editing.agent_type,
                        name: name.trim(),
                        providerId: profile.provider,
                        defaultModel: profile.model.trim(),
                      })
                    } else
                      await createHarnessInstance({
                        realTool: profile.tool,
                        name: name.trim(),
                        providerId: profile.provider,
                        defaultModel: profile.model.trim(),
                      })
                    await refreshInstances()
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
              onClick={(event) => {
                event.preventDefault()
                if (deleteTarget)
                  void run(
                    async () => {
                      await deleteHarnessInstance(deleteTarget.agent_type)
                      await refreshInstances()
                    },
                    () => setDeleteTarget(null)
                  )
              }}
            >
              {t("delete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
