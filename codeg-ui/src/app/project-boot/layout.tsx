import { Suspense } from "react"
import { RemoteConnectionGate } from "@/contexts/remote-connection-context"

export default function ProjectBootLayout({
  children,
}: {
  children: React.ReactNode
}) {
  return (
    <Suspense>
      <RemoteConnectionGate>{children}</RemoteConnectionGate>
    </Suspense>
  )
}
