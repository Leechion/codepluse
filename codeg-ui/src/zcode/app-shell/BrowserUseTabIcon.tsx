import { MousePointer2Icon } from "lucide-react";
import { BrowserTabFavicon } from "./BrowserTabFavicon";
import { useBrowserUseOperationActive } from "../browser-use/useBrowserUseOperationActive";
import type { BrowserUseSidePaneTab } from "../lib/workspaceSidePane";

export function BrowserUseTabIcon({ tab }: { tab: BrowserUseSidePaneTab }) {
  const isAgentOperating = useBrowserUseOperationActive(tab.browserUseOperationUntil);

  if (isAgentOperating) {
    return (
      <span
        aria-hidden="true"
        data-browser-use-operation-indicator="active"
        className="browser-use-operation-breathe inline-flex size-3.5 shrink-0 items-center justify-center"
      >
        <MousePointer2Icon className="size-3.5" />
      </span>
    );
  }

  return <BrowserTabFavicon faviconUrl={tab.faviconUrl} />;
}
