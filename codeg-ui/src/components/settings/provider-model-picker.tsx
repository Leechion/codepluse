"use client"
import { useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { RefreshCw } from "lucide-react"
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
import { listHarnessProviderModels } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"

const EMPTY = "__empty__"

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
