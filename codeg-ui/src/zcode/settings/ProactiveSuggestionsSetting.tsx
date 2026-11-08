import { useState } from "react";
import { Switch } from "../components/ui/switch";
import { toast } from "../components/ui/toast";
import { useIsOfficeMode } from "../hooks/useInterfaceMode";
import { useOnboardingRecordService } from "../hooks/useOnboardingRecordService";
import { useSettings } from "../hooks/useSettingService";
import { useZCodeIntl } from "../i18n/IntlProvider";
import { logger } from "../logger";
import { SettingsRow } from "./SettingsPageParts";

export function ProactiveSuggestionsSetting() {
  const { intl } = useZCodeIntl();
  const { settings, update } = useSettings();
  const onboardingRecordService = useOnboardingRecordService();
  const isOfficeMode = useIsOfficeMode();
  const [saving, setSaving] = useState(false);
  const setSuggestions = async (enabled: boolean) => {
    setSaving(true);
    try {
      await update({ proactiveSuggestionsEnabled: enabled });
      await onboardingRecordService
        ?.updateRecordPreferences({ proactiveSuggestionsEnabled: enabled })
        .catch((cause: unknown) => {
          logger.warn("[settings] 回写引导记录失败", { error: String(cause) });
        });
    } catch (error) {
      logger.warn("[settings] 更新主动任务推荐失败", { error: String(error) });
      toast(intl.formatMessage({ id: "chat.officeSuggestions.saveError" }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <SettingsRow
      label={intl.formatMessage({ id: "chat.officeSuggestions.setting" })}
      description={intl.formatMessage({ id: "chat.officeSuggestions.settingDescription" })}
      control={
        <Switch
          checked={isOfficeMode && settings?.proactiveSuggestionsEnabled === true}
          disabled={!isOfficeMode || saving || !settings}
          onCheckedChange={setSuggestions}
          aria-label={intl.formatMessage({ id: "chat.officeSuggestions.setting" })}
        />
      }
    />
  );
}
