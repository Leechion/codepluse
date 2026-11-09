import { TID_SIDE_PANE_TOGGLE } from "@zcode/shared/test-ids";
import { PanelRightClose, PanelRightOpen } from "lucide-react";
import { cn } from "./lib/utils";
import { Button } from "./ui/button";
import { ControlHintTooltip } from "../ControlHintTooltip";
import { useZCodeIntl } from "../i18n/IntlProvider";
import { WINDOWS_CAPTION_CONTROL_CLASS } from "../windowCaptionControls";

export function WorkspaceSidePaneToggleButton({
  isSidePaneOpen,
  onToggleSidePane,
  shortcutLabel,
  useWindowsCaptionSpacing = false,
}: {
  isSidePaneOpen: boolean;
  onToggleSidePane: () => void;
  shortcutLabel?: string;
  useWindowsCaptionSpacing?: boolean;
}) {
  const { intl } = useZCodeIntl();
  const SidePaneToggleIcon = isSidePaneOpen ? PanelRightClose : PanelRightOpen;

  return (
    <ControlHintTooltip
      title={intl.formatMessage({ id: "sidePane.togglePanel" })}
      side="bottom"
      shortcut={shortcutLabel}
    >
      <Button
        type="button"
        variant="ghost"
        size="icon-md"
        data-testid={TID_SIDE_PANE_TOGGLE}
        className={cn(
          "text-foreground hover:bg-hover hover:text-foreground [app-region:no-drag]",
          useWindowsCaptionSpacing && WINDOWS_CAPTION_CONTROL_CLASS,
          isSidePaneOpen && "!bg-selected text-foreground",
        )}
        aria-label={intl.formatMessage({
          id: isSidePaneOpen ? "sidePane.collapse" : "sidePane.expand",
        })}
        onClick={onToggleSidePane}
      >
        <SidePaneToggleIcon className="size-4" />
      </Button>
    </ControlHintTooltip>
  );
}
