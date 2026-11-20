"use client"

// Adapted from ZCode ComposerModelMenu.tsx, Apache-2.0; see THIRD_PARTY_NOTICES.md.
import { useState } from "react"
import type {
  SessionChoiceSetting,
  SessionSettingTarget,
} from "@/lib/session-settings"

const BUBBLES = [
  { top: 4, size: 2, duration: 2.6, delay: -0.4 },
  { top: 14, size: 3, duration: 3.2, delay: -2.1 },
  { top: 8, size: 4, duration: 2.8, delay: -1.1 },
  { top: 18, size: 2, duration: 3.6, delay: -0.8 },
  { top: 3, size: 3, duration: 3, delay: -2.5 },
  { top: 12, size: 2, duration: 3.4, delay: -1.7 },
  { top: 6, size: 3, duration: 3.1, delay: -0.9 },
  { top: 17, size: 4, duration: 2.9, delay: -2.3 },
  { top: 10, size: 2, duration: 3.5, delay: -0.2 },
  { top: 2, size: 2, duration: 2.7, delay: -1.8 },
  { top: 15, size: 3, duration: 3.3, delay: -1.2 },
  { top: 7, size: 4, duration: 3.6, delay: -3.1 },
]
const NO_THOUGHT = new Set([
  "disabled",
  "false",
  "no",
  "none",
  "nothink",
  "no-think",
  "no_think",
  "off",
])

export function ComposerThoughtSlider({
  setting,
  onChange,
}: {
  setting: SessionChoiceSetting
  onChange: (target: SessionSettingTarget, value: string) => void
}) {
  const levels = setting.groups.flatMap((group) => group.options)
  const current = levels.findIndex(
    (level) => level.value === setting.currentValue
  )
  const [preview, setPreview] = useState<number | null>(null)
  const [hovered, setHovered] = useState<number | null>(null)
  const index = preview !== null && preview < levels.length ? preview : current
  const selected = levels[index]
  const disabled = levels.length < 2
  const progress =
    levels.length > 1 && index >= 0 ? (index / (levels.length - 1)) * 100 : 0
  const filled =
    !!selected && !NO_THOUGHT.has(selected.value.trim().toLowerCase())
  const commit = (value: string) => {
    const option = levels[Number(value)]
    setPreview(null)
    if (!disabled && option && option.value !== setting.currentValue)
      onChange(setting.target, option.value)
  }
  if (!levels.length) return null
  return (
    <div className="px-3 py-3">
      <div className="mb-1 flex items-center gap-2 text-sm">
        <span className="text-muted-foreground">{setting.title}</span>
        <span data-testid="composer-thought-value">
          {selected?.name ?? "—"}
        </span>
      </div>
      {levels.length > 1 ? (
        <div
          className="composer-thought-slider relative h-8 w-full rounded-full"
          data-disabled={disabled}
        >
          <div
            aria-hidden="true"
            className="pointer-events-none absolute inset-0"
          >
            <div
              data-testid="composer-thought-track"
              data-progress={filled ? progress : 0}
              className="absolute inset-x-0 top-1 h-6 overflow-hidden rounded-full bg-foreground/10"
            >
              <div className="absolute inset-0 rounded-full border border-foreground/10" />
              <div
                className="composer-thought-slider-fill absolute inset-y-0 left-0 overflow-hidden rounded-full"
                data-max={filled && index === levels.length - 1}
                style={{
                  width: filled
                    ? `calc(14px + (100% - 28px) * ${progress / 100})`
                    : 0,
                }}
              >
                {filled && index === levels.length - 1 && !disabled ? (
                  <div
                    data-testid="composer-thought-bubbles"
                    className="composer-thought-bubbles absolute inset-0"
                  >
                    {BUBBLES.map((bubble, i) => (
                      <span
                        key={i}
                        className="composer-thought-bubble absolute inset-x-0"
                        style={{
                          top: bubble.top,
                          animationDuration: `${bubble.duration}s`,
                          animationDelay: `${bubble.delay}s`,
                        }}
                      >
                        <span
                          className="block rounded-full bg-white"
                          style={{ width: bubble.size, height: bubble.size }}
                        />
                      </span>
                    ))}
                  </div>
                ) : null}
              </div>
            </div>
            <div className="absolute inset-x-3.5 inset-y-0 flex items-center justify-between">
              {levels.map((level, i) => (
                <span
                  key={level.value}
                  data-composer-thought-stop
                  data-filled={filled && i <= index}
                  data-hovered={!disabled && hovered === i}
                  className="composer-thought-slider-stop size-1 shrink-0 rounded-full"
                />
              ))}
            </div>
            <div
              className="composer-thought-slider-thumb absolute top-0.5 size-7 rounded-full shadow-sm"
              style={{ left: `calc((100% - 28px) * ${progress / 100})` }}
            />
          </div>
          <input
            type="range"
            min={0}
            max={levels.length - 1}
            step={1}
            value={Math.max(0, index)}
            aria-disabled={disabled}
            aria-label={setting.title}
            aria-valuetext={selected?.name ?? "—"}
            data-testid="composer-thought-slider"
            data-levels={JSON.stringify(levels.map((level) => level.value))}
            className="composer-thought-range absolute inset-0 m-0 h-8 w-full cursor-pointer appearance-none bg-transparent outline-none aria-disabled:cursor-default"
            onPointerLeave={() => setHovered(null)}
            onPointerMove={(event) => {
              const rect = event.currentTarget.getBoundingClientRect()
              const trackWidth = rect.width - 28
              if (trackWidth <= 0 || event.pointerType === "touch") {
                setHovered(null)
                return
              }
              const next = Math.round(
                ((event.clientX - rect.left - 14) / trackWidth) *
                  (levels.length - 1)
              )
              const center =
                rect.left + 14 + (trackWidth * next) / (levels.length - 1)
              setHovered(
                levels[next] && Math.abs(event.clientX - center) <= 10
                  ? next
                  : null
              )
            }}
            onClick={(event) => event.stopPropagation()}
            onKeyDown={(event) => {
              if (event.key !== "Escape") event.stopPropagation()
            }}
            onChange={(event) => setPreview(Number(event.currentTarget.value))}
            onPointerUp={(event) => commit(event.currentTarget.value)}
            onKeyUp={(event) => {
              if (
                [
                  "Home",
                  "End",
                  "ArrowLeft",
                  "ArrowRight",
                  "ArrowUp",
                  "ArrowDown",
                  "PageUp",
                  "PageDown",
                ].includes(event.key)
              )
                commit(event.currentTarget.value)
            }}
            onBlur={(event) => {
              if (preview !== null) commit(event.currentTarget.value)
            }}
          />
        </div>
      ) : null}
      {selected?.description ? (
        <p className="mt-2 text-xs leading-snug text-muted-foreground">
          {selected.description}
        </p>
      ) : null}
    </div>
  )
}
