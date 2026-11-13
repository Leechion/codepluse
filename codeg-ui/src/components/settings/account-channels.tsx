"use client"

import { useCallback, useEffect, useState } from "react"
import { ExternalLink, Loader2, LogIn, LogOut, RefreshCw } from "lucide-react"
import { toast } from "sonner"
import { getTransport } from "@/lib/transport"
import { goDesktopBridge } from "@/lib/go-desktop"
import { toErrorMessage } from "@/lib/app-error"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"

type Channel = {
  id: string
  name: string
  configured: boolean
  accounts: { id: string; nickname: string; enabled: boolean }[]
  models: { id: string; name: string }[]
}
type Login = {
  id: string
  provider: string
  url: string
  state: string
  error?: string
}

export function AccountChannels({
  onChanged,
  provider,
}: {
  onChanged: () => void
  provider?: string
}) {
  const [open, setOpen] = useState(false)
  const [rows, setRows] = useState<Channel[]>([])
  const [login, setLogin] = useState<Login | null>(null)
  const [busy, setBusy] = useState("")
  const [error, setError] = useState("")
  const load = useCallback(async () => {
    setRows(
      await getTransport().call<Channel[]>("codepluse_channel_status", {})
    )
  }, [])
  useEffect(() => {
    if (open) void load().catch((error) => setError(toErrorMessage(error)))
  }, [open, load])
  useEffect(() => {
    if (!open || !login || login.state !== "pending") return
    let stopped = false
    const poll = setInterval(() => {
      void getTransport()
        .call<Login>("codepluse_channel_login_status", { id: login.id })
        .then(async (result) => {
          if (stopped) return
          setLogin(result)
          if (result.state === "complete") {
            toast.success("账号已接入，模型目录已加载")
            await load()
            onChanged()
          }
        })
        .catch((error) => {
          if (!stopped) {
            setError(toErrorMessage(error))
            setLogin(null)
          }
        })
    }, 1500)
    return () => {
      stopped = true
      clearInterval(poll)
    }
  }, [open, login, load, onChanged])
  const start = async (provider: string) => {
    setBusy(provider)
    setError("")
    try {
      const result = await getTransport().call<Login>(
        "codepluse_channel_login",
        { provider }
      )
      setLogin(result)
      if (result.state === "failed")
        throw new Error(result.error || "无法开始登录")
      const native = goDesktopBridge()
      if (result.url && native?.openExternal)
        await native.openExternal(result.url)
    } catch (error) {
      setError(toErrorMessage(error))
    } finally {
      setBusy("")
    }
  }
  return (
    <Dialog
      open={open}
      onOpenChange={(value) => {
        setOpen(value)
        if (!value && login?.state === "pending") {
          void getTransport()
            .call("codepluse_channel_cancel", { id: login.id })
            .catch(() => {})
          setLogin(null)
        }
      }}
    >
      <DialogTrigger asChild>
        <Button variant="outline" size="sm">
          <LogIn className="size-3.5" />
          管理官网账号
        </Button>
      </DialogTrigger>
      <DialogContent className="max-h-[85dvh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>
            {rows.find((row) => row.id === provider)?.name ||
              "WorkBuddy · Qoder"}{" "}
            官网账号
          </DialogTitle>
        </DialogHeader>
        <p className="text-sm leading-relaxed text-muted-foreground">
          每个产品都是独立智能体，拥有自己的模型、会话和工具执行循环。登录后可将其设为主智能体，或由其他智能体委派任务；各渠道的账号与令牌单独保存。
        </p>
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}
        <div className={provider ? "grid gap-3" : "grid gap-3 sm:grid-cols-2"}>
          {rows
            .filter((row) => !provider || row.id === provider)
            .map((row) => (
              <section
                key={row.id}
                className="cp-account-card space-y-3 rounded-2xl border p-4"
              >
                <div className="flex items-center justify-between gap-2">
                  <h3 className="font-medium">{row.name}</h3>
                  <span className="text-xs text-muted-foreground">
                    {row.models.length} 个模型
                  </span>
                </div>
                {row.accounts.length === 0 && (
                  <p className="text-xs text-muted-foreground">尚未登录</p>
                )}
                {row.accounts.map((account) => (
                  <div
                    key={account.id}
                    className="flex items-center gap-2 text-xs"
                  >
                    <span className="size-1.5 rounded-full bg-emerald-500" />
                    <span className="min-w-0 flex-1 truncate">
                      {account.nickname}
                    </span>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={`退出 ${row.name} 账号`}
                      onClick={() => {
                        void getTransport()
                          .call("codepluse_channel_logout", {
                            provider: row.id,
                            accountId: account.id,
                          })
                          .then(async () => {
                            await load()
                            onChanged()
                          })
                          .catch((error) => setError(toErrorMessage(error)))
                      }}
                    >
                      <LogOut className="size-3.5" />
                    </Button>
                  </div>
                ))}
                <Button
                  size="sm"
                  variant="outline"
                  disabled={!!busy || login?.state === "pending"}
                  onClick={() => void start(row.id)}
                >
                  {busy === row.id ? (
                    <Loader2 className="size-3.5 animate-spin" />
                  ) : (
                    <LogIn className="size-3.5" />
                  )}
                  {row.configured ? "添加账号" : "网页登录"}
                </Button>
              </section>
            ))}
        </div>
        {login?.state === "pending" && (
          <div className="space-y-2 rounded-xl border p-4">
            <p className="flex items-center gap-2 text-sm">
              <Loader2 className="size-4 animate-spin" />
              等待完成网页登录…
            </p>
            <Button asChild size="sm" variant="outline">
              <a
                href={login.url}
                target="_blank"
                rel="noreferrer"
                onClick={(event) => {
                  const native = goDesktopBridge()
                  if (native?.openExternal) {
                    event.preventDefault()
                    void native.openExternal(login.url)
                  }
                }}
              >
                <ExternalLink className="size-3.5" />
                打开登录网页
              </a>
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={() => {
                void getTransport().call("codepluse_channel_cancel", {
                  id: login.id,
                })
                setLogin(null)
              }}
            >
              取消
            </Button>
          </div>
        )}
        {login?.state === "failed" && (
          <p role="alert" className="text-sm text-destructive">
            {login.error || "登录失败，请重新登录"}
          </p>
        )}
        <Button
          variant="ghost"
          size="sm"
          onClick={() =>
            void load().catch((error) => setError(toErrorMessage(error)))
          }
        >
          <RefreshCw className="size-3.5" />
          刷新账号与模型
        </Button>
      </DialogContent>
    </Dialog>
  )
}
