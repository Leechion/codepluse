import { fireEvent, render, screen } from "@testing-library/react"
import { describe, expect, it } from "vitest"

import { ExecuteOutput } from "./execute-output"

describe("ZCode terminal output", () => {
  it("pauses a running output while the reader scrolls up and follows again at the bottom", () => {
    const view = render(<ExecuteOutput text="first" running />)
    const scroll = screen.getByTestId("bash-output-scroll")
    Object.defineProperties(scroll, {
      clientHeight: { configurable: true, get: () => 40 },
      scrollHeight: { configurable: true, get: () => 200 },
    })
    view.rerender(<ExecuteOutput text="first\nsecond" running />)
    expect(scroll.scrollTop).toBe(200)
    scroll.scrollTop = 30
    fireEvent.scroll(scroll)
    expect(scroll).toHaveAttribute("data-following", "false")
    view.rerender(<ExecuteOutput text="first\nsecond\nthird" running />)
    expect(scroll).not.toHaveTextContent("third")
    scroll.scrollTop = 160
    fireEvent.scroll(scroll)
    expect(scroll).toHaveAttribute("data-following", "true")
    expect(scroll).toHaveTextContent("third")
  })

  it("keeps ANSI output readable after the command has completed", () => {
    render(<ExecuteOutput text={"\u001b[31mfailed\u001b[0m"} running={false} />)
    expect(screen.getByTestId("bash-result-output")).toHaveTextContent("failed")
  })
})
