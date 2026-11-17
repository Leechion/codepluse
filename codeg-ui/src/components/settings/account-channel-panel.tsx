"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { ExternalLink, Loader2, LogOut } from "lucide-react"
import { toast } from "sonner"
import { Button } from "@/components/ui/button"
import {
  cancelAccountChannelLogin,
  getAccountChannelLoginStatus,
  listAccountChannels,
  logoutAccountChannel,
  startAccountChannelLogin,
  type AccountChannel,
  type AccountChannelAccount,
  type AccountChannelLogin,
} from "@/lib/api"
import { openExternalUrl } from "@/lib/open-external"
import { toErrorMessage } from "@/lib/app-error"

/** 登录在浏览器里完成，这里只轮询状态。 */
const POLL_INTERVAL = 2000

/**
 * 独立账号（WorkBuddy / Qoder）的官网账号管理。
 *
 * 登录本身发生在产品自己的网页上：后端把登录地址交给系统浏览器，用户完成
 * 授权后协议进程拿到令牌，这里只负责发起、轮询与取消。凭据不经过前端。
 */
export function AccountChannelPanel({
  channelId,
  onChanged,
  onPrimary,
}: {
  /** 渠道 id（buddy / workbuddy / qoder / qodercn）。 */
  channelId: string
  /** 登录状态变化后通知外层刷新连接目录。 */
  onChanged?: () => void
  /** 把当前主账号（头像/用户名/积分）交给外层卡片展示。 */
  onPrimary?: (account: AccountChannelAccount | null) => void
}) {
  const t = useTranslations("Connections")
  const [channel, setChannel] = useState<AccountChannel | null>(null)
  const [login, setLogin] = useState<AccountChannelLogin | null>(null)
  const [busy, setBusy] = useState(false)
  const loginIdRef = useRef<string | null>(null)

  const load = useCallback(async () => {
    try {
      const rows = await listAccountChannels()
      const next = rows.find((row) => row.id === channelId) ?? null
      setChannel(next)
      // 优先用启用中的账号做「当前账号」。
      const accounts = next?.accounts ?? []
      onPrimary?.(accounts.find((account) => account.enabled) ?? accounts[0] ?? null)
    } catch (error) {
      toast.error(toErrorMessage(error))
    }
  }, [channelId, onPrimary])

  useEffect(() => {
    void load()
  }, [load])

  const finish = useCallback(
    async (state: string, error?: string) => {
      loginIdRef.current = null
      setLogin(null)
      if (state === "complete") {
        toast.success(t("accountLoginDone"))
        await load()
        onChanged?.()
        return
      }
      if (state === "cancelled") {
        toast.info(t("accountLoginCancelled"))
        return
      }
      toast.error(t("accountLoginFailed", { message: error || state }))
    },
    [load, onChanged, t]
  )

  const poll = useCallback(async () => {
    const id = loginIdRef.current
    if (!id) return
    try {
      const next = await getAccountChannelLoginStatus(id)
      if (next.state !== "pending") {
        await finish(next.state, next.error)
      }
    } catch (error) {
      // 登录流程可能已经过期（后端重启等）：停止轮询并提示。
      await finish("failed", toErrorMessage(error))
    }
  }, [finish])

  useEffect(() => {
    if (!loginIdRef.current) return
    const timer = setInterval(() => void poll(), POLL_INTERVAL)
    return () => clearInterval(timer)
  }, [poll, login])

  const startLogin = useCallback(async () => {
    setBusy(true)
    try {
      const started = await startAccountChannelLogin(channelId)
      loginIdRef.current = started.id
      setLogin(started)
      if (started.url) {
        await openExternalUrl(started.url)
      } else if (started.state !== "pending") {
        await finish(started.state, started.error)
      }
    } catch (error) {
      toast.error(toErrorMessage(error))
    } finally {
      setBusy(false)
    }
  }, [channelId, finish])

  const cancelLogin = useCallback(async () => {
    const id = loginIdRef.current
    if (!id) return
    try {
      const cancelled = await cancelAccountChannelLogin(id)
      await finish(cancelled.state === "pending" ? "cancelled" : cancelled.state, cancelled.error)
    } catch (error) {
      toast.error(toErrorMessage(error))
    }
  }, [finish])

  const logout = useCallback(
    async (accountId: string) => {
      setBusy(true)
      try {
        await logoutAccountChannel(channelId, accountId)
        toast.success(t("accountLoggedOut"))
        await load()
        onChanged?.()
      } catch (error) {
        toast.error(toErrorMessage(error))
      } finally {
        setBusy(false)
      }
    },
    [channelId, load, onChanged, t]
  )

  const pending = login?.state === "pending"
  const accounts = channel?.accounts ?? []

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <Button
          size="sm"
          disabled={busy || pending}
          onClick={() => void startLogin()}
        >
          {pending ? (
            <Loader2 className="mr-1.5 size-3.5 animate-spin" />
          ) : (
            <ExternalLink className="mr-1.5 size-3.5" />
          )}
          {t("manageAccount")}
        </Button>
        {pending && (
          <Button size="sm" variant="ghost" onClick={() => void cancelLogin()}>
            {t("cancelAccountLogin")}
          </Button>
        )}
      </div>
      {pending && (
        <p className="text-xs leading-relaxed text-muted-foreground">
          {t("accountLoginWaiting")}
        </p>
      )}
      {accounts.length > 0 && (
        <ul className="space-y-1.5">
          {accounts.map((account) => {
            // 用户名优先用上游资料接口给的真实昵称，其次才是通道索引里的值。
            const name =
              account.username || account.nickname || account.name || account.id
            return (
              <li
                key={account.id ?? account.nickname ?? account.name}
                className="flex items-center justify-between gap-3 rounded-lg border px-3 py-2"
              >
                <div className="flex min-w-0 items-center gap-2.5">
                  <AccountAvatar account={account} name={name} />
                  <div className="min-w-0">
                    <p className="truncate text-xs font-medium">{name}</p>
                    {account.id && (
                      <p className="mt-0.5 truncate font-mono text-[10px] text-muted-foreground">
                        {account.id}
                      </p>
                    )}
                  </div>
                </div>
                <div className="flex shrink-0 items-center gap-2">
                  {typeof account.credits === "number" && (
                    <span className="rounded-full border bg-background/40 px-2 py-0.5 text-[10px] text-muted-foreground">
                      {t("credits", { value: account.credits })}
                    </span>
                  )}
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    aria-label={t("logoutAccount")}
                    disabled={busy || !account.id}
                    onClick={() => void logout(account.id!)}
                  >
                    <LogOut className="size-3.5" />
                  </Button>
                </div>
              </li>
            )
          })}
        </ul>
      )}
      <p className="text-xs leading-relaxed text-muted-foreground">
        {t("manageAccountHint")}
      </p>
    </div>
  )
}

/**
 * 账号头像：上游给的是带签名的对象存储地址，会过期或加载失败，
 * 所以失败就退回首字母圆标，绝不显示破图。
 */
function AccountAvatar({
  account,
  name,
}: {
  account: AccountChannelAccount
  name?: string
}) {
  const [broken, setBroken] = useState(false)
  if (account.avatarUrl && !broken) {
    return (
      // eslint-disable-next-line @next/next/no-img-element
      <img
        src={account.avatarUrl}
        alt=""
        referrerPolicy="no-referrer"
        onError={() => setBroken(true)}
        className="size-8 shrink-0 rounded-full border object-cover"
      />
    )
  }
  const initial = (name ?? "?").trim().slice(0, 1) || "?"
  return (
    <span className="flex size-8 shrink-0 items-center justify-center rounded-full bg-primary/10 text-xs font-medium">
      {initial.toUpperCase()}
    </span>
  )
}
