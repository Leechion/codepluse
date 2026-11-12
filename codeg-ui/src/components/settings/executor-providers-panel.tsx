"use client"

// 执行端供应商面板：从原「API 供应商」独立页移植，改为挂在「设置 → 智能体」里
// 每个 agent 自己的配置区，所以不再带页面容器与标签切换 —— agent 由调用方给定。
// 数据走 codepluse_provider_* 后端命令，与上游 zcode 的模型设置（ModelProviderSection）无关。
import { useCallback, useEffect, useState } from "react"
import { getTransport } from "@/lib/transport"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"

export type ExecutorProviderAgent = "claude" | "dsh"

type Provider = {
  id: string
  name: string
  agent: ExecutorProviderAgent
  url: string
  model: string
  hasKey: boolean
  source?: string
  ccCurrent?: boolean
}

type State = {
  providers: Provider[]
  active: Record<string, string>
  ccError?: string
}

const emptyForm = (agent: ExecutorProviderAgent) => ({
  id: "",
  agent,
  name: "",
  url: "",
  model: "",
  key: "",
})

export function ExecutorProvidersPanel({
  agent,
}: {
  agent: ExecutorProviderAgent
}) {
  const [data, setData] = useState<State>({ providers: [], active: {} })
  const [form, setForm] = useState(emptyForm(agent))
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState("")

  const load = useCallback(async () => {
    setData(await getTransport().call<State>("codepluse_providers_list"))
  }, [])

  // Switching the selected agent remounts nothing, so reset the draft form
  // whenever the panel is pointed at another harness.
  useEffect(() => {
    setForm(emptyForm(agent))
    setMessage("")
  }, [agent])

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

  // DeepSeek Harness has no locally stored executor providers: it reads the
  // model and gateway config from its own desktop app.
  if (agent === "dsh") {
    return (
      <section className="space-y-1.5 rounded-md border bg-muted/10 p-3">
        <h3 className="text-xs font-medium">执行端供应商</h3>
        <p className="text-2xs text-muted-foreground">
          DeepSeek Harness 使用其桌面应用的模型和供应商配置。请在 DeepSeek
          Harness 的模型设置中修改；新会话会读取最新配置。
        </p>
        {message && (
          <p role="status" className="text-2xs text-muted-foreground">
            {message}
          </p>
        )}
      </section>
    )
  }

  const rows = data.providers.filter((p) => p.agent === agent)

  return (
    <section className="space-y-3 rounded-md border bg-muted/10 p-3">
      <div>
        <h3 className="text-xs font-medium">执行端供应商</h3>
        <p className="mt-1 text-2xs text-muted-foreground">
          为执行端保存多个 Anthropic Messages 兼容供应商，并选择新会话使用的配置。
          已有会话继续使用原配置。API 密钥保存在本机项目数据目录。
        </p>
      </div>

      <div className="space-y-2">
        <div className="flex items-center justify-between gap-3 rounded border p-3">
          <span className="text-xs">
            跟随 CC Switch 当前配置（未安装时使用原有配置）
            {!data.active[agent] && " · 使用中"}
          </span>
          <Button
            size="sm"
            variant="outline"
            disabled={busy || !data.active[agent]}
            onClick={() =>
              void run(
                () =>
                  getTransport().call("codepluse_provider_activate", {
                    agent,
                    id: "",
                  }),
                "已切换，新会话生效。"
              )
            }
          >
            使用
          </Button>
        </div>
        {rows.map((p) => (
          <div
            key={p.id}
            className="flex flex-wrap items-center justify-between gap-3 rounded border p-3"
          >
            <div className="min-w-0">
              <div className="text-xs font-medium">
                {p.name} {p.source === "ccswitch" && "· CC Switch"}{" "}
                {p.ccCurrent && "· CC 当前"}
                {data.active[agent] === p.id && " · 使用中"}
              </div>
              <div className="break-all text-2xs text-muted-foreground">
                {p.url} · {p.model}
              </div>
            </div>
            <div className="flex shrink-0 gap-2">
              <Button
                size="sm"
                variant="outline"
                disabled={
                  busy ||
                  data.active[agent] === p.id ||
                  (p.source === "ccswitch" && !p.hasKey && !p.ccCurrent)
                }
                onClick={() =>
                  void run(
                    () =>
                      getTransport().call("codepluse_provider_activate", {
                        agent,
                        id: p.id,
                      }),
                    "已切换，新会话生效。"
                  )
                }
              >
                使用
              </Button>
              {p.source !== "ccswitch" && (
                <>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setForm({ ...p, key: "" })}
                  >
                    编辑
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={busy}
                    onClick={() => {
                      if (window.confirm(`删除供应商「${p.name}」？`)) {
                        void run(
                          () =>
                            getTransport().call("codepluse_provider_delete", {
                              id: p.id,
                            }),
                          "供应商已删除。"
                        )
                      }
                    }}
                  >
                    删除
                  </Button>
                </>
              )}
            </div>
          </div>
        ))}
      </div>

      <div className="flex items-center gap-3 text-2xs text-muted-foreground">
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={busy}
          onClick={() => void run(load, "已刷新 CC Switch 配置。")}
        >
          刷新 CC Switch
        </Button>
        <span>{data.ccError || "CC Switch 配置在其应用中编辑；更改后可刷新。"}</span>
      </div>

      <form
        className="space-y-3 rounded border p-3"
        onSubmit={(e) => {
          e.preventDefault()
          void run(async () => {
            await getTransport().call("codepluse_provider_save", form)
            setForm(emptyForm(agent))
          }, "供应商已保存。")
        }}
      >
        <h4 className="text-xs font-medium">
          {form.id ? "编辑供应商" : "添加供应商"}
        </h4>
        <label className="block text-2xs">
          名称
          <Input
            required
            value={form.name}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
            placeholder="例如：我的 API"
          />
        </label>
        <label className="block text-2xs">
          API 地址
          <Input
            required
            type="url"
            value={form.url}
            onChange={(e) => setForm({ ...form, url: e.target.value })}
            placeholder="https://api.example.com"
          />
        </label>
        <label className="block text-2xs">
          模型 ID
          <Input
            required
            value={form.model}
            onChange={(e) => setForm({ ...form, model: e.target.value })}
            placeholder="模型名称"
          />
        </label>
        <label className="block text-2xs">
          API 密钥
          <Input
            type="password"
            autoComplete="off"
            required={!form.id}
            value={form.key}
            onChange={(e) => setForm({ ...form, key: e.target.value })}
            placeholder={form.id ? "留空保留原密钥" : "输入 API 密钥"}
          />
        </label>
        <div className="flex gap-2">
          <Button disabled={busy} type="submit" size="sm">
            保存
          </Button>
          {form.id && (
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={() => setForm(emptyForm(agent))}
            >
              取消编辑
            </Button>
          )}
        </div>
      </form>

      {message && (
        <p role="status" className="text-2xs text-muted-foreground">
          {message}
        </p>
      )}
    </section>
  )
}
