"use client"

// 模型供应商管理：智能体设置页「供应商」折叠区的上半部分。
// 走 list/create/update/delete_model_provider 后端命令（Go 移植已实现）。
import { useCallback, useEffect, useState } from "react"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import {
  createModelProvider,
  deleteModelProvider,
  listModelProviders,
  updateModelProvider,
} from "@/lib/api"
import type { ModelProviderInfo } from "@/lib/types"

const AGENT_TYPES = ["claude_code", "codex", "gemini"] as const
const AGENT_LABELS: Record<string, string> = {
  claude_code: "Claude Code",
  codex: "Codex",
  gemini: "Gemini",
}

type Form = {
  id: number | null
  name: string
  apiUrl: string
  apiKey: string
  agentType: string
  model: string
}

const empty = (agentType: string): Form => ({
  id: null,
  name: "",
  apiUrl: "",
  apiKey: "",
  agentType,
  model: "",
})

export function ModelProviderManager() {
  const [providers, setProviders] = useState<ModelProviderInfo[]>([])
  const [filter, setFilter] = useState<string>("all")
  const [form, setForm] = useState<Form>(empty("claude_code"))
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState("")

  const load = useCallback(async () => {
    setProviders(await listModelProviders())
  }, [])
  useEffect(() => {
    void load().catch((e) => setMessage(String(e)))
  }, [load])

  async function run(action: () => Promise<unknown>, success: string) {
    setBusy(true)
    setMessage("")
    try {
      await action()
      await load()
      setMessage(success)
    } catch (e) {
      setMessage(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  const visible =
    filter === "all"
      ? providers
      : providers.filter((p) => p.agent_type === filter)

  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-muted-foreground">
        为智能体保存模型供应商凭据（名称 / API 地址 / 密钥 / 模型）。密钥保存在本机项目数据目录，列表中只显示掩码。
      </p>

      <div className="flex items-center gap-2">
        <select
          className="h-8 rounded-md border bg-background px-2 text-sm"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        >
          <option value="all">全部类型</option>
          {AGENT_TYPES.map((t) => (
            <option key={t} value={t}>
              {AGENT_LABELS[t]}
            </option>
          ))}
        </select>
        <span className="text-xs text-muted-foreground">
          共 {visible.length} 个
        </span>
      </div>

      <div className="space-y-2">
        {visible.length === 0 ? (
          <div className="rounded-lg border p-4 text-sm text-muted-foreground">
            尚未配置模型供应商。
          </div>
        ) : (
          visible.map((p) => (
            <div
              key={p.id}
              className="flex flex-wrap items-center justify-between gap-3 rounded-lg border p-3"
            >
              <div>
                <div className="font-medium">
                  {p.name} · {AGENT_LABELS[p.agent_type] ?? p.agent_type}
                </div>
                <div className="break-all text-xs text-muted-foreground">
                  {p.api_url} · {p.model || "默认模型"} · 密钥 {p.api_key_masked}
                </div>
              </div>
              <div className="flex gap-2">
                <Button
                  size="sm"
                  variant="outline"
                  disabled={busy}
                  onClick={() =>
                    setForm({
                      id: p.id,
                      name: p.name,
                      apiUrl: p.api_url,
                      apiKey: "",
                      agentType: p.agent_type,
                      model: p.model ?? "",
                    })
                  }
                >
                  编辑
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={busy}
                  onClick={() => {
                    if (window.confirm(`删除供应商「${p.name}」？`))
                      void run(
                        () => deleteModelProvider(p.id),
                        "供应商已删除。"
                      )
                  }}
                >
                  删除
                </Button>
              </div>
            </div>
          ))
        )}
      </div>

      <form
        className="space-y-3 rounded-lg border p-4"
        onSubmit={(e) => {
          e.preventDefault()
          setBusy(true)
          setMessage("")
          const done = () => {
            setForm(empty(form.agentType))
            setBusy(false)
          }
          if (form.id != null) {
            void run(async () => {
              await updateModelProvider({
                id: form.id,
                name: form.name,
                apiUrl: form.apiUrl,
                apiKey: form.apiKey || null,
                agentType: form.agentType,
                model: form.model || null,
              })
              done()
            }, "供应商已更新。")
          } else {
            void run(async () => {
              await createModelProvider({
                name: form.name,
                apiUrl: form.apiUrl,
                apiKey: form.apiKey,
                agentType: form.agentType,
                model: form.model || null,
              })
              done()
            }, "供应商已创建。")
          }
        }}
      >
        <h3 className="font-medium">{form.id ? "编辑供应商" : "添加供应商"}</h3>
        <div className="grid gap-3 sm:grid-cols-2">
          <label className="block text-sm">
            智能体类型
            <select
              className="mt-1 h-9 w-full rounded-md border bg-background px-2 text-sm"
              value={form.agentType}
              onChange={(e) => setForm({ ...form, agentType: e.target.value })}
            >
              {AGENT_TYPES.map((t) => (
                <option key={t} value={t}>
                  {AGENT_LABELS[t]}
                </option>
              ))}
            </select>
          </label>
          <label className="block text-sm">
            名称
            <Input
              required
              className="mt-1"
              value={form.name}
              onChange={(e) => setForm({ ...form, name: e.target.value })}
              placeholder="例如：我的网关"
            />
          </label>
          <label className="block text-sm">
            API 地址
            <Input
              required
              type="url"
              className="mt-1"
              value={form.apiUrl}
              onChange={(e) => setForm({ ...form, apiUrl: e.target.value })}
              placeholder="https://api.example.com/v1"
            />
          </label>
          <label className="block text-sm">
            模型 ID（可选）
            <Input
              className="mt-1"
              value={form.model}
              onChange={(e) => setForm({ ...form, model: e.target.value })}
              placeholder="模型名称"
            />
          </label>
          <label className="block text-sm sm:col-span-2">
            API 密钥
            <Input
              type="password"
              autoComplete="off"
              required={form.id == null}
              className="mt-1"
              value={form.apiKey}
              onChange={(e) => setForm({ ...form, apiKey: e.target.value })}
              placeholder={form.id != null ? "留空保留原密钥" : "输入 API 密钥"}
            />
          </label>
        </div>
        <div className="flex gap-2">
          <Button disabled={busy} type="submit">
            {form.id != null ? "保存修改" : "添加"}
          </Button>
          {form.id != null && (
            <Button
              type="button"
              variant="outline"
              onClick={() => setForm(empty(form.agentType))}
            >
              取消编辑
            </Button>
          )}
        </div>
      </form>

      {message && (
        <p role="status" className="text-sm">
          {message}
        </p>
      )}
    </div>
  )
}
