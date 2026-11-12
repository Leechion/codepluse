"use client"

import { Suspense } from "react"
import {
  AppWorkspaceProvider,
  ConversationStatusEventBridge,
} from "@/contexts/app-workspace-context"
import { AlertProvider } from "@/contexts/alert-context"
import { GitCredentialProvider } from "@/contexts/git-credential-context"
import { TaskProvider } from "@/contexts/task-context"
import { AcpConnectionsProvider } from "@/contexts/acp-connections-context"
import { DelegationProvider } from "@/contexts/delegation-context"
import { ConversationRuntimeProvider } from "@/contexts/conversation-runtime-context"
import { WorkspaceProvider } from "@/contexts/workspace-context"
import { TabProvider } from "@/contexts/tab-context"
import { SidebarProvider } from "@/contexts/sidebar-context"
import { AuxPanelProvider } from "@/contexts/aux-panel-context"
import { TerminalProvider } from "@/contexts/terminal-context"
import { SearchDialogProvider } from "@/contexts/search-dialog-context"
import { AutomationsViewProvider } from "@/contexts/automations-view-context"
import { TasksViewProvider } from "@/contexts/tasks-view-context"
import { WorkbenchRouteProvider } from "@/contexts/workbench-route-context"
import { RemoteConnectionGate } from "@/contexts/remote-connection-context"
import { UpdateProvider } from "@/components/providers/update-provider"
import { ConversationTagsSync } from "@/stores/conversation-tags-store"
import { AssistantsSync } from "@/stores/assistants-store"
import { ConversationTagsManagerHost } from "@/components/conversations/conversation-tags-manager"
import { TaskFromMessageHost } from "@/components/tasks/task-from-message-host"
import { ExternalConflictDialog } from "@/components/files/external-conflict-dialog"
import { HeavyPluginsWarmup } from "@/components/ai-elements/heavy-plugins-warmup"

export default function WorkspaceLayout({
  children,
}: {
  children: React.ReactNode
}) {
  return (
    <Suspense>
      <RemoteConnectionGate>
        <UpdateProvider>
          <AppWorkspaceProvider>
            <AlertProvider>
              <GitCredentialProvider>
                <TaskProvider>
                  <AcpConnectionsProvider>
                    <DelegationProvider>
                      <ConversationStatusEventBridge />
                      <ConversationTagsSync />
                      <AssistantsSync />
                      <ConversationTagsManagerHost />
                      <ConversationRuntimeProvider>
                        <WorkspaceProvider>
                          <TabProvider>
                            <HeavyPluginsWarmup />
                            <ExternalConflictDialog />
                            <SidebarProvider>
                              <AuxPanelProvider>
                                <TerminalProvider>
                                  <SearchDialogProvider>
                                    <AutomationsViewProvider>
                                      <TasksViewProvider>
                                        <WorkbenchRouteProvider>
                                          <TaskFromMessageHost />
                                          {children}
                                        </WorkbenchRouteProvider>
                                      </TasksViewProvider>
                                    </AutomationsViewProvider>
                                  </SearchDialogProvider>
                                </TerminalProvider>
                              </AuxPanelProvider>
                            </SidebarProvider>
                          </TabProvider>
                        </WorkspaceProvider>
                      </ConversationRuntimeProvider>
                    </DelegationProvider>
                  </AcpConnectionsProvider>
                </TaskProvider>
              </GitCredentialProvider>
            </AlertProvider>
          </AppWorkspaceProvider>
        </UpdateProvider>
      </RemoteConnectionGate>
    </Suspense>
  )
}