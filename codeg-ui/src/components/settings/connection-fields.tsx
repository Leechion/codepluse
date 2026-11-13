"use client"

import { useTranslations } from "next-intl"
import {
  Copy,
  RefreshCw,
  UserRound,
  CircleCheck,
  CircleHelp,
} from "lucide-react"
import { toast } from "sonner"
import { Button } from "@/components/ui/button"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { ProviderModelPicker } from "./provider-model-picker"
import type { AcpAgentInfo } from "@/lib/types"
import type {
  HarnessAuthMode,
  HarnessConnectionSource,
  HarnessProviderInfo,
} from "@/lib/api"

export interface ConnectionChoice {
  agentType: string
  authMode: HarnessAuthMode
  providerId: string
  model: string
}

export function agentTool(agent: AcpAgentInfo): string {
  return (
    agent.real_tool ||
    ({ claude_code: "claude", deepseek: "dsh" } as Record<string, string>)[
      agent.agent_type
    ] ||
    agent.agent_type
  )
}
export function supportsAPI(tool: string): boolean {
  return ["claude", "codex", "dsh"].includes(tool)
}
export function sourceLabel(tool: string): string {
  return tool === "claude"
    ? "CC Switch"
    : tool === "codex"
      ? "Codex"
      : tool === "dsh"
        ? "DSH"
        : ""
}

export function connectionValid(
  value: ConnectionChoice,
  source: HarnessConnectionSource | undefined,
  providers: HarnessProviderInfo[]
): boolean {
  if (!value.agentType) return false
  if (value.authMode === "subscription") return !!source?.account.supported
  return (
    !!value.model.trim() &&
    providers.some(
      (p) =>
        p.id === value.providerId &&
        p.agent === source?.tool &&
        p.hasKey &&
        p.api !== false &&
        p.enabled !== false
    )
  )
}

export function ConnectionModeSelect({
  value,
  onChange,
  source,
}: {
  value: HarnessAuthMode
  onChange: (v: HarnessAuthMode) => void
  source?: HarnessConnectionSource
}) {
  const t = useTranslations("Connections")
  return (
    <div className="space-y-1.5">
      <Label>{t("connectionType")}</Label>
      <Select
        value={value}
        onValueChange={(v) => v && onChange(v as HarnessAuthMode)}
      >
        <SelectTrigger aria-label={t("connectionType")} className="w-full">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem
            value="api"
            disabled={source ? !supportsAPI(source.tool) : false}
          >
            {t("api")}
          </SelectItem>
          <SelectItem
            value="subscription"
            disabled={source ? !source.account.supported : false}
          >
            {t("subscription")}
          </SelectItem>
        </SelectContent>
      </Select>
    </div>
  )
}

