"use client"

import { useCallback, useEffect, useMemo, useState } from "react"
import {
  Check,
  CheckCircle2,
  Cloud,
  Copy,
  ExternalLink,
  FolderCheck,
  GitBranch,
  GitFork,
  Github,
  Gitlab,
  Globe,
  Key,
  Loader2,
  Plus,
  RefreshCw,
  ShieldCheck,
  Sliders,
  Trash2,
  User,
  XCircle,
} from "lucide-react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Badge } from "@/components/ui/badge"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Switch } from "@/components/ui/switch"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  detectGit,
  getGitSettings,
  updateGitSettings,
  testGitPath,
  getGitUserConfig,
  setGitUserConfig,
  addGitSafeDirectory,
  getGitSshKeys,
  generateGitSshKey,
  getGitHubAccounts,
  updateGitHubAccounts,
  validateGitHubToken,
  validateGitLabToken,
  validateGiteaToken,
  validateCnbToken,
  getAccountToken,
  deleteAccountToken,
} from "@/lib/api"
import type {
  ForgeProviderId,
  GitDetectResult,
  GitHubAccount,
  GitHubAccountsSettings,
  GitSettings,
  GitSshKey,
  GitUserConfig,
} from "@/lib/types"
import { toErrorMessage } from "@/lib/app-error"
import { useActiveFolder } from "@/contexts/active-folder-context"
import { AddForgeAccountDialog } from "./add-forge-account-dialog"
import { AddGitAccountDialog } from "./add-git-account-dialog"

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function isGitHubAccount(account: GitHubAccount): boolean {
  if (account.provider) return account.provider === "github"
  return account.server_url.toLowerCase().includes("github.com")
}

function isGitLabAccount(account: GitHubAccount): boolean {
  return account.provider === "gitlab"
}

function isGiteaAccount(account: GitHubAccount): boolean {
  return account.provider === "gitea"
}

function isCnbAccount(account: GitHubAccount): boolean {
  return account.provider === "cnb"
}

function validatorFor(
  account: GitHubAccount
): typeof validateGitHubToken | null {
  if (isCnbAccount(account)) return validateCnbToken
  if (isGiteaAccount(account)) return validateGiteaToken
  if (isGitLabAccount(account)) return validateGitLabToken
  if (isGitHubAccount(account)) return validateGitHubToken
  return null
}

function providerOf(account: GitHubAccount): ForgeProviderId {
  if (isCnbAccount(account)) return "cnb"
  if (isGiteaAccount(account)) return "gitea"
  if (isGitLabAccount(account)) return "gitlab"
  return "github"
}

function accountInitial(username: string): string {
  return Array.from(username.trim())[0]?.toUpperCase() ?? "?"
}

// ---------------------------------------------------------------------------
// Shared account row component
// ---------------------------------------------------------------------------

