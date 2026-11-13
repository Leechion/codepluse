"use client"

/**
 * The main bot: one harness (built-in or instance wire) plus an optional
 * model that every new conversation starts on. Lives server-side in
 * `get_main_bot_settings`; this
 * module is the client cache so sync call sites (draft-tab resolution, the
 * connect-time prefs merge) don't await the API on every keystroke.
 *
 * An unset main bot (or one naming a harness that vanished) reads as `null` —
 * callers fall through to their previous defaults, so adopting this feature
 * can never strand a new conversation on a dead agent.
 */

import { getMainBotSettings, setMainBotSettings } from "@/lib/api"
import type { AgentType } from "@/lib/types"

export interface MainBot {
  agentType: AgentType
  model: string
  providerId?: string
  baseAgentType?: AgentType
}

let cached: MainBot | null = null
let loaded = false
let inflight: Promise<MainBot | null> | null = null
let generation = 0

function normalize(
  raw:
    | {
        agent_type?: string
        model?: string
        provider_id?: string
        base_agent_type?: string
      }
    | null
    | undefined
): MainBot | null {
  const agentType = (raw?.agent_type ?? "").trim()
  if (!agentType) return null
  return {
    agentType,
    model: (raw?.model ?? "").trim(),
    providerId: raw?.provider_id,
    baseAgentType: raw?.base_agent_type,
  }
}

export function applyMainBotSettings(raw: Parameters<typeof normalize>[0]) {
  generation += 1
  cached = normalize(raw)
  loaded = true
  return cached
}

/** Cached value without fetching — for sync consumers (may be stale). */
export function getCachedMainBot(): MainBot | null {
  return cached
}

/** Fetch (or reuse the in-flight fetch) and cache; resolves null when unset. */
export async function loadMainBot(force = false): Promise<MainBot | null> {
  if (!force && loaded) return cached
  if (!force && inflight) return inflight
  const requestGeneration = ++generation
  inflight = (async () => {
    try {
      const raw = await getMainBotSettings()
      if (requestGeneration === generation) cached = normalize(raw)
    } catch {
      // Keep the previous cache over a transient backend hiccup; an
      // unreachable backend also can't serve conversations anyway.
      if (!loaded && requestGeneration === generation) cached = null
    } finally {
      loaded = true
      inflight = null
    }
    return cached
  })()
  return inflight
}

/** Write through the backend, then update the cache. */
export async function saveMainBot(
  agentType: AgentType | null,
  model = "",
  providerId = ""
): Promise<void> {
  const raw = await setMainBotSettings({
    agent_type: agentType ?? "",
    model: model ?? "",
    provider_id: providerId,
  })
  applyMainBotSettings(
    raw ?? { agent_type: agentType ?? "", model, provider_id: providerId }
  )
}

/**
 * The main bot's agent type, validated against the caller's fresh agent list.
 * Consumers pass the same list they use for everything else, so deleting an
 * instance in settings immediately stops new drafts from landing on it.
 * An empty list means the caller knows nothing yet (cold start) — the bot is
 * trusted there, and the standard provisional-correction pass re-evaluates
 * the draft once the fresh list lands.
 */
export function mainBotAgentFor(available: AgentType[]): AgentType | null {
  if (!cached) return null
  if (cached.providerId) return cached.agentType
  if (available.length === 0) return cached.agentType
  return available.includes(cached.agentType) ? cached.agentType : null
}

/**
 * Connect-time merge: a main-bot model rides along ONLY when it targets this
 * harness. Provider-bound main profiles override device-local picks so a new
 * conversation always starts on the saved model. Legacy settings keep their
 * former preference ordering until a provider is configured.
 */
export function withMainBotModel(
  agentType: AgentType,
  prefs: { modeId: string | null; configValues: Record<string, string> | null }
): { modeId: string | null; configValues: Record<string, string> | null } {
  if (!cached || cached.agentType !== agentType || !cached.model) return prefs
  if (prefs.configValues?.model && !cached.providerId) return prefs
  return {
    ...prefs,
    configValues: { ...prefs.configValues, model: cached.model },
  }
}
