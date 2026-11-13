import { NOT_A_GIT_REPO_PATTERNS } from "@/i18n/git-error-patterns"
import type { AppCommandError } from "@/lib/types"

type ObjectLike = Record<string, unknown>

function asObject(value: unknown): ObjectLike | null {
  return value !== null && typeof value === "object"
    ? (value as ObjectLike)
    : null
}

function normalizeString(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value.trim() : null
}

function parseI18nParams(value: unknown): Record<string, string> | null {
  const obj = asObject(value)
  if (!obj) return null
  const out: Record<string, string> = {}
  for (const [key, raw] of Object.entries(obj)) {
    if (typeof raw === "string") {
      out[key] = raw
    } else if (typeof raw === "number" || typeof raw === "boolean") {
      out[key] = String(raw)
    }
  }
  return Object.keys(out).length > 0 ? out : null
}

function parseErrorObject(value: unknown): AppCommandError | null {
  const obj = asObject(value)
  if (!obj) return null

  const code = normalizeString(obj.code)
  const message = normalizeString(obj.message)
  const detailRaw = normalizeString(obj.detail)
  const detail = detailRaw ?? null
  const i18nKey = normalizeString(obj.i18n_key)
  const i18nParams = parseI18nParams(obj.i18n_params)

  if (!code || !message) return null

  return {
    code,
    message,
    detail,
    i18n_key: i18nKey,
    i18n_params: i18nParams,
  }
}

export function extractAppCommandError(error: unknown): AppCommandError | null {
  const direct = parseErrorObject(error)
  if (direct) return direct

  const errorObject = asObject(error)
  const message = normalizeString(errorObject?.message)
  if (!message) return null

  try {
    const parsed = JSON.parse(message)
    return parseErrorObject(parsed)
  } catch {
    return null
  }
}

// Must mirror `AppErrorCode::NotAGitRepository` in src-tauri/src/app_error.rs.
// If the backend enum ever renames, both sides must change together.
export const NOT_A_GIT_REPO_CODE = "not_a_git_repository"

export function isNotAGitRepoError(error: unknown): boolean {
  const appError = extractAppCommandError(error)
  if (appError?.code === NOT_A_GIT_REPO_CODE) return true

  const candidates = [appError?.detail, appError?.message]
  return candidates.some(
    (text) =>
      typeof text === "string" &&
      NOT_A_GIT_REPO_PATTERNS.some((pattern) => pattern.test(text))
  )
}

/** Whether a command failed because the thing it named does not exist —
 *  `AppErrorCode::NotFound` on the backend. */
export function isNotFoundError(error: unknown): boolean {
  return extractAppCommandError(error)?.code === "not_found"
}

/**
 * 把后端 HTTP 错误体（`{code,message,detail,i18n_key,i18n_params}`）转成 Error。
 *
 * Go 后台失败时返回 JSON 对象，传输层原本直接 `throw` 这个纯对象；而 Tauri 侧后端
 * 会把错误塌缩成字符串，所以 `err instanceof Error ? err.message : String(err)`
 * 这种写法在 Tauri 下正常、在 Go 下却渲染成 `[object Object]`（非 vendor 树里就有
 * 18 处）。统一成 Error 之后：旧写法天然拿到可读文案，`extractAppCommandError`
 * 也仍能从对象上读到 code/detail，`isNotFoundError` 一类判定不受影响。
 */
export function toCommandError(body: unknown, status?: number): Error {
  const parsed = parseErrorObject(body)
  const text =
    parsed?.message ??
    normalizeString(typeof body === "string" ? body : undefined) ??
    (status ? `HTTP ${status}` : "Request failed")
  const error = Object.assign(new Error(text), {
    code: parsed?.code ?? "task_execution_failed",
  }) as Error & AppCommandError
  if (parsed?.detail) error.detail = parsed.detail
  if (parsed?.i18n_key) error.i18n_key = parsed.i18n_key
  if (parsed?.i18n_params) error.i18n_params = parsed.i18n_params
  return error
}

export function toErrorMessage(error: unknown): string {
  const appError = extractAppCommandError(error)
  if (appError) {
    return appError.detail?.trim() || appError.message
  }

  if (error instanceof Error) {
    return error.message.trim()
  }

  if (typeof error === "string") {
    return error.trim()
  }

  try {
    const serialized = JSON.stringify(error)
    return serialized ? serialized.trim() : String(error)
  } catch {
    return String(error)
  }
}

/** Translator callable shape compatible with next-intl's scoped translator. */
export type AppErrorTranslator = (
  key: string,
  params?: Record<string, string | number>
) => string

/**
 * Like `toErrorMessage`, but prefers the backend-provided i18n hint when
 * present. The translator should be scoped to the namespace whose keys the
 * backend emits (e.g. for MCP errors, a translator scoped to `McpSettings`).
 *
 * Falls back to the English `message` when the key is missing OR the
 * translator throws (e.g. unknown key in a different translator's namespace).
 */
export function toLocalizedErrorMessage(
  error: unknown,
  translate: AppErrorTranslator
): string {
  const appError = extractAppCommandError(error)
  if (appError?.i18n_key) {
    try {
      const params = appError.i18n_params ?? undefined
      const localized = translate(appError.i18n_key, params)
      const trimmed = localized.trim()
      if (trimmed && trimmed !== appError.i18n_key) {
        return trimmed
      }
    } catch {
      // fall through to non-localized path
    }
  }
  return toErrorMessage(error)
}
