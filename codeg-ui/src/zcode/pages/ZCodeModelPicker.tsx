"use client"

/**
 * ZCodeModelPicker —— 复刻页的模型选择控件。
 *
 * ZCode 原生 composer 的 ModelConfigSelect 依赖桌面 Host 的
 * ModelSelectionService（buildRegistryModelSelectGroups 只认 ZCode Agent
 * provider），在 codepluse 的 web/Go 部署里该服务是断连桩，modelSelectGroups
 * 恒为空，模型按钮被整块隐藏。这里按 ZCode DESIGN.md 的工具栏密度补一个
 * 等价控件，数据源改为 codepluse 后端的 ACP config_options。
 */
import { useEffect, useRef, useState } from "react"
import { Check, ChevronDown, Cpu, Loader2 } from "lucide-react"

export interface ZCodeModelOption {
  value: string
  name: string
  description?: string
}

interface ZCodeModelPickerProps {
  models: readonly ZCodeModelOption[]
  current: string
  loading: boolean
  disabled?: boolean
  /** 列表为空且用户点开下拉时，向后端懒加载该执行端的模型目录。 */
  onRequestOptions?: () => void
  onSelect: (value: string) => void
}

export function ZCodeModelPicker({
  models,
  current,
  loading,
  disabled,
  onRequestOptions,
  onSelect,
}: ZCodeModelPickerProps) {
  const [open, setOpen] = useState(false)
  const rootRef = useRef<HTMLDivElement | null>(null)

  useEffect(() => {
    if (!open) return
    const onPointerDown = (event: PointerEvent) => {
      if (rootRef.current && !rootRef.current.contains(event.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener("pointerdown", onPointerDown)
    return () => document.removeEventListener("pointerdown", onPointerDown)
  }, [open])

  const currentOption = models.find((model) => model.value === current)
  const label = currentOption?.name || current || "选择模型"

  return (
    <div className="zw-model" ref={rootRef}>
      <button
        type="button"
        className="zw-model-trigger"
        data-open={open}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => {
          if (!models.length && !loading) onRequestOptions?.()
          setOpen((value) => !value)
        }}
      >
        {loading ? <Loader2 size={12} className="zw-model-spin" /> : <Cpu size={12} />}
        <span className="zw-model-label">{loading ? "读取模型目录…" : label}</span>
        <ChevronDown size={12} className="zw-model-chevron" data-open={open} />
      </button>

      {open ? (
        <div className="zw-model-menu" role="listbox" aria-label="模型">
          <div className="zw-model-menu-head">模型</div>
          {loading ? (
            <div className="zw-model-hint">正在读取执行端模型列表…</div>
          ) : models.length === 0 ? (
            <div className="zw-model-hint">暂无可选模型，关闭后重新点开可重试</div>
          ) : (
            models.map((model) => (
              <button
                type="button"
                key={model.value}
                role="option"
                aria-selected={model.value === current}
                className="zw-model-option"
                data-active={model.value === current}
                onClick={() => {
                  onSelect(model.value)
                  setOpen(false)
                }}
              >
                <span className="zw-model-option-body">
                  <span className="zw-model-option-name">{model.name}</span>
                  {model.description ? (
                    <span className="zw-model-option-desc">{model.description}</span>
                  ) : null}
                </span>
                {model.value === current ? <Check size={13} /> : null}
              </button>
            ))
          )}
        </div>
      ) : null}
    </div>
  )
}

export default ZCodeModelPicker
