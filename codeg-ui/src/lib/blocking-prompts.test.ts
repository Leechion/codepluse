import { describe, expect, it } from "vitest"

import { blockingPrompts } from "@/lib/blocking-prompts"

const PERMISSION = { request_id: "req-1", tool_call: {}, options: [] }
const QUESTION = {
  question_id: "q-1",
  questions: [
    {
      id: "q",
      question: "Which DB?",
      header: "DB",
      multi_select: false,
      options: [],
    },
  ],
  created_at: "2026-10-18T00:00:00Z",
}
const PLAN = {
  approval_id: "plan-1",
  tool_call_id: "tc-1",
  plan_markdown: "1. migrate",
  created_at: "2026-10-18T00:00:00Z",
}
const SPLIT = {
  split_id: "sp-1",
  parent_connection_id: "c-parent",
  root_connection_id: "c-root",
  tasks: [
    {
      index: 0,
      title: "audit",
      prompt: "audit the auth flow",
      agent_type: "claude_code",
      model: "fixture-v1",
    },
  ],
  created_at: "2026-10-18T00:00:00Z",
}
const NONE = {
  pendingPermission: null,
  pendingAskQuestion: null,
  pendingPlanApproval: null,
  pendingSplit: null,
}

describe("blockingPrompts", () => {
  it("is null for a connection waiting on nobody, or none at all", () => {
    expect(blockingPrompts(undefined)).toBeNull()
    expect(blockingPrompts(NONE)).toBeNull()
  })

  it("does not count a question set with no questions — it renders nothing", () => {
    expect(
      blockingPrompts({
        ...NONE,
        pendingAskQuestion: { ...QUESTION, questions: [] },
      })
    ).toBeNull()
  })

  it("does not count a split with no tasks — it renders no card", () => {
    expect(
      blockingPrompts({ ...NONE, pendingSplit: { ...SPLIT, tasks: [] } })
    ).toBeNull()
  })

  it("names each decision by its own id, so only a new decision brings a new one", () => {
    const first = blockingPrompts({ ...NONE, pendingPermission: PERMISSION })
    expect(first?.permission).toBe(PERMISSION)
    expect(first?.ids).toEqual(["permission:req-1"])
    expect(
      blockingPrompts({
        ...NONE,
        pendingPermission: { ...PERMISSION, request_id: "req-2" },
      })?.ids
    ).toEqual(["permission:req-2"])

    const all = blockingPrompts({
      pendingPermission: PERMISSION,
      pendingAskQuestion: QUESTION,
      pendingPlanApproval: PLAN,
      pendingSplit: SPLIT,
    })
    expect(all).toMatchObject({
      permission: PERMISSION,
      askQuestion: QUESTION,
      planApproval: PLAN,
      split: SPLIT,
    })
    expect(all?.ids).toEqual([
      "permission:req-1",
      "question:q-1",
      "plan:plan-1",
      "split:sp-1",
    ])
  })
})
