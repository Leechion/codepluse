"use client"

import { useEffect } from "react"
import { useRouter, useSearchParams } from "next/navigation"

export function settingsRedirectTarget(
  to: string,
  search: string,
  view?: string
) {
  const params = new URLSearchParams(search)
  if (view) params.set("view", view)
  const query = params.toString()
  return `${to}${query ? `?${query}` : ""}`
}

/** Retain locale, remote workspace and pack-tab deep links when merging pages. */
export function SettingsRedirect({ to, view }: { to: string; view?: string }) {
  const router = useRouter()
  const search = useSearchParams().toString()
  useEffect(() => {
    router.replace(settingsRedirectTarget(to, search, view))
  }, [router, search, to, view])
  return null
}
