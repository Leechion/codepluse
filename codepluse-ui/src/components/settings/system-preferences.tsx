"use client"

import { useState } from "react"
import { useTranslations } from "next-intl"
import { Languages, Info } from "lucide-react"
import { toast } from "sonner"
import { useAppI18n } from "@/components/i18n-provider"
import { useAppUpdate } from "@/components/providers/update-provider"
import { SettingsSection } from "@/components/shared/settings-section"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Button } from "@/components/ui/button"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { updateSystemLanguageSettings } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import { APP_LOCALES } from "@/lib/i18n"
import type { AppLocale } from "@/lib/types"

const LANGUAGE_KEYS = {
  en: "english",
  zh_cn: "simplifiedChinese",
  zh_tw: "traditionalChinese",
  ja: "japanese",
  ko: "korean",
  es: "spanish",
  de: "german",
  fr: "french",
  pt: "portuguese",
  ar: "arabic",
} as const

/** Only settings supported by the Go runtime; no unsupported proxy/sync loads. */
export function SystemPreferences() {
  const t = useTranslations("SystemSettings")
  const language = useTranslations("Language")
  const { languageSettings, languageSettingsLoaded, setLanguageSettings } =
    useAppI18n()
  const [saving, setSaving] = useState(false)
  const update = useAppUpdate()
  return (
    <ScrollArea className="h-full">
      <div className="w-full space-y-4 p-3 md:p-4">
        <SettingsSection
          icon={Languages}
          title={t("languageTitle")}
          description={t("languageDescription")}
        >
          <Select
            value={
              languageSettings.mode === "system"
                ? "system"
                : languageSettings.language
            }
            disabled={saving || !languageSettingsLoaded}
            onValueChange={(value) => {
              if (
                value !== "system" &&
                !APP_LOCALES.includes(value as AppLocale)
              )
                return
              setSaving(true)
              void updateSystemLanguageSettings({
                mode: value === "system" ? "system" : "manual",
                language:
                  value === "system"
                    ? languageSettings.language
                    : (value as AppLocale),
              })
                .then(setLanguageSettings)
                .catch((error) => {
                  toast.error(
                    t("languageSaveFailed", { message: toErrorMessage(error) })
                  )
                })
                .finally(() => setSaving(false))
            }}
          >
            <SelectTrigger
              className="w-full sm:w-56"
              aria-label={t("appLanguage")}
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="system">{language("followSystem")}</SelectItem>
              {APP_LOCALES.map((locale) => (
                <SelectItem key={locale} value={locale}>
                  {language(LANGUAGE_KEYS[locale])}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingsSection>
        <SettingsSection icon={Info} title="codepluse">
          <div className="flex items-center justify-between gap-3 text-xs">
            <span className="text-muted-foreground">
              {t("currentVersion")}：{update?.currentVersion || "—"}
            </span>
            <Button
              variant="ghost"
              size="sm"
              disabled={!update || update.checking}
              onClick={() => void update?.checkNow()}
            >
              {update?.checking ? t("checking") : t("checkUpdate")}
            </Button>
          </div>
          {update?.checkError && (
            <p className="text-xs text-destructive">
              {toErrorMessage(update.checkError)}
            </p>
          )}
        </SettingsSection>
      </div>
    </ScrollArea>
  )
}
