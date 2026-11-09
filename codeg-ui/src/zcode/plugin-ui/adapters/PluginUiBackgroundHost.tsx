import { useEffect, useRef } from "react";
import { useOptionalPlatform } from "../../hooks/usePlatform";
import { useOptionalServices } from "../../hooks/useServices";
import { useZCodeIntl } from "../../i18n/IntlProvider";
import { attachPluginUiToolResults } from "./pluginUiBackground";

export function PluginUiBackgroundHost() {
  const platform = useOptionalPlatform();
  const services = useOptionalServices();
  const { locale } = useZCodeIntl();
  const latestLocale = useRef(locale);
  latestLocale.current = locale;
  useEffect(() => {
    if (!platform || !services) return;
    return attachPluginUiToolResults(services, platform, () => ({
      locale: latestLocale.current,
      theme: document.documentElement.classList.contains("dark") ? "dark" : "light",
    }));
  }, [platform, services]);
  return null;
}
