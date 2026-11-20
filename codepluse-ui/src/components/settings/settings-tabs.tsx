"use client"

import { useState, type ReactNode } from "react"
import { usePathname, useRouter, useSearchParams } from "next/navigation"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"

export function SettingsTabs({
  panels,
}: {
  panels: { id: string; label: string; content: ReactNode }[]
}) {
  const search = useSearchParams()
  const pathname = usePathname()
  const router = useRouter()
  const requested = search.get("view")
  const current =
    panels.find((panel) => panel.id === requested)?.id ?? panels[0].id
  const [selection, setSelection] = useState({
    source: current,
    active: current,
    visited: [current],
  })

  // Honour deep links and history while keeping visited editors mounted so
  // switching between settings tabs does not discard an unsaved draft.
  if (selection.source !== current) {
    setSelection({
      source: current,
      active: current,
      visited: selection.visited.includes(current)
        ? selection.visited
        : [...selection.visited, current],
    })
  }
  const { active, visited } = selection

  return (
    <Tabs
      value={active}
      onValueChange={(next) => {
        setSelection((previous) => ({
          ...previous,
          active: next,
          visited: previous.visited.includes(next)
            ? previous.visited
            : [...previous.visited, next],
        }))
        const params = new URLSearchParams(search.toString())
        params.set("view", next)
        router.replace(`${pathname}?${params.toString()}`, { scroll: false })
      }}
      className="h-full min-h-0 flex flex-col gap-0"
    >
      <div className="shrink-0 px-4 pt-4 pb-2">
        <TabsList className="max-w-full overflow-x-auto">
          {panels.map((panel) => (
            <TabsTrigger
              key={panel.id}
              value={panel.id}
              className="data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-sm"
            >
              {panel.label}
            </TabsTrigger>
          ))}
        </TabsList>
      </div>
      {panels.map((panel) =>
        visited.includes(panel.id) ? (
          <TabsContent
            key={panel.id}
            value={panel.id}
            forceMount
            hidden={active !== panel.id}
            className="flex-1 min-h-0 overflow-hidden data-[state=inactive]:hidden"
          >
            {panel.content}
          </TabsContent>
        ) : null
      )}
    </Tabs>
  )
}
