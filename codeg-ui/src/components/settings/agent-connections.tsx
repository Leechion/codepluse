"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { useSearchParams } from "next/navigation"
import { useTranslations } from "next-intl"
import {
  Check,
  ChevronRight,
  KeyRound,
  Plus,
  RefreshCw,
  Pencil,
  Trash2,
  MoreHorizontal,
} from "lucide-react"
import { toast } from "sonner"
import { AgentIcon } from "@/components/agent-icon"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Switch } from "@/components/ui/switch"
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
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
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
import {
  acpListAgents,
  acpDeleteCustomAgent,
  listHarnessProviders,
  listHarnessConnections,
  listHarnessProviderModels,
  saveHarnessConnection,
  type HarnessConnectionSource,
  type HarnessProviderInfo,
  type HarnessAuthMode,
} from "@/lib/api"
import type { AcpAgentInfo } from "@/lib/types"
import { getTransport } from "@/lib/transport"
import { toErrorMessage } from "@/lib/app-error"
import { cn } from "@/lib/utils"
import { AddCustomAgentDialog } from "./add-custom-agent-dialog"
import {
  AccountConnection,
  ConnectionModeSelect,
  supportsAPI,
  sourceLabel,
} from "./connection-fields"

type Model = NonNullable<HarnessProviderInfo["models"]>[number]
const isHarnessProfile = (agent: AcpAgentInfo) =>
  agent.instance && agent.agent_type.startsWith("inst_")

type ProviderForm = {
  name: string
  url: string
  key: string
  protocol: string
  models: Model[]
  model: string
  enabled: boolean
}
const blankForm = (tool: string): ProviderForm => ({
  name: "",
  url: "",
  key: "",
  protocol:
    tool === "claude"
      ? "anthropic"
      : tool === "codex"
        ? "openai-responses"
        : "openai",
  models: [],
  model: "",
  enabled: true,
})
const providerForm = (
  provider: HarnessProviderInfo,
  tool: string
): ProviderForm => ({
  name: provider.name,
  url: provider.url,
  key: "",
  protocol:
    tool === "codex"
      ? "openai-responses"
      : provider.protocol || blankForm(tool).protocol,
  models: provider.models?.length
    ? provider.models.map((model) => ({ ...model }))
    : provider.model
      ? [{ id: provider.model }]
      : [],
  model: provider.model,
  enabled: provider.enabled !== false,
})

