import { fireEvent, render } from "@testing-library/react"
import { describe, expect, it } from "vitest"
import { NextIntlClientProvider } from "next-intl"

import messages from "@/i18n/messages/en.json"
import { Reasoning, ReasoningContent, ReasoningTrigger } from "./reasoning"

describe("ZCode reasoning", () => {
  it("follows streaming content until the reader scrolls away, then resumes at the bottom", () => {
    const tree = (content: string) => (
      <NextIntlClientProvider locale="en" messages={messages}>
        <Reasoning open isStreaming>
          <ReasoningTrigger streamingText={content} />
          <ReasoningContent>{content}</ReasoningContent>
        </Reasoning>
      </NextIntlClientProvider>
    )
    const view = render(tree("first"))
    const scroll = view.container.querySelector<HTMLElement>(
      "[data-reasoning-scroll-mask]"
    )!
    let height = 300
    Object.defineProperties(scroll, {
      clientHeight: { configurable: true, get: () => 100 },
      scrollHeight: { configurable: true, get: () => height },
    })

    view.rerender(tree("first\nsecond"))
    expect(scroll.scrollTop).toBe(300)
    scroll.scrollTop = 50
    fireEvent.scroll(scroll)
    height = 400
    view.rerender(tree("first\nsecond\nthird"))
    expect(scroll.scrollTop).toBe(50)
    scroll.scrollTop = 300
    fireEvent.scroll(scroll)
    height = 500
    view.rerender(tree("first\nsecond\nthird\nfourth"))
    expect(scroll.scrollTop).toBe(500)
  })
})
