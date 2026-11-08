"use client"

import { useCallback, useEffect, useState } from "react"
import { getTransport } from "@/lib/transport"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"

type Agent = "claude" | "dsh"
type Provider = { id: string; name: string; agent: Agent; url: string; model: string; hasKey: boolean; source?: string; ccCurrent?: boolean }
type State = { providers: Provider[]; active: Record<string, string>; ccError?: string }
const empty = (agent: Agent) => ({ id: "", agent, name: "", url: "", model: "", key: "" })

export function ProjectBootWorkspace() {
  const [data, setData] = useState<State>({ providers: [], active: {} })
  const [agent, setAgent] = useState<Agent>("claude")
  const [form, setForm] = useState(empty("claude"))
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState("")
  const load = useCallback(async () => {
    setData(await getTransport().call<State>("codepluse_providers_list"))
  }, [])
  useEffect(() => { void load().catch(e => setMessage(String(e))) }, [load])
  async function run(action: () => Promise<unknown>, success: string) {
    setBusy(true); setMessage("")
    try { await action(); await load(); setMessage(success) }
    catch (e) { setMessage(e instanceof Error ? e.message : String(e)) }
    finally { setBusy(false) }
  }
  function selectAgent(value: Agent) { setAgent(value); setForm(empty(value)); setMessage("") }
  return (
    <div className="h-full overflow-auto p-6">
      <div className="mx-auto flex max-w-3xl flex-col gap-5">
        <div><h1 className="text-xl font-semibold">API 供应商</h1><p className="mt-1 text-sm text-muted-foreground">为执行端保存多个供应商，并选择新会话使用的配置。已有会话继续使用原配置。</p></div>
        <div className="flex gap-2"><Button variant={agent === "claude" ? "default" : "outline"} onClick={() => selectAgent("claude")}>Claude Code</Button><Button variant={agent === "dsh" ? "default" : "outline"} onClick={() => selectAgent("dsh")}>DeepSeek Harness</Button></div>
        {agent === "dsh" ? <section className="rounded-lg border p-4 text-sm text-muted-foreground">DeepSeek Harness 使用其桌面应用的模型和供应商配置。请在 DeepSeek Harness 的模型设置中修改；新会话会读取最新配置。</section> : <>
        <p className="text-sm text-muted-foreground">使用 Anthropic Messages 兼容接口。API 密钥保存在本机项目数据目录。</p>
        <section className="rounded-lg border p-4"><h2 className="mb-3 font-medium">当前供应商</h2><div className="space-y-2">
          <div className="flex items-center justify-between gap-3 rounded border p-3"><span>{agent === "claude" ? "跟随 CC Switch 当前配置（未安装时使用原有配置）" : "原有配置"} { !data.active[agent] && "· 使用中" }</span><Button size="sm" variant="outline" disabled={busy || !data.active[agent]} onClick={() => void run(() => getTransport().call("codepluse_provider_activate", { agent, id: "" }), "已切换，新会话生效。")}>使用</Button></div>
          {data.providers.filter(p => p.agent === agent).map(p => <div key={p.id} className="flex flex-wrap items-center justify-between gap-3 rounded border p-3"><div><div className="font-medium">{p.name} {p.source === "ccswitch" && "· CC Switch"} {p.ccCurrent && "· CC 当前"} {data.active[agent] === p.id && "· 使用中"}</div><div className="break-all text-xs text-muted-foreground">{p.url} · {p.model}</div></div><div className="flex gap-2"><Button size="sm" variant="outline" disabled={busy || data.active[agent] === p.id || (p.source === "ccswitch" && !p.hasKey && !p.ccCurrent)} onClick={() => void run(() => getTransport().call("codepluse_provider_activate", {agent, id:p.id}), "已切换，新会话生效。")}>使用</Button>{p.source !== "ccswitch" && <><Button size="sm" variant="outline" onClick={() => setForm({ ...p, key: "" })}>编辑</Button><Button size="sm" variant="outline" disabled={busy} onClick={() => { if (window.confirm(`删除供应商「${p.name}」？`)) void run(() => getTransport().call("codepluse_provider_delete", {id:p.id}), "供应商已删除。") }}>删除</Button></>}</div></div>)}
        </div>{agent === "claude" && <div className="mt-3 flex items-center gap-3 text-sm text-muted-foreground"><Button type="button" size="sm" variant="outline" disabled={busy} onClick={() => void run(load, "已刷新 CC Switch 配置。")}>刷新 CC Switch</Button><span>{data.ccError || "CC Switch 配置在其应用中编辑；更改后可刷新。"}</span></div>}</section>
        <form className="space-y-3 rounded-lg border p-4" onSubmit={e => {e.preventDefault(); void run(async () => { await getTransport().call("codepluse_provider_save", form); setForm(empty(agent)) }, "供应商已保存。")}}>
          <h2 className="font-medium">{form.id ? "编辑供应商" : "添加供应商"}</h2>
          <label className="block text-sm">名称<Input required value={form.name} onChange={e => setForm({...form,name:e.target.value})} placeholder="例如：我的 API" /></label>
          <label className="block text-sm">API 地址<Input required type="url" value={form.url} onChange={e => setForm({...form,url:e.target.value})} placeholder={agent === "claude" ? "https://api.example.com" : "https://api.example.com/v1"} /></label>
          <label className="block text-sm">模型 ID<Input required value={form.model} onChange={e => setForm({...form,model:e.target.value})} placeholder="模型名称" /></label>
          <label className="block text-sm">API 密钥<Input type="password" autoComplete="off" required={!form.id} value={form.key} onChange={e => setForm({...form,key:e.target.value})} placeholder={form.id ? "留空保留原密钥" : "输入 API 密钥"} /></label>
          <div className="flex gap-2"><Button disabled={busy} type="submit">保存</Button>{form.id && <Button type="button" variant="outline" onClick={() => setForm(empty(agent))}>取消编辑</Button>}</div>
        </form>
        </>}
        {message && <p role="status" className="text-sm">{message}</p>}
      </div>
    </div>
  )
}
