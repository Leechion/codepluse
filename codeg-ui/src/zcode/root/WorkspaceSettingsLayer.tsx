import { useEffect } from "react";
import { SettingsPage } from "../SettingsPage";
import { ServiceProvider } from "../hooks/useServices";
import { logger } from "../logger";
import type { WorkspaceSettingsLayerProps } from "./types";

export function WorkspaceSettingsLayer({
  workspaceScopedServices,
  isDesktop,
  isMacDesktop,
  isWindowsDesktop,
  isWebRemoteControl,
  windowsWindowControlsRightPaddingPx,
  captionWorkspacePath,
  onBack,
  onCreateTask,
  onOpenWorkspace,
  onOpenRemoteWorkspace,
  allowOpenWorkspace,
  onLogin,
  onLogout,
  user,
}: WorkspaceSettingsLayerProps) {
  useEffect(() => {
    logger.info("[Root] settings layer mounted");
    return () => {
      logger.info("[Root] settings layer unmounted");
    };
  }, []);

  return (
    <div className="absolute inset-0 z-10">
      {workspaceScopedServices ? (
        <ServiceProvider services={workspaceScopedServices}>
          <SettingsPage
            isDesktop={isDesktop}
            isMacDesktop={isMacDesktop}
            isWindowsDesktop={isWindowsDesktop}
            isWebRemoteControl={isWebRemoteControl}
            windowsWindowControlsRightPaddingPx={windowsWindowControlsRightPaddingPx}
            captionWorkspacePath={captionWorkspacePath}
            onBack={onBack}
            onCreateTask={onCreateTask}
            onOpenWorkspace={onOpenWorkspace}
            onOpenRemoteWorkspace={onOpenRemoteWorkspace}
            allowOpenWorkspace={allowOpenWorkspace}
            onLogin={onLogin}
            onLogout={onLogout}
            user={user}
          />
        </ServiceProvider>
      ) : (
        <SettingsPage
          isDesktop={isDesktop}
          isMacDesktop={isMacDesktop}
          isWindowsDesktop={isWindowsDesktop}
          isWebRemoteControl={isWebRemoteControl}
          windowsWindowControlsRightPaddingPx={windowsWindowControlsRightPaddingPx}
          captionWorkspacePath={captionWorkspacePath}
          onBack={onBack}
          onCreateTask={onCreateTask}
          onOpenWorkspace={onOpenWorkspace}
          onOpenRemoteWorkspace={onOpenRemoteWorkspace}
          allowOpenWorkspace={allowOpenWorkspace}
          onLogin={onLogin}
          onLogout={onLogout}
          user={user}
        />
      )}
    </div>
  );
}
