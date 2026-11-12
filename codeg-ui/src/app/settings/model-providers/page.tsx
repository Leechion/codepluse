"use client"

// 旧「模型供应商」独立入口：已并入「智能体」设置页的供应商折叠区。
import { useEffect } from "react"
import { useRouter } from "next/navigation"

export default function SettingsModelProvidersPage() {
  const router = useRouter()
  useEffect(() => {
    router.replace("/settings/agents")
  }, [router])
  return null
}
