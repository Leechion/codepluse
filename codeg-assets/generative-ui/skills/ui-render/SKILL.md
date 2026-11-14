---
name: ui-render
description: Create structured UI specifications that Codepluse can preview as interactive cards.
---

# UI render skill

When the user asks for a small interface, dashboard, form, or visual prototype, return a concise specification in a fenced `spec` block. Keep surrounding explanation short.

```spec
{
  "type": "ui",
  "title": "Example",
  "components": [
    { "type": "text", "content": "A clear heading" },
    { "type": "button", "label": "Continue", "action": "continue" }
  ]
}
```

Use only components and properties supported by the host renderer. Prefer semantic labels, sensible defaults, and a compact layout. Do not claim that a button performed an action unless the conversation tools actually performed it.