export function AgentConnections({
  initialAgentType,
}: {
  initialAgentType?: string | null
}) {
  const t = useTranslations("Connections")
  const params = useSearchParams()
  const requested = initialAgentType || params.get("agent")
  const [agents, setAgents] = useState<AcpAgentInfo[]>([])
  const [sources, setSources] = useState<HarnessConnectionSource[]>([])
  const [providers, setProviders] = useState<HarnessProviderInfo[]>([])
  const [selectedAgent, setSelectedAgent] = useState("")
  const [selectedProvider, setSelectedProvider] = useState("")
  const [authMode, setAuthMode] = useState<HarnessAuthMode>("api")
  const [officialModel, setOfficialModel] = useState("")
  const [form, setForm] = useState<ProviderForm>(blankForm(""))
  const [busy, setBusy] = useState(false)
  const [loading, setLoading] = useState(true)
  const [warning, setWarning] = useState("")
  const [addOpen, setAddOpen] = useState(false)
  const [editDefinition, setEditDefinition] = useState<string | undefined>()
  const [removeAgent, setRemoveAgent] = useState(false)
  const [modelOpen, setModelOpen] = useState(false)
  const [editIndex, setEditIndex] = useState<number | null>(null)
  const [modelDraft, setModelDraft] = useState({ id: "", name: "" })
  const [removeProvider, setRemoveProvider] = useState(false)
  const initializedAgent = useRef("")

  const load = useCallback(async () => {
    const [a, p, s] = await Promise.all([
      acpListAgents(),
      listHarnessProviders(),
      listHarnessConnections(),
    ])
    setAgents(a.filter((agent) => !isHarnessProfile(agent)))
    setProviders(p.providers)
    setSources(s)
    setWarning(p.ccError || "")
    setSelectedAgent((old) =>
      a.some((agent) => agent.agent_type === old && !isHarnessProfile(agent))
        ? old
        : (
            a.find(
              (agent) =>
                agent.agent_type === requested && !isHarnessProfile(agent)
            ) ||
            a.find(
              (candidate) =>
                candidate.agent_type ===
                  a.find((i) => i.agent_type === requested)?.base_agent_type &&
                !isHarnessProfile(candidate)
            ) ||
            a.find((agent) => !isHarnessProfile(agent))
          )?.agent_type || ""
    )
  }, [requested])
  useEffect(() => {
    void load()
      .catch((e) => toast.error(toErrorMessage(e)))
      .finally(() => setLoading(false))
  }, [load])
  const agent = agents.find((a) => a.agent_type === selectedAgent)
  const source = sources.find((s) => s.agent_type === selectedAgent)
  const tool = source?.tool || ""
  const rows = providers.filter(
    (p) => p.agent === tool && (p.api !== false || (!p.hasKey && !!p.url))
  )
  const selected = rows.find((p) => p.id === selectedProvider)
  const dirty =
    !!selected &&
    JSON.stringify(form) !== JSON.stringify(providerForm(selected, tool))
  const imported =
    !!selected &&
    (selected.id.startsWith("native:") ||
      selected.id.startsWith("ccswitch:") ||
      selected.id.startsWith("store:"))
  useEffect(() => {
    if (!source || initializedAgent.current === selectedAgent) return
    initializedAgent.current = selectedAgent
    const mode = source.defaults.auth_mode
    setAuthMode(
      mode === "subscription" ||
        (!mode && (source.account.configured || !supportsAPI(source.tool)))
        ? "subscription"
        : "api"
    )
    setOfficialModel(mode === "subscription" ? source.defaults.model : "")
    setSelectedProvider(
      source.defaults.provider_id ||
        providers.find((p) => p.agent === source.tool)?.id ||
        ""
    )
  }, [selectedAgent, source, providers])
  useEffect(() => {
    if (selected) {
      setForm(providerForm(selected, tool))
    } else setForm(blankForm(tool))
  }, [selected, tool])

  const run = async (action: () => Promise<unknown>, message = t("saved")) => {
    setBusy(true)
    try {
      await action()
      toast.success(message)
    } catch (e) {
      toast.error(toErrorMessage(e))
    } finally {
      setBusy(false)
    }
  }
  const refresh = () => void load().catch((e) => toast.error(toErrorMessage(e)))
  const activeModels = form.models.filter((m) => m.enabled !== false)
  const saveProvider = () =>
    run(async () => {
      const saved = await getTransport().call<HarnessProviderInfo>(
        "codepluse_provider_save",
        {
          ...form,
          id: selected?.id || "",
          agent: tool,
          model: form.model || activeModels[0]?.id || form.models[0]?.id,
        }
      )
      await load()
      setSelectedProvider(saved.id)
    })
  const activateProvider = () =>
    run(async () => {
      if (!selected) return
      await saveHarnessConnection({
        agentType: selectedAgent,
        authMode: "api",
        providerId: selected.id,
        model: form.model || activeModels[0]?.id,
      })
      await load()
    })
  const setModelEnabled = (index: number, enabled: boolean) =>
    setForm((old) => {
      const models = old.models.map((m, i) =>
        i === index ? { ...m, enabled } : m
      )
      return {
        ...old,
        models,
        model:
          models.find((m) => m.id === old.model && m.enabled !== false)?.id ||
          models.find((m) => m.enabled !== false)?.id ||
          old.model,
      }
    })
  const providerUsed =
    source?.defaults.auth_mode === "api" &&
    source.defaults.provider_id === selected?.id &&
    source.defaults.model === form.model
  const customID = agent?.agent_type.startsWith("custom:")
    ? agent.agent_type.slice(7)
    : undefined

  return (
    <div className="cp-agent-connections space-y-6">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">{t("title")}</h1>
          <p className="mt-1.5 text-sm text-muted-foreground">
            {t("description")}
          </p>
        </div>
        <div className="flex items-center gap-1">
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={refresh}
            aria-label={t("refreshProviders")}
          >
            <RefreshCw className="size-4" />
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() => {
              setEditDefinition(undefined)
              setAddOpen(true)
            }}
          >
            <Plus className="size-3.5" />
            {t("addAgent")}
          </Button>
          {customID && (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={t("manageAgent")}
                >
                  <MoreHorizontal className="size-4" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem
                  onSelect={() => {
                    setEditDefinition(customID)
                    setAddOpen(true)
                  }}
                >
                  {t("editAgent")}
                </DropdownMenuItem>
                <DropdownMenuItem
                  variant="destructive"
                  onSelect={() => setRemoveAgent(true)}
                >
                  {t("removeAgent")}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </div>
      </header>
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="space-y-1.5">
          <Label>{t("harness")}</Label>
          <Select
            value={selectedAgent}
            onValueChange={(v) => v && setSelectedAgent(v)}
          >
            <SelectTrigger className="w-full" aria-label={t("harness")}>
              <SelectValue
                placeholder={loading ? t("loading") : t("chooseHarness")}
              />
            </SelectTrigger>
            <SelectContent>
              {agents.map((a) => (
                <SelectItem value={a.agent_type} key={a.agent_type}>
                  <AgentIcon agentType={a.agent_type} className="size-4" />
                  {a.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <ConnectionModeSelect
          value={authMode}
          onChange={setAuthMode}
          source={source}
        />
      </div>
      {warning && (
        <p role="status" className="text-xs text-muted-foreground">
          {warning}
        </p>
      )}
      {authMode === "subscription" ? (
        <section className="max-w-2xl space-y-5">
          <AccountConnection
            source={source}
            model={officialModel}
            onModelChange={setOfficialModel}
            onRefresh={refresh}
          />
          <Button
            size="sm"
            disabled={busy || !source?.account.supported}
            onClick={() =>
              void run(async () => {
                await saveHarnessConnection({
                  agentType: selectedAgent,
                  authMode: "subscription",
                  model: officialModel,
                })
                await load()
              })
            }
          >
            {t("useAccount")}
          </Button>
        </section>
      ) : (
        <div className="cp-provider-workbench grid min-h-[420px] overflow-hidden rounded-2xl border md:grid-cols-[210px_minmax(0,1fr)]">
          <aside className="cp-provider-sidebar border-b p-3 md:border-r md:border-b-0">
            <div className="mb-3 flex items-center justify-between px-2">
              <p className="text-xs text-muted-foreground">{t("providers")}</p>
              <Button
                size="icon-sm"
                variant="ghost"
                aria-label={t("addProvider")}
                disabled={!supportsAPI(tool)}
                onClick={() => setSelectedProvider("__new__")}
              >
                <Plus className="size-3.5" />
              </Button>
            </div>
            <div className="space-y-1">
              {rows.map((p) => (
                <button
                  type="button"
                  key={p.id}
                  onClick={() => setSelectedProvider(p.id)}
                  aria-current={selectedProvider === p.id ? "true" : undefined}
                  className={cn(
                    "cp-provider-row flex w-full items-center gap-2 rounded-lg px-2.5 py-2.5 text-left text-sm transition-colors hover:bg-accent/50",
                    selectedProvider === p.id &&
                      "bg-accent/60 ring-1 ring-inset ring-border"
                  )}
                >
                  <KeyRound className="size-3.5 shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate">{p.name}</span>
                  <span
                    className={cn(
                      "size-1.5 shrink-0 rounded-full",
                      p.hasKey && p.enabled !== false
                        ? "bg-emerald-500"
                        : "bg-amber-500"
                    )}
                  />
                </button>
              ))}
              {!rows.length && (
                <p className="px-2 py-5 text-xs leading-relaxed text-muted-foreground">
                  {t("noProviders")}
                </p>
              )}
            </div>
          </aside>
          <section className="min-w-0 space-y-5 p-5 sm:p-6">
            {selected || selectedProvider === "__new__" ? (
              <>
                <div className="flex items-center gap-3">
                  <div className="flex size-8 items-center justify-center rounded-lg bg-primary/5">
                    <KeyRound className="size-4" />
                  </div>
                  <div className="min-w-0 flex-1">
                    <h2 className="truncate text-base font-medium">
                      {selected?.name || t("newProvider")}
                    </h2>
                    <p className="mt-0.5 text-2xs text-muted-foreground">
                      {imported
                        ? t("importedFrom", {
                            name: sourceLabel(tool) || selected?.source || "",
                          })
                        : t("localConfiguration")}
                    </p>
                  </div>
                  <Switch
                    checked={form.enabled}
                    onCheckedChange={(enabled) =>
                      setForm((old) => ({ ...old, enabled }))
                    }
                    aria-label={t("enableProvider")}
                  />
                  {selected && !imported && (
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("deleteProvider")}
                      onClick={() => setRemoveProvider(true)}
                    >
                      <Trash2 className="size-3.5" />
                    </Button>
                  )}
                </div>
                <div className="grid gap-4">
                  <div className="space-y-1.5">
                    <Label htmlFor="provider-name">{t("name")}</Label>
                    <Input
                      id="provider-name"
                      value={form.name}
                      onChange={(e) =>
                        setForm((old) => ({ ...old, name: e.target.value }))
                      }
                    />
                  </div>
                  <div className="space-y-1.5">
                    <Label htmlFor="provider-base-url">Base URL</Label>
                    <Input
                      id="provider-base-url"
                      value={form.url}
                      placeholder="https://"
                      spellCheck={false}
                      onChange={(e) =>
                        setForm((old) => ({ ...old, url: e.target.value }))
                      }
                    />
                  </div>
                  <div className="space-y-1.5">
                    <Label>{t("apiFormat")}</Label>
                    <Select
                      value={form.protocol}
                      onValueChange={(v) =>
                        v && setForm((old) => ({ ...old, protocol: v }))
                      }
                    >
                      <SelectTrigger
                        className="w-full"
                        aria-label={t("apiFormat")}
                      >
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {tool === "claude" ? (
                          <SelectItem value="anthropic">
                            Anthropic Messages (/v1/messages)
                          </SelectItem>
                        ) : tool === "codex" ? (
                          <SelectItem value="openai-responses">
                            OpenAI Responses (/responses)
                          </SelectItem>
                        ) : (
                          <>
                            <SelectItem value="openai">
                              OpenAI Chat Completions (/chat/completions)
                            </SelectItem>
                            <SelectItem value="openai-responses">
                              OpenAI Responses (/responses)
                            </SelectItem>
                            <SelectItem value="anthropic">
                              Anthropic Messages (/v1/messages)
                            </SelectItem>
                          </>
                        )}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-1.5">
                    <Label htmlFor="provider-api-key">API Key</Label>
                    <Input
                      id="provider-api-key"
                      type="password"
                      autoComplete="new-password"
                      value={form.key}
                      placeholder={
                        selected?.hasKey ? t("keyStored") : t("enterKey")
                      }
                      onChange={(e) =>
                        setForm((old) => ({ ...old, key: e.target.value }))
                      }
                    />
                  </div>
                </div>
                <div className="space-y-2">
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <Label>{t("modelList")}</Label>
                    <div className="flex gap-1">
                      {selected && (
                        <Button
                          variant="ghost"
                          size="sm"
                          disabled={busy || dirty || !selected.hasKey}
                          onClick={() =>
                            void run(async () => {
                              const result = await listHarnessProviderModels(
                                selected.id
                              )
                              setForm((old) => ({
                                ...old,
                                models: [
                                  ...old.models,
                                  ...result.models
                                    .filter(
                                      (m) =>
                                        !old.models.some(
                                          (existing) => existing.id === m.id
                                        )
                                    )
                                    .map(
                                      (m) =>
                                        old.models.find(
                                          (existing) => existing.id === m.id
                                        ) || {
                                          id: m.id,
                                          name: m.displayName,
                                          enabled: true,
                                        }
                                    ),
                                ],
                              }))
                              if (result.warning) toast.message(result.warning)
                            }, t("modelsLoaded"))
                          }
                        >
                          <RefreshCw className="size-3" />
                          {t("syncModels")}
                        </Button>
                      )}
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => {
                          setEditIndex(null)
                          setModelDraft({ id: "", name: "" })
                          setModelOpen(true)
                        }}
                      >
                        <Plus className="size-3" />
                        {t("addModel")}
                      </Button>
                    </div>
                  </div>
                  <div className="max-h-[360px] overflow-y-auto rounded-xl border">
                    {!form.models.length && (
                      <p className="p-5 text-center text-xs text-muted-foreground">
                        {t("noModels")}
                      </p>
                    )}
                    {form.models.map((m, index) => (
                      <div
                        key={`${m.id}:${index}`}
                        className={cn(
                          "flex items-center gap-2 border-b px-3 py-3 last:border-b-0",
                          m.enabled === false && "opacity-45"
                        )}
                      >
                        <div className="min-w-0 flex-1">
                          <p className="truncate font-mono text-xs">{m.id}</p>
                          {m.name && m.name !== m.id && (
                            <p className="mt-0.5 truncate text-2xs text-muted-foreground">
                              {m.name}
                            </p>
                          )}
                        </div>
                        {form.model === m.id && (
                          <Check className="size-3.5 text-primary" />
                        )}
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={`${t("editModel")} ${m.id}`}
                          onClick={() => {
                            setEditIndex(index)
                            setModelDraft({ id: m.id, name: m.name || "" })
                            setModelOpen(true)
                          }}
                        >
                          <Pencil className="size-3.5" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={`${t("deleteModel")} ${m.id}`}
                          onClick={() =>
                            setForm((old) => {
                              const models = old.models.filter(
                                (_, i) => i !== index
                              )
                              return {
                                ...old,
                                models,
                                model:
                                  old.model === m.id
                                    ? models.find((m) => m.enabled !== false)
                                        ?.id || ""
                                    : old.model,
                              }
                            })
                          }
                        >
                          <Trash2 className="size-3.5" />
                        </Button>
                        <Switch
                          checked={m.enabled !== false}
                          onCheckedChange={(enabled) =>
                            setModelEnabled(index, enabled)
                          }
                          aria-label={`${t("enableModel")} ${m.id}`}
                        />
                      </div>
                    ))}
                  </div>
                </div>
                {!!activeModels.length && (
                  <div className="space-y-1.5">
                    <Label>{t("defaultModel")}</Label>
                    <Select
                      value={form.model || activeModels[0].id}
                      onValueChange={(v) =>
                        v && setForm((old) => ({ ...old, model: v }))
                      }
                    >
                      <SelectTrigger
                        className="w-full"
                        aria-label={t("defaultModel")}
                      >
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {activeModels.map((m) => (
                          <SelectItem value={m.id} key={m.id}>
                            {m.name || m.id}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                )}
                <div className="flex flex-wrap gap-2">
                  <Button
                    size="sm"
                    disabled={
                      busy ||
                      (!!selected && !dirty) ||
                      !form.name.trim() ||
                      !form.url.trim() ||
                      (!form.key && !selected?.hasKey) ||
                      !form.models.length
                    }
                    onClick={() => void saveProvider()}
                  >
                    {selected ? t("saveChanges") : t("saveProvider")}
                  </Button>
                  {selected && !dirty && (
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={
                        busy ||
                        providerUsed ||
                        !form.enabled ||
                        !activeModels.length ||
                        !selected.hasKey
                      }
                      onClick={() => void activateProvider()}
                    >
                      {providerUsed ? (
                        <>
                          <Check className="size-3.5" />
                          {t("inUse")}
                        </>
                      ) : (
                        <>
                          {t("useProvider")}
                          <ChevronRight className="size-3.5" />
                        </>
                      )}
                    </Button>
                  )}
                </div>
              </>
            ) : (
              <div className="flex h-full min-h-[240px] flex-col items-center justify-center gap-3 text-center text-muted-foreground">
                <KeyRound className="size-6 opacity-40" />
                <p className="max-w-xs text-sm">
                  {supportsAPI(tool) ? t("selectProvider") : t("nativeOnly")}
                </p>
                {supportsAPI(tool) && (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setSelectedProvider("__new__")}
                  >
                    <Plus className="size-3.5" />
                    {t("addProvider")}
                  </Button>
                )}
              </div>
            )}
          </section>
        </div>
      )}
      <AddCustomAgentDialog
        open={addOpen}
        onOpenChange={setAddOpen}
        editRegistryId={editDefinition}
        onAdded={refresh}
      />
      <Dialog open={modelOpen} onOpenChange={setModelOpen}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>
              {t(editIndex === null ? "addModel" : "editModel")}
            </DialogTitle>
          </DialogHeader>
          <div className="space-y-3">
            <div className="space-y-1.5">
              <Label htmlFor="model-id">{t("modelId")}</Label>
              <Input
                id="model-id"
                value={modelDraft.id}
                autoFocus
                onChange={(e) =>
                  setModelDraft((old) => ({ ...old, id: e.target.value }))
                }
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="model-name">{t("modelName")}</Label>
              <Input
                id="model-name"
                value={modelDraft.name}
                onChange={(e) =>
                  setModelDraft((old) => ({ ...old, name: e.target.value }))
                }
              />
            </div>
          </div>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setModelOpen(false)}>
              {t("cancel")}
            </Button>
            <Button
              disabled={!modelDraft.id.trim()}
              onClick={() => {
                const id = modelDraft.id.trim()
                if (
                  form.models.some((m, i) => m.id === id && i !== editIndex)
                ) {
                  toast.error(t("duplicateModel"))
                  return
                }
                setForm((old) => {
                  const models = [...old.models]
                  const model = {
                    id,
                    name: modelDraft.name.trim(),
                    enabled:
                      editIndex === null ? true : models[editIndex].enabled,
                  }
                  if (editIndex === null) models.push(model)
                  else models[editIndex] = model
                  return {
                    ...old,
                    models,
                    model:
                      !old.model ||
                      old.model === old.models[editIndex ?? -1]?.id
                        ? id
                        : old.model,
                  }
                })
                setModelOpen(false)
              }}
            >
              {t("save")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <AlertDialog open={removeProvider} onOpenChange={setRemoveProvider}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("deleteProvider")}</AlertDialogTitle>
            <AlertDialogDescription>{selected?.name}</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("cancel")}</AlertDialogCancel>
            <AlertDialogAction
              onClick={() =>
                void run(async () => {
                  if (selected)
                    await getTransport().call("codepluse_provider_delete", {
                      id: selected.id,
                    })
                  setSelectedProvider("")
                  await load()
                })
              }
            >
              {t("deleteProvider")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <AlertDialog open={removeAgent} onOpenChange={setRemoveAgent}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("removeAgent")}</AlertDialogTitle>
            <AlertDialogDescription>{t("keepHistory")}</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("cancel")}</AlertDialogCancel>
            <AlertDialogAction
              onClick={() =>
                void run(async () => {
                  if (customID) await acpDeleteCustomAgent(customID, false)
                  setSelectedAgent("")
                  await load()
                })
              }
            >
              {t("removeAgent")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
