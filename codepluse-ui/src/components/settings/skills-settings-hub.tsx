"use client"

import { useTranslations } from "next-intl"
import { SkillsSettings } from "./skills-settings"
import { SkillPacksSettings } from "./skill-packs-settings"
import { SettingsTabs } from "./settings-tabs"

export function SkillsSettingsHub() {
  const t = useTranslations("SettingsNavigation")
  return (
    <SettingsTabs
      panels={[
        { id: "installed", label: t("installed"), content: <SkillsSettings /> },
        { id: "packs", label: t("packs"), content: <SkillPacksSettings /> },
      ]}
    />
  )
}
