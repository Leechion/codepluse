import type {
  ConnectionState,
  PendingPermission,
  PendingTaskSplit,
} from "@/contexts/acp-connections-context"
import type {
  PendingPlanApprovalState,
  PendingQuestionState,
} from "@/lib/types"

/**
 * What a connection is parked on until the user answers: the prompts that
 * resolve WITHOUT driving a new turn — a permission request, a codeg-mcp
 * `ask_user_question`, a plan approval, and a task-split card awaiting
 * confirmation. The first three are what the backend's `blocked_on` reports for
 * a delegated task (Rust `BlockedKind`); the split card is not turn-parking —
 * the proposal tool returns immediately — but the card still asks the user for
 * a decision, so it belongs here. The legacy free-text `pendingQuestion` is
 * not one of them: it is answered by sending a prompt.
 */
export interface BlockingPrompts {
  permission: PendingPermission | null
  askQuestion: PendingQuestionState | null
  planApproval: PendingPlanApprovalState | null
  split: PendingTaskSplit | null
  /** One stable id per decision asked for (`permission:<request_id>`, …): a
   *  new decision brings a new id, an answered one takes its id away. */
  ids: string[]
}

type PromptFields = Pick<
  ConnectionState,
  | "pendingPermission"
  | "pendingAskQuestion"
  | "pendingPlanApproval"
  | "pendingSplit"
>

/** The connection's blocking prompts, or `null` when it waits on nobody. */
export function blockingPrompts(
  conn: PromptFields | null | undefined
): BlockingPrompts | null {
  if (!conn) return null
  const permission = conn.pendingPermission ?? null
  // A question set with no questions renders no card, so it asks nothing.
  const askQuestion =
    conn.pendingAskQuestion && conn.pendingAskQuestion.questions.length > 0
      ? conn.pendingAskQuestion
      : null
  const planApproval = conn.pendingPlanApproval ?? null
  // A split with no tasks renders no card, so it asks nothing.
  const split =
    conn.pendingSplit && conn.pendingSplit.tasks.length > 0
      ? conn.pendingSplit
      : null
  const ids: string[] = []
  if (permission) ids.push(`permission:${permission.request_id}`)
  if (askQuestion) ids.push(`question:${askQuestion.question_id}`)
  if (planApproval) ids.push(`plan:${planApproval.approval_id}`)
  if (split) ids.push(`split:${split.split_id}`)
  if (ids.length === 0) return null
  return { permission, askQuestion, planApproval, split, ids }
}