function AccountRow({
  account,
  testingId,
  onTest,
  onRotate,
  onSetDefault,
  onRemove,
  t,
}: {
  account: GitHubAccount
  testingId: string | null
  onTest: (account: GitHubAccount) => void
  onRotate?: (account: GitHubAccount) => void
  onSetDefault: (id: string) => void
  onRemove: (account: GitHubAccount) => void
  t: ReturnType<typeof useTranslations<"VersionControlSettings">>
}) {
  return (
    <div className="flex items-center gap-3 rounded-lg border bg-muted/10 px-3 py-2.5">
      <Avatar>
        {account.avatar_url && (
          <AvatarImage src={account.avatar_url} alt={account.username} />
        )}
        <AvatarFallback className="text-xs font-medium">
          {accountInitial(account.username)}
        </AvatarFallback>
      </Avatar>

      <div className="flex-1 min-w-0 space-y-1">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium truncate">
            {account.username}
          </span>
          {account.is_default && (
            <Badge variant="secondary" className="text-3xs px-1.5 py-0">
              {t("defaultLabel")}
            </Badge>
          )}
        </div>
        <div className="flex items-center gap-2 text-2xs text-muted-foreground">
          <span className="truncate">{account.server_url}</span>
          {account.scopes.length > 0 && (
            <>
              <span>·</span>
              <span className="truncate">{account.scopes.join(", ")}</span>
            </>
          )}
        </div>
      </div>

      <div className="flex items-center gap-1 shrink-0">
        <Button
          size="xs"
          variant="ghost"
          onClick={() => onTest(account)}
          disabled={testingId === account.id}
        >
          {testingId === account.id ? (
            <Loader2 className="h-3 w-3 animate-spin" />
          ) : (
            t("testConnection")
          )}
        </Button>
        {onRotate && (
          <Button size="xs" variant="ghost" onClick={() => onRotate(account)}>
            {t("updateToken")}
          </Button>
        )}
        {!account.is_default && (
          <Button
            size="xs"
            variant="ghost"
            onClick={() => onSetDefault(account.id)}
          >
            {t("setDefault")}
          </Button>
        )}
        <Button
          size="xs"
          variant="ghost"
          className="text-destructive hover:text-destructive"
          onClick={() => onRemove(account)}
        >
          <Trash2 className="h-3 w-3" />
        </Button>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function VersionControlSettings() {
  const t = useTranslations("VersionControlSettings")
  const { activeFolder } = useActiveFolder()

  const [loading, setLoading] = useState(true)
  const [gitInfo, setGitInfo] = useState<GitDetectResult | null>(null)
  const [gitSettings, setGitSettings] = useState<GitSettings>({
    custom_path: null,
  })
  const [customPath, setCustomPath] = useState("")
  const [editingPath, setEditingPath] = useState(false)
  const [savingGit, setSavingGit] = useState(false)

  // Git identity states
  const [userConfig, setUserConfig] = useState<GitUserConfig | null>(null)
  const [globalName, setGlobalName] = useState("")
  const [globalEmail, setGlobalEmail] = useState("")
  const [localName, setLocalName] = useState("")
  const [localEmail, setLocalEmail] = useState("")
  const [savingIdentity, setSavingIdentity] = useState(false)

  // Preferences & Behavior
  const [defaultBranch, setDefaultBranch] = useState("main")
  const [autoCrlf, setAutoCrlf] = useState("false")
  const [autoFetch, setAutoFetch] = useState(false)
  const [fetchInterval, setFetchInterval] = useState(10)
  const [pruneOnFetch, setPruneOnFetch] = useState(true)
  const [savingBehavior, setSavingBehavior] = useState(false)
  const [addingSafeDir, setAddingSafeDir] = useState(false)

  // SSH Keys
  const [sshKeys, setSshKeys] = useState<GitSshKey[]>([])
  const [copiedKeyPath, setCopiedKeyPath] = useState<string | null>(null)
  const [generateSshOpen, setGenerateSshOpen] = useState(false)
  const [generateEmail, setGenerateEmail] = useState("")
  const [generatingSsh, setGeneratingSsh] = useState(false)

  // Accounts
  const [accounts, setAccounts] = useState<GitHubAccountsSettings>({
    accounts: [],
  })
  const [addGitHubOpen, setAddGitHubOpen] = useState(false)
  const [addGitLabOpen, setAddGitLabOpen] = useState(false)
  const [addGiteaOpen, setAddGiteaOpen] = useState(false)
  const [addCnbOpen, setAddCnbOpen] = useState(false)
  const [rotateTarget, setRotateTarget] = useState<GitHubAccount | null>(null)
  const [addGitOpen, setAddGitOpen] = useState(false)
  const [testingAccountId, setTestingAccountId] = useState<string | null>(null)
  const [removeTarget, setRemoveTarget] = useState<GitHubAccount | null>(null)

  // Split accounts
  const accountList = useMemo(() => {
    if (Array.isArray(accounts?.accounts)) return accounts.accounts
    if (Array.isArray(accounts)) return accounts as unknown as GitHubAccount[]
    return []
  }, [accounts])

  const githubAccounts = useMemo(
    () => accountList.filter(isGitHubAccount),
    [accountList]
  )
  const gitlabAccounts = useMemo(
    () => accountList.filter(isGitLabAccount),
    [accountList]
  )
  const giteaAccounts = useMemo(
    () => accountList.filter(isGiteaAccount),
    [accountList]
  )
  const cnbAccounts = useMemo(
    () => accountList.filter(isCnbAccount),
    [accountList]
  )
  const gitAccounts = useMemo(
    () =>
      accountList.filter(
        (a) =>
          !isGitHubAccount(a) &&
          !isGitLabAccount(a) &&
          !isGiteaAccount(a) &&
          !isCnbAccount(a)
      ),
    [accountList]
  )

  const loadData = useCallback(async () => {
    setLoading(true)
    try {
      const folderPath = activeFolder?.path
      const [git, settings, ghAccounts, userCfg, sshData] = await Promise.all([
        detectGit().catch(() => ({ installed: false, path: null, version: null })),
        getGitSettings().catch((): GitSettings => ({ custom_path: null })),
        getGitHubAccounts().catch(() => ({ accounts: [] })),
        getGitUserConfig(folderPath).catch(() => ({
          global_name: null,
          global_email: null,
          default_branch: "main",
          autocrlf: "false",
          local_name: null,
          local_email: null,
          is_repo: false,
          is_safe_directory: true,
        })),
        getGitSshKeys().catch(() => ({ keys: [] })),
      ])
      setGitInfo(git ?? { installed: false, path: null, version: null })
      setGitSettings(settings ?? { custom_path: null })
      setCustomPath(settings?.custom_path ?? "")
      setDefaultBranch(
        settings?.default_branch ?? userCfg?.default_branch ?? "main"
      )
      setAutoCrlf(settings?.auto_crlf ?? userCfg?.autocrlf ?? "false")
      setAutoFetch(settings?.auto_fetch ?? false)
      setFetchInterval(settings?.fetch_interval_minutes ?? 10)
      setPruneOnFetch(settings?.prune_on_fetch ?? true)
      setAccounts(
        Array.isArray(ghAccounts)
          ? { accounts: ghAccounts }
          : ghAccounts && Array.isArray(ghAccounts.accounts)
          ? ghAccounts
          : { accounts: [] }
      )
      setUserConfig(userCfg ?? null)
      setGlobalName(userCfg?.global_name ?? "")
      setGlobalEmail(userCfg?.global_email ?? "")
      setLocalName(userCfg?.local_name ?? "")
      setLocalEmail(userCfg?.local_email ?? "")
      setSshKeys(Array.isArray(sshData?.keys) ? sshData.keys : [])
    } catch (err) {
      const message = toErrorMessage(err)
      toast.error(t("loadFailed", { message }))
    } finally {
      setLoading(false)
    }
  }, [activeFolder?.path, t])

  useEffect(() => {
    loadData().catch(console.error)
  }, [loadData])

  // --- Git path handlers ---

  const handleSaveGit = useCallback(async () => {
    const trimmed = customPath.trim()
    setSavingGit(true)
    try {
      if (trimmed) {
        const result = await testGitPath(trimmed)
        if (!result.installed) {
          toast.error(
            t("testFailed", { message: "not a valid git executable" })
          )
          return
        }
      }
      const updated = await updateGitSettings({
        ...gitSettings,
        custom_path: trimmed || null,
      })
      setGitSettings(updated)
      const git = await detectGit()
      setGitInfo(git)
      setEditingPath(false)
      toast.success(t("saveSuccess"))
    } catch (err) {
      const message = toErrorMessage(err)
      toast.error(t("saveFailed", { message }))
    } finally {
      setSavingGit(false)
    }
  }, [customPath, gitSettings, t])

  const handleCancelEdit = useCallback(() => {
    setEditingPath(false)
    getGitSettings()
      .then((s) => setCustomPath(s.custom_path ?? ""))
      .catch(() => {})
  }, [])

  // --- Identity handlers ---

  const handleSaveGlobalIdentity = useCallback(async () => {
    setSavingIdentity(true)
    try {
      const updated = await setGitUserConfig({
        scope: "global",
        name: globalName.trim() || null,
        email: globalEmail.trim() || null,
      })
      setUserConfig(updated)
      toast.success(t("identitySaved"))
    } catch (err) {
      toast.error(t("saveFailed", { message: toErrorMessage(err) }))
    } finally {
      setSavingIdentity(false)
    }
  }, [globalName, globalEmail, t])

  const handleSaveLocalIdentity = useCallback(async () => {
    if (!activeFolder?.path) return
    setSavingIdentity(true)
    try {
      const updated = await setGitUserConfig({
        scope: "local",
        path: activeFolder.path,
        name: localName.trim() || null,
        email: localEmail.trim() || null,
      })
      setUserConfig(updated)
      toast.success(t("identitySaved"))
    } catch (err) {
      toast.error(t("saveFailed", { message: toErrorMessage(err) }))
    } finally {
      setSavingIdentity(false)
    }
  }, [activeFolder?.path, localName, localEmail, t])

  const handleClearLocalIdentity = useCallback(async () => {
    if (!activeFolder?.path) return
    setSavingIdentity(true)
    try {
      const updated = await setGitUserConfig({
        scope: "local",
        path: activeFolder.path,
        name: null,
        email: null,
      })
      setUserConfig(updated)
      setLocalName("")
      setLocalEmail("")
      toast.success(t("identityCleared"))
    } catch (err) {
      toast.error(t("saveFailed", { message: toErrorMessage(err) }))
    } finally {
      setSavingIdentity(false)
    }
  }, [activeFolder?.path, t])

  // --- Behavior handlers ---

  const handleSaveBehavior = useCallback(async () => {
    setSavingBehavior(true)
    try {
      await setGitUserConfig({
        scope: "global",
        default_branch: defaultBranch.trim() || "main",
        autocrlf: autoCrlf,
      })
      const saved = await updateGitSettings({
        ...gitSettings,
        auto_fetch: autoFetch,
        fetch_interval_minutes: Number(fetchInterval) || 10,
        prune_on_fetch: pruneOnFetch,
        default_branch: defaultBranch.trim() || "main",
        auto_crlf: autoCrlf,
      })
      setGitSettings(saved)
      toast.success(t("identitySaved"))
    } catch (err) {
      toast.error(t("saveFailed", { message: toErrorMessage(err) }))
    } finally {
      setSavingBehavior(false)
    }
  }, [
    autoCrlf,
    autoFetch,
    defaultBranch,
    fetchInterval,
    gitSettings,
    pruneOnFetch,
    t,
  ])

  const handleAddSafeDirectory = useCallback(async () => {
    if (!activeFolder?.path) return
    setAddingSafeDir(true)
    try {
      await addGitSafeDirectory(activeFolder.path)
      const userCfg = await getGitUserConfig(activeFolder.path)
      setUserConfig(userCfg)
      toast.success(t("safeDirectoryAdded"))
    } catch (err) {
      toast.error(t("saveFailed", { message: toErrorMessage(err) }))
    } finally {
      setAddingSafeDir(false)
    }
  }, [activeFolder?.path, t])

  // --- SSH Keys handlers ---

  const handleCopySshKey = useCallback(
    async (key: GitSshKey) => {
      try {
        await navigator.clipboard.writeText(key.public_key)
        setCopiedKeyPath(key.path)
        toast.success(t("publicKeyCopied"))
        setTimeout(() => setCopiedKeyPath(null), 2500)
      } catch {
        toast.error("Failed to copy public key")
      }
    },
    [t]
  )

  const handleGenerateSshKey = useCallback(async () => {
    const email = generateEmail.trim()
    if (!email) return
    setGeneratingSsh(true)
    try {
      const newKey = await generateGitSshKey(email, "ed25519")
      await navigator.clipboard.writeText(newKey.public_key)
      setSshKeys((prev) => [...prev, newKey])
      setGenerateSshOpen(false)
      setGenerateEmail("")
      toast.success(t("sshKeyGenerated"))
    } catch (err) {
      toast.error(t("saveFailed", { message: toErrorMessage(err) }))
    } finally {
      setGeneratingSsh(false)
    }
  }, [generateEmail, t])

  // --- Shared account handlers ---

  const handleAccountAdded = useCallback(
    async (account: GitHubAccount) => {
      const currentList = Array.isArray(accounts?.accounts)
        ? accounts.accounts
        : Array.isArray(accounts)
        ? (accounts as unknown as GitHubAccount[])
        : []
      const known = currentList.some((a) => a.id === account.id)
      const others = currentList.map((a) =>
        account.is_default && a.id !== account.id
          ? { ...a, is_default: false }
          : a
      )
      const updated: GitHubAccountsSettings = {
        accounts: known
          ? others.map((a) => (a.id === account.id ? account : a))
          : [...others, account],
      }
      try {
        const saved = await updateGitHubAccounts(updated)
        setAccounts(saved && Array.isArray(saved.accounts) ? saved : updated)
        toast.success(t("addSuccess", { username: account.username }))
      } catch (err) {
        const message = toErrorMessage(err)
        toast.error(t("addFailed", { message }))
      }
    },
    [accounts, t]
  )

  const handleTestConnection = useCallback(
    async (account: GitHubAccount) => {
      setTestingAccountId(account.id)
      try {
        const token = await getAccountToken(account.id)
        if (!token) {
          toast.error(t("connectionFailed", { message: "Token not found" }))
          return
        }
        const validate = validatorFor(account)
        if (validate) {
          const result = await validate(account.server_url, token)
          if (result.success) {
            toast.success(t("connectionSuccess"))
          } else {
            toast.error(
              t("connectionFailed", {
                message: result.message ?? "Unknown error",
              })
            )
          }
        } else {
          toast.success(t("connectionSuccess"))
        }
      } catch (err) {
        const message = toErrorMessage(err)
        toast.error(t("connectionFailed", { message }))
      } finally {
        setTestingAccountId(null)
      }
    },
    [t]
  )

  const handleSetDefault = useCallback(
    async (accountId: string) => {
      const currentList = Array.isArray(accounts?.accounts)
        ? accounts.accounts
        : Array.isArray(accounts)
        ? (accounts as unknown as GitHubAccount[])
        : []
      const updated: GitHubAccountsSettings = {
        accounts: currentList.map((a) => ({
          ...a,
          is_default: a.id === accountId,
        })),
      }
      try {
        const saved = await updateGitHubAccounts(updated)
        setAccounts(saved && Array.isArray(saved.accounts) ? saved : updated)
        toast.success(t("defaultSet"))
      } catch (err) {
        const message = toErrorMessage(err)
        toast.error(message)
      }
    },
    [accounts, t]
  )

  const handleRemoveAccount = useCallback(async () => {
    if (!removeTarget) return
    const currentList = Array.isArray(accounts?.accounts)
      ? accounts.accounts
      : Array.isArray(accounts)
      ? (accounts as unknown as GitHubAccount[])
      : []
    const updated: GitHubAccountsSettings = {
      accounts: currentList.filter((a) => a.id !== removeTarget.id),
    }
    try {
      await deleteAccountToken(removeTarget.id)
      const saved = await updateGitHubAccounts(updated)
      setAccounts(saved && Array.isArray(saved.accounts) ? saved : updated)
      toast.success(t("removeSuccess"))
    } catch (err) {
      const message = toErrorMessage(err)
      toast.error(message)
    } finally {
      setRemoveTarget(null)
    }
  }, [accounts, removeTarget, t])

  // --- Render ---

  if (loading) {
    return (
      <div className="h-full flex items-center justify-center text-sm text-muted-foreground gap-2">
        <Loader2 className="h-4 w-4 animate-spin" />
        {t("loading")}
      </div>
    )
  }

  return (
    <ScrollArea className="h-full">
      <div className="w-full space-y-6 p-4 md:p-6 max-w-5xl mx-auto">
        {/* Header */}
        <section className="space-y-1">
          <div className="flex items-center gap-2">
            <GitBranch className="h-5 w-5 text-primary" />
            <h1 className="text-base font-semibold">{t("sectionTitle")}</h1>
          </div>
          <p className="text-xs text-muted-foreground">
            {t("sectionDescription")}
          </p>
        </section>

        {/* 1. Git Executable Environment */}
        <section className="rounded-xl border bg-card p-4 space-y-4 shadow-2xs">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <Sliders className="h-4 w-4 text-muted-foreground" />
              <h2 className="text-sm font-semibold">{t("gitTitle")}</h2>
            </div>
            <div className="flex items-center gap-2">
              <Button
                size="xs"
                variant="ghost"
                onClick={loadData}
                disabled={loading}
              >
                <RefreshCw className="h-3 w-3 mr-1" />
                {t("test")}
              </Button>
            </div>
          </div>

          <div className="rounded-lg border bg-muted/20 px-3.5 py-3 text-xs space-y-2">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                {gitInfo?.installed ? (
                  <>
                    <CheckCircle2 className="h-4 w-4 text-emerald-500" />
                    <span className="text-emerald-600 dark:text-emerald-400 font-medium">
                      {t("gitDetected")}
                    </span>
                  </>
                ) : (
                  <>
                    <XCircle className="h-4 w-4 text-rose-500" />
                    <span className="text-rose-600 dark:text-rose-400 font-medium">
                      {t("gitNotFound")}
                    </span>
                  </>
                )}
                {gitInfo?.version && (
                  <Badge variant="outline" className="text-3xs font-mono">
                    {gitInfo.version}
                  </Badge>
                )}
              </div>
              {!editingPath && (
                <Button
                  size="xs"
                  variant="outline"
                  onClick={() => setEditingPath(true)}
                >
                  {t("customGitPath")}
                </Button>
              )}
            </div>
            {gitInfo?.path && (
              <p className="text-muted-foreground font-mono text-2xs truncate">
                {t("gitPath")}: {gitInfo.path}
              </p>
            )}
          </div>

          {editingPath && (
            <div className="space-y-2 pt-1 border-t">
              <label className="text-xs font-medium text-muted-foreground">
                {t("customGitPath")}
              </label>
              <div className="flex gap-2">
                <Input
                  value={customPath}
                  onChange={(e) => setCustomPath(e.target.value)}
                  placeholder={t("customGitPathPlaceholder")}
                  className="flex-1 font-mono text-xs"
                  autoFocus
                />
                <Button
                  size="sm"
                  variant="outline"
                  onClick={handleCancelEdit}
                  disabled={savingGit}
                >
                  {t("removeCancel")}
                </Button>
                <Button size="sm" onClick={handleSaveGit} disabled={savingGit}>
                  {savingGit ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    t("save")
                  )}
                </Button>
              </div>
              <p className="text-2xs text-muted-foreground">
                {t("customGitPathHint")}
              </p>
            </div>
          )}
        </section>

        {/* 2. Git Commit Identity */}
        <section className="rounded-xl border bg-card p-4 space-y-4 shadow-2xs">
          <div className="flex items-center gap-2">
            <User className="h-4 w-4 text-muted-foreground" />
            <h2 className="text-sm font-semibold">{t("identityTitle")}</h2>
          </div>
          <p className="text-xs text-muted-foreground">
            {t("identityDescription")}
          </p>

          <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
            {/* Global identity */}
            <div className="rounded-lg border bg-muted/10 p-3.5 space-y-3">
              <div className="flex items-center justify-between">
                <span className="text-xs font-semibold">
                  {t("globalIdentity")}
                </span>
                <Badge variant="secondary" className="text-3xs">
                  git config --global
                </Badge>
              </div>
              <div className="space-y-2">
                <div>
                  <label className="text-2xs font-medium text-muted-foreground">
                    {t("userName")}
                  </label>
                  <Input
                    value={globalName}
                    onChange={(e) => setGlobalName(e.target.value)}
                    placeholder={t("userNamePlaceholder")}
                    className="h-8 text-xs"
                  />
                </div>
                <div>
                  <label className="text-2xs font-medium text-muted-foreground">
                    {t("userEmail")}
                  </label>
                  <Input
                    value={globalEmail}
                    onChange={(e) => setGlobalEmail(e.target.value)}
                    placeholder={t("userEmailPlaceholder")}
                    className="h-8 text-xs"
                  />
                </div>
              </div>
              <div className="flex justify-end pt-1">
                <Button
                  size="xs"
                  onClick={handleSaveGlobalIdentity}
                  disabled={savingIdentity}
                >
                  {savingIdentity ? (
                    <Loader2 className="h-3 w-3 animate-spin" />
                  ) : (
                    t("saveIdentity")
                  )}
                </Button>
              </div>
            </div>

            {/* Local workspace identity */}
            <div className="rounded-lg border bg-muted/10 p-3.5 space-y-3">
              <div className="flex items-center justify-between">
                <span className="text-xs font-semibold">
                  {t("localIdentity")}
                </span>
                {userConfig?.is_repo ? (
                  <Badge variant="outline" className="text-3xs">
                    git config --local
                  </Badge>
                ) : (
                  <span className="text-2xs text-muted-foreground">
                    {t("localNotRepo")}
                  </span>
                )}
              </div>
              <div className="space-y-2">
                <div>
                  <label className="text-2xs font-medium text-muted-foreground">
                    {t("userName")}
                  </label>
                  <Input
                    value={localName}
                    onChange={(e) => setLocalName(e.target.value)}
                    placeholder={globalName || t("userNamePlaceholder")}
                    className="h-8 text-xs"
                    disabled={!userConfig?.is_repo}
                  />
                </div>
                <div>
                  <label className="text-2xs font-medium text-muted-foreground">
                    {t("userEmail")}
                  </label>
                  <Input
                    value={localEmail}
                    onChange={(e) => setLocalEmail(e.target.value)}
                    placeholder={globalEmail || t("userEmailPlaceholder")}
                    className="h-8 text-xs"
                    disabled={!userConfig?.is_repo}
                  />
                </div>
              </div>
              <div className="flex justify-end gap-2 pt-1">
                {userConfig?.local_name || userConfig?.local_email ? (
                  <Button
                    size="xs"
                    variant="outline"
                    onClick={handleClearLocalIdentity}
                    disabled={savingIdentity || !userConfig?.is_repo}
                  >
                    {t("clearLocalIdentity")}
                  </Button>
                ) : null}
                <Button
                  size="xs"
                  onClick={handleSaveLocalIdentity}
                  disabled={savingIdentity || !userConfig?.is_repo}
                >
                  {savingIdentity ? (
                    <Loader2 className="h-3 w-3 animate-spin" />
                  ) : (
                    t("saveIdentity")
                  )}
                </Button>
              </div>
            </div>
          </div>
        </section>

        {/* 3. Preferences & Behavior */}
        <section className="rounded-xl border bg-card p-4 space-y-4 shadow-2xs">
          <div className="flex items-center gap-2">
            <Sliders className="h-4 w-4 text-muted-foreground" />
            <h2 className="text-sm font-semibold">{t("behaviorTitle")}</h2>
          </div>
          <p className="text-xs text-muted-foreground">
            {t("behaviorDescription")}
          </p>

          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {t("defaultBranch")}
              </label>
              <Input
                value={defaultBranch}
                onChange={(e) => setDefaultBranch(e.target.value)}
                placeholder={t("defaultBranchPlaceholder")}
                className="h-8 text-xs"
              />
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {t("autoCrlf")}
              </label>
              <Select value={autoCrlf} onValueChange={setAutoCrlf}>
                <SelectTrigger className="h-8 text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="false" className="text-xs">
                    {t("autoCrlfFalse")}
                  </SelectItem>
                  <SelectItem value="true" className="text-xs">
                    {t("autoCrlfTrue")}
                  </SelectItem>
                  <SelectItem value="input" className="text-xs">
                    {t("autoCrlfInput")}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>

          <div className="rounded-lg border bg-muted/10 p-3.5 space-y-3">
            <div className="flex items-center justify-between">
              <div className="space-y-0.5">
                <span className="text-xs font-medium">{t("autoFetch")}</span>
                <p className="text-2xs text-muted-foreground">
                  {t("autoFetchHint")}
                </p>
              </div>
              <Switch checked={autoFetch} onCheckedChange={setAutoFetch} />
            </div>

            {autoFetch && (
              <div className="grid grid-cols-1 md:grid-cols-2 gap-3 pt-2 border-t">
                <div className="space-y-1">
                  <label className="text-2xs font-medium text-muted-foreground">
                    {t("fetchInterval")}
                  </label>
                  <Input
                    type="number"
                    min={1}
                    max={120}
                    value={fetchInterval}
                    onChange={(e) =>
                      setFetchInterval(Number(e.target.value) || 10)
                    }
                    className="h-8 text-xs"
                  />
                </div>
                <div className="flex items-center justify-between pt-4">
                  <span className="text-xs text-muted-foreground">
                    {t("pruneOnFetch")}
                  </span>
                  <Switch
                    checked={pruneOnFetch}
                    onCheckedChange={setPruneOnFetch}
                  />
                </div>
              </div>
            )}
          </div>

          {activeFolder?.path && (
            <div className="rounded-lg border border-dashed bg-muted/5 p-3 flex items-center justify-between gap-3">
              <div className="space-y-0.5 min-w-0">
                <div className="flex items-center gap-1.5">
                  <ShieldCheck className="h-3.5 w-3.5 text-emerald-500" />
                  <span className="text-xs font-medium">
                    {t("safeDirectoryTitle")}
                  </span>
                </div>
                <p className="text-2xs text-muted-foreground truncate">
                  {activeFolder.path}
                </p>
              </div>
              <Button
                size="xs"
                variant="outline"
                onClick={handleAddSafeDirectory}
                disabled={addingSafeDir}
              >
                {addingSafeDir ? (
                  <Loader2 className="h-3 w-3 animate-spin" />
                ) : (
                  <>
                    <FolderCheck className="h-3.5 w-3.5 mr-1" />
                    {t("addSafeDirectory")}
                  </>
                )}
              </Button>
            </div>
          )}

          <div className="flex justify-end">
            <Button
              size="sm"
              onClick={handleSaveBehavior}
              disabled={savingBehavior}
            >
              {savingBehavior ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                t("save")
              )}
            </Button>
          </div>
        </section>

        {/* 4. SSH Keys Management */}
        <section className="rounded-xl border bg-card p-4 space-y-4 shadow-2xs">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <Key className="h-4 w-4 text-muted-foreground" />
              <h2 className="text-sm font-semibold">{t("sshTitle")}</h2>
            </div>
            <Button
              size="xs"
              variant="outline"
              onClick={() => {
                setGenerateEmail(globalEmail || "")
                setGenerateSshOpen(true)
              }}
            >
              <Plus className="h-3 w-3 mr-1" />
              {t("generateSshKey")}
            </Button>
          </div>
          <p className="text-xs text-muted-foreground">
            {t("sshDescription")}
          </p>

          {sshKeys.length === 0 ? (
            <div className="rounded-md border border-dashed bg-muted/10 px-4 py-5 text-center text-xs text-muted-foreground">
              {t("noSshKeys")}
            </div>
          ) : (
            <div className="space-y-2">
              {sshKeys.map((key) => {
                const isCopied = copiedKeyPath === key.path
                return (
                  <div
                    key={key.path}
                    className="flex items-center justify-between gap-3 rounded-lg border bg-muted/10 px-3.5 py-2.5"
                  >
                    <div className="space-y-0.5 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="text-xs font-medium font-mono">
                          {key.name}
                        </span>
                        <Badge
                          variant="secondary"
                          className="text-3xs uppercase font-mono"
                        >
                          {key.key_type.replace(/^ssh-/, "")}
                        </Badge>
                        {key.comment && (
                          <span className="text-2xs text-muted-foreground truncate">
                            {key.comment}
                          </span>
                        )}
                      </div>
                      <p className="text-3xs text-muted-foreground font-mono truncate max-w-lg">
                        {key.public_key}
                      </p>
                    </div>
                    <Button
                      size="xs"
                      variant="ghost"
                      onClick={() => handleCopySshKey(key)}
                      className="shrink-0"
                    >
                      {isCopied ? (
                        <>
                          <Check className="h-3.5 w-3.5 text-emerald-500 mr-1" />
                          <span className="text-emerald-500">
                            {t("publicKeyCopied")}
                          </span>
                        </>
                      ) : (
                        <>
                          <Copy className="h-3.5 w-3.5 mr-1" />
                          {t("copyPublicKey")}
                        </>
                      )}
                    </Button>
                  </div>
                )
              })}
            </div>
          )}
        </section>

        {/* 5. GitHub Accounts */}
        <section className="rounded-xl border bg-card p-4 space-y-4 shadow-2xs">
          <div className="flex items-center gap-2">
            <Github className="h-4 w-4 text-muted-foreground" />
            <h2 className="text-sm font-semibold">{t("githubTitle")}</h2>
          </div>
          <p className="text-xs text-muted-foreground leading-5">
            {t("githubDescription")}
          </p>

          {githubAccounts.length === 0 ? (
            <div className="rounded-md border border-dashed bg-muted/10 px-4 py-6 text-center text-xs text-muted-foreground">
              {t("noAccounts")}
            </div>
          ) : (
            <div className="space-y-2">
              {githubAccounts.map((account) => (
                <AccountRow
                  key={account.id}
                  account={account}
                  testingId={testingAccountId}
                  onTest={handleTestConnection}
                  onRotate={setRotateTarget}
                  onSetDefault={handleSetDefault}
                  onRemove={setRemoveTarget}
                  t={t}
                />
              ))}
            </div>
          )}

          <div className="flex justify-end">
            <Button size="sm" onClick={() => setAddGitHubOpen(true)}>
              {t("addAccount")}
            </Button>
          </div>
        </section>

        {/* 6. GitLab Accounts */}
        <section className="rounded-xl border bg-card p-4 space-y-4 shadow-2xs">
          <div className="flex items-center gap-2">
            <Gitlab className="h-4 w-4 text-muted-foreground" />
            <h2 className="text-sm font-semibold">{t("gitlabTitle")}</h2>
          </div>
          <p className="text-xs text-muted-foreground leading-5">
            {t("gitlabDescription")}
          </p>

          {gitlabAccounts.length === 0 ? (
            <div className="rounded-md border border-dashed bg-muted/10 px-4 py-6 text-center text-xs text-muted-foreground">
              {t("gitlabNoAccounts")}
            </div>
          ) : (
            <div className="space-y-2">
              {gitlabAccounts.map((account) => (
                <AccountRow
                  key={account.id}
                  account={account}
                  testingId={testingAccountId}
                  onTest={handleTestConnection}
                  onRotate={setRotateTarget}
                  onSetDefault={handleSetDefault}
                  onRemove={setRemoveTarget}
                  t={t}
                />
              ))}
            </div>
          )}

          <div className="flex justify-end">
            <Button size="sm" onClick={() => setAddGitLabOpen(true)}>
              {t("addAccount")}
            </Button>
          </div>
        </section>

        {/* 7. Gitea Accounts */}
        <section className="rounded-xl border bg-card p-4 space-y-4 shadow-2xs">
          <div className="flex items-center gap-2">
            <GitFork className="h-4 w-4 text-muted-foreground" />
            <h2 className="text-sm font-semibold">{t("giteaTitle")}</h2>
          </div>
          <p className="text-xs text-muted-foreground leading-5">
            {t("giteaDescription")}
          </p>

          {giteaAccounts.length === 0 ? (
            <div className="rounded-md border border-dashed bg-muted/10 px-4 py-6 text-center text-xs text-muted-foreground">
              {t("giteaNoAccounts")}
            </div>
          ) : (
            <div className="space-y-2">
              {giteaAccounts.map((account) => (
                <AccountRow
                  key={account.id}
                  account={account}
                  testingId={testingAccountId}
                  onTest={handleTestConnection}
                  onRotate={setRotateTarget}
                  onSetDefault={handleSetDefault}
                  onRemove={setRemoveTarget}
                  t={t}
                />
              ))}
            </div>
          )}

          <div className="flex justify-end">
            <Button size="sm" onClick={() => setAddGiteaOpen(true)}>
              {t("addAccount")}
            </Button>
          </div>
        </section>

        {/* 8. CNB Accounts */}
        <section className="rounded-xl border bg-card p-4 space-y-4 shadow-2xs">
          <div className="flex items-center gap-2">
            <Cloud className="h-4 w-4 text-muted-foreground" />
            <h2 className="text-sm font-semibold">{t("cnbTitle")}</h2>
          </div>
          <p className="text-xs text-muted-foreground leading-5">
            {t("cnbDescription")}
          </p>

          {cnbAccounts.length === 0 ? (
            <div className="rounded-md border border-dashed bg-muted/10 px-4 py-6 text-center text-xs text-muted-foreground">
              {t("cnbNoAccounts")}
            </div>
          ) : (
            <div className="space-y-2">
              {cnbAccounts.map((account) => (
                <AccountRow
                  key={account.id}
                  account={account}
                  testingId={testingAccountId}
                  onTest={handleTestConnection}
                  onRotate={setRotateTarget}
                  onSetDefault={handleSetDefault}
                  onRemove={setRemoveTarget}
                  t={t}
                />
              ))}
            </div>
          )}

          <div className="flex justify-end">
            <Button size="sm" onClick={() => setAddCnbOpen(true)}>
              {t("addAccount")}
            </Button>
          </div>
        </section>

        {/* 9. Plain Git Accounts */}
        <section className="rounded-xl border bg-card p-4 space-y-4 shadow-2xs">
          <div className="flex items-center gap-2">
            <Globe className="h-4 w-4 text-muted-foreground" />
            <h2 className="text-sm font-semibold">
              {t("gitAccount.sectionTitle")}
            </h2>
          </div>
          <p className="text-xs text-muted-foreground leading-5">
            {t("gitAccount.sectionDescription")}
          </p>

          {gitAccounts.length === 0 ? (
            <div className="rounded-md border border-dashed bg-muted/10 px-4 py-6 text-center text-xs text-muted-foreground">
              {t("gitAccount.noAccounts")}
            </div>
          ) : (
            <div className="space-y-2">
              {gitAccounts.map((account) => (
                <AccountRow
                  key={account.id}
                  account={account}
                  testingId={testingAccountId}
                  onTest={handleTestConnection}
                  onSetDefault={handleSetDefault}
                  onRemove={setRemoveTarget}
                  t={t}
                />
              ))}
            </div>
          )}

          <div className="flex justify-end">
            <Button size="sm" onClick={() => setAddGitOpen(true)}>
              {t("gitAccount.addAccount")}
            </Button>
          </div>
        </section>
      </div>

      {/* Dialogs */}
      <AddForgeAccountDialog
        provider="github"
        open={addGitHubOpen}
        onOpenChange={setAddGitHubOpen}
        onAccountAdded={handleAccountAdded}
        isFirstAccount={accountList.length === 0}
      />
      <AddForgeAccountDialog
        provider="gitlab"
        open={addGitLabOpen}
        onOpenChange={setAddGitLabOpen}
        onAccountAdded={handleAccountAdded}
        isFirstAccount={accountList.length === 0}
      />
      <AddForgeAccountDialog
        provider="gitea"
        open={addGiteaOpen}
        onOpenChange={setAddGiteaOpen}
        onAccountAdded={handleAccountAdded}
        isFirstAccount={accountList.length === 0}
      />
      <AddForgeAccountDialog
        provider="cnb"
        open={addCnbOpen}
        onOpenChange={setAddCnbOpen}
        onAccountAdded={handleAccountAdded}
        isFirstAccount={accountList.length === 0}
      />
      {rotateTarget && (
        <AddForgeAccountDialog
          key={rotateTarget.id}
          provider={providerOf(rotateTarget)}
          existing={rotateTarget}
          open
          onOpenChange={(open) => !open && setRotateTarget(null)}
          onAccountAdded={handleAccountAdded}
          isFirstAccount={false}
        />
      )}
      <AddGitAccountDialog
        open={addGitOpen}
        onOpenChange={setAddGitOpen}
        onAccountAdded={handleAccountAdded}
        isFirstAccount={accountList.length === 0}
      />

      {/* Generate SSH Key Dialog */}
      <Dialog open={generateSshOpen} onOpenChange={setGenerateSshOpen}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t("generateSshKeyDialogTitle")}</DialogTitle>
            <DialogDescription>
              {t("generateSshKeyDialogDescription")}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-3 py-2">
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {t("generateSshKeyEmail")}
              </label>
              <Input
                value={generateEmail}
                onChange={(e) => setGenerateEmail(e.target.value)}
                placeholder="your.email@example.com"
                className="text-xs"
                autoFocus
              />
            </div>
          </div>
          <DialogFooter>
            <Button
              size="sm"
              onClick={handleGenerateSshKey}
              disabled={generatingSsh || !generateEmail.trim()}
            >
              {generatingSsh ? (
                <>
                  <Loader2 className="h-3.5 w-3.5 animate-spin mr-1" />
                  {t("loading")}
                </>
              ) : (
                t("generateSshKeySubmit")
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Remove Confirmation */}
      <AlertDialog
        open={!!removeTarget}
        onOpenChange={(open) => !open && setRemoveTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("removeConfirmTitle")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("removeConfirmMessage", {
                username: removeTarget?.username ?? "",
              })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("removeCancel")}</AlertDialogCancel>
            <AlertDialogAction onClick={handleRemoveAccount}>
              {t("removeConfirm")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </ScrollArea>
  )
}
