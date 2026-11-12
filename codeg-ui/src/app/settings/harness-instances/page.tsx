import { redirect } from "next/navigation"

/**
 * The harness-instances page was merged into Collaboration. Keep the old URL
 * alive as a redirect so bookmarks and in-app links do not 404; under static
 * export Next emits a client-side redirect page here.
 */
export default function SettingsHarnessInstancesPage() {
  redirect("/settings/collaboration")
}
