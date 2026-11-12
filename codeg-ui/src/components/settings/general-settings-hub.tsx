"use client"

import { useTranslations } from "next-intl"
import { GeneralSettings } from "./general-settings"
import { QuickMessagesSettings } from "./quick-messages-settings"
import { SystemPreferences } from "./system-preferences"
import { SettingsTabs } from "./settings-tabs"

export function GeneralSettingsHub() {
  const t = useTranslations("SettingsNavigation")
  const nav = useTranslations("SettingsShell.nav")
  return (
    <SettingsTabs
      panels={[
        {
          id: "general",
          label: t("preferences"),
          content: <GeneralSettings />,
        },
        {
          id: "quick-messages",
          label: nav("quick_messages"),
          content: <QuickMessagesSettings />,
        },
        { id: "system", label: t("system"), content: <SystemPreferences /> },
      ]}
    />
  )
}
