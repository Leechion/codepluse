"use client"

import { useCallback, useEffect, useRef, type PointerEvent } from "react"

/** Paint-only feedback: inputs and their React state never move between trees. */
export function useSettingsCardMotion() {
  const active = useRef<HTMLElement | null>(null)
  const frame = useRef<number | null>(null)

  const reset = useCallback(() => {
    if (frame.current != null) cancelAnimationFrame(frame.current)
    frame.current = null
    if (active.current) {
      for (const key of [
        "--cp-spotlight-x",
        "--cp-spotlight-y",
        "--cp-tilt-x",
        "--cp-tilt-y",
      ]) {
        active.current.style.removeProperty(key)
      }
    }
    active.current = null
  }, [])

  useEffect(() => reset, [reset])

  const onPointerMove = useCallback(
    (event: PointerEvent<HTMLElement>) => {
      if (
        event.pointerType !== "mouse" ||
        !matchMedia("(hover: hover) and (pointer: fine)").matches ||
        matchMedia("(prefers-reduced-motion: reduce)").matches
      ) {
        reset()
        return
      }
      const target = event.target
      const card =
        target instanceof Element
          ? target.closest<HTMLElement>("[data-settings-card]")
          : null
      if (!card || !event.currentTarget.contains(card)) {
        reset()
        return
      }
      if (active.current !== card) reset()
      active.current = card
      if (frame.current != null) cancelAnimationFrame(frame.current)
      const { clientX, clientY } = event
      frame.current = requestAnimationFrame(() => {
        frame.current = null
        const bounds = card.getBoundingClientRect()
        if (!bounds.width || !bounds.height) return
        const x = Math.max(0, Math.min(bounds.width, clientX - bounds.left))
        const y = Math.max(0, Math.min(bounds.height, clientY - bounds.top))
        // Focused controls and collapsible sections stay perfectly still.
        const still =
          card.contains(document.activeElement) || card.matches("[data-state]")
        card.style.setProperty("--cp-spotlight-x", `${x}px`)
        card.style.setProperty("--cp-spotlight-y", `${y}px`)
        card.style.setProperty(
          "--cp-tilt-x",
          `${still ? 0 : (0.5 - y / bounds.height) * 1.4}deg`
        )
        card.style.setProperty(
          "--cp-tilt-y",
          `${still ? 0 : (x / bounds.width - 0.5) * 1.4}deg`
        )
      })
    },
    [reset]
  )

  return { onPointerMove, onPointerLeave: reset }
}