export function AccountConnection({
  source,
  model,
  onModelChange,
  onRefresh,
}: {
  source?: HarnessConnectionSource
  model: string
  onModelChange: (v: string) => void
  onRefresh: () => void
}) {
  const t = useTranslations("Connections")
  const account = source?.account
  if (!account?.supported)
    return (
      <p className="text-sm text-muted-foreground">{t("noSubscription")}</p>
    )
  const StatusIcon = account.configured ? CircleCheck : CircleHelp
  const models = account.models || []
  return (
    <div className="space-y-5">
      <div className="cp-account-card rounded-2xl border p-5">
        <div className="flex items-center gap-3">
          <div className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-primary/5">
            <UserRound className="size-5" />
          </div>
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">{t("nativeAccount")}</p>
            <p className="mt-1 flex items-center gap-1.5 text-xs text-muted-foreground">
              <StatusIcon className="size-3.5" />
              {account.configured
                ? t("credentialsFound")
                : account.managed
                  ? t("accountManaged")
                  : t("loginRequired")}
            </p>
          </div>
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={onRefresh}
            aria-label={t("refreshAccount")}
          >
            <RefreshCw className="size-4" />
          </Button>
        </div>
        <p className="mt-4 text-xs leading-relaxed text-muted-foreground">
          {t("accountHint")}
        </p>
        {account.loginCommand && !account.configured && (
          <div className="mt-3 flex items-center gap-2 rounded-lg border bg-background/25 px-3 py-2">
            <code className="min-w-0 flex-1 truncate text-xs">
              {account.loginCommand}
            </code>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={t("copyLogin")}
              onClick={() =>
                void navigator.clipboard
                  .writeText(account.loginCommand!)
                  .then(() => toast.success(t("copied")))
              }
            >
              <Copy className="size-3.5" />
            </Button>
          </div>
        )}
      </div>
      <div className="space-y-1.5">
        <Label>{t("model")}</Label>
        <Select
          value={model || "__default__"}
          onValueChange={(v) =>
            onModelChange(v === "__default__" ? "" : v || "")
          }
        >
          <SelectTrigger aria-label={t("model")} className="w-full">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="__default__">{t("accountDefault")}</SelectItem>
            {model && !models.some((m) => m.id === model) && (
              <SelectItem value={model}>{model}</SelectItem>
            )}
            {models.map((m) => (
              <SelectItem value={m.id} key={m.id}>
                {m.name || m.id}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
    </div>
  )
}

export function ConnectionFields({
  value,
  onChange,
  agents,
  providers,
  sources,
  onRefresh,
  hideHarness = false,
}: {
  value: ConnectionChoice
  onChange: (v: ConnectionChoice) => void
  agents: AcpAgentInfo[]
  providers: HarnessProviderInfo[]
  sources: HarnessConnectionSource[]
  onRefresh: () => void
  hideHarness?: boolean
}) {
  const t = useTranslations("Connections")
  const source = sources.find((s) => s.agent_type === value.agentType)
  const available = providers.filter(
    (p) => p.agent === source?.tool && p.api !== false && p.enabled !== false
  )
  return (
    <div className="space-y-4">
      {!hideHarness && (
        <div className="space-y-1.5">
          <Label>{t("harness")}</Label>
          <Select
            value={value.agentType}
            onValueChange={(v) => {
              if (!v) return
              const s = sources.find((s) => s.agent_type === v)
              const mode =
                s?.defaults.auth_mode ||
                (s?.account.configured || !supportsAPI(s?.tool || "")
                  ? "subscription"
                  : "api")
              onChange({
                agentType: v,
                authMode: mode,
                providerId: mode === "api" ? s?.defaults.provider_id || "" : "",
                model: s?.defaults.model || "",
              })
            }}
          >
            <SelectTrigger aria-label={t("harness")} className="w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {agents.map((a) => (
                <SelectItem value={a.agent_type} key={a.agent_type}>
                  {a.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      )}
      <ConnectionModeSelect
        value={value.authMode}
        source={source}
        onChange={(mode) =>
          onChange({ ...value, authMode: mode, providerId: "", model: "" })
        }
      />
      {value.authMode === "subscription" ? (
        <AccountConnection
          source={source}
          model={value.model}
          onModelChange={(model) => onChange({ ...value, model })}
          onRefresh={onRefresh}
        />
      ) : (
        <>
          <div className="space-y-1.5">
            <div className="flex items-center justify-between">
              <Label>{t("provider")}</Label>
              <button
                type="button"
                onClick={onRefresh}
                aria-label={t("refreshProviders")}
                className="text-muted-foreground hover:text-foreground"
              >
                <RefreshCw className="size-3.5" />
              </button>
            </div>
            <Select
              value={value.providerId || "__none__"}
              onValueChange={(id) => {
                const p = available.find((p) => p.id === id)
                onChange({
                  ...value,
                  providerId: p?.id || "",
                  model:
                    p?.model ||
                    p?.models?.find((m) => m.enabled !== false)?.id ||
                    "",
                })
              }}
            >
              <SelectTrigger aria-label={t("provider")} className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__none__" disabled>
                  {t("chooseProvider")}
                </SelectItem>
                {value.providerId &&
                  !available.some((p) => p.id === value.providerId) && (
                    <SelectItem value={value.providerId} disabled>
                      {t("missingProvider")}
                    </SelectItem>
                  )}
                {available.map((p) => (
                  <SelectItem key={p.id} value={p.id} disabled={!p.hasKey}>
                    {p.name}
                    {!p.hasKey ? ` · ${t("keyMissing")}` : ""}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <ProviderModelPicker
            provider={value.providerId}
            model={value.model}
            onChange={(model) => onChange({ ...value, model })}
          />
          <p className="text-xs text-muted-foreground">
            {t(source?.tool === "claude" ? "claudeSource" : "nativeSource", {
              name: sourceLabel(source?.tool || ""),
            })}
          </p>
        </>
      )}
    </div>
  )
}
