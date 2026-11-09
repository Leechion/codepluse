// Adapted from ZCode's ExecuteOutput, Apache-2.0; see THIRD_PARTY_NOTICES.md.
import { useLayoutEffect, useRef, useState } from "react"
import Ansi from "ansi-to-react"
import { ScrollFadeViewport } from "./scroll-fade-viewport"

export function ExecuteOutput({
  text,
  running,
}: {
  text: string
  running: boolean
}) {
  const scroll = useRef<HTMLDivElement>(null)
  const previousTop = useRef(0)
  const hasStreamed = useRef(running)
  const [frozen, setFrozen] = useState<string | null>(null)
  const following = frozen === null
  const display = frozen ?? text

  useLayoutEffect(() => {
    if (running) hasStreamed.current = true
    if (!hasStreamed.current || !following || !scroll.current) return
    scroll.current.scrollTop = scroll.current.scrollHeight
    // 程序吸底后记录浏览器实际位置，避免尾窗变短时把程序滚动误判为上滚。
    previousTop.current = scroll.current.scrollTop
  }, [display, following, running])

  return (
    <ScrollFadeViewport
      ref={scroll}
      data-testid="bash-output-scroll"
      data-following={following}
      // 原预览与结果的高度上限不同且不吸底；共用五行上限，短内容自适应，结束时保留阅读状态。
      className="mx-4 my-3 min-w-0 max-w-full max-h-[5lh] flex-none overflow-auto leading-5"
      tabIndex={0}
      onScroll={(event) => {
        const el = event.currentTarget
        const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight <= 8
        if (
          hasStreamed.current &&
          following &&
          el.scrollTop < previousTop.current &&
          !atBottom
        ) {
          setFrozen(display)
        } else if (!following && atBottom) {
          setFrozen(null)
        }
        previousTop.current = el.scrollTop
      }}
    >
      <pre
        data-testid={
          running ? "bash-output-preview-full" : "bash-result-output"
        }
        className="activity-ansi whitespace-pre-wrap break-words font-mono text-sm leading-5 text-muted-foreground"
      >
        <Ansi>{display}</Ansi>
      </pre>
    </ScrollFadeViewport>
  )
}
