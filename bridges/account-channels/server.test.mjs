import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createRuntime, serve } from "./server.mjs";

test("only the requested four products are registered; an empty store has no credentials or models", async () => {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), "codepluse-account-"));
  const runtime = await createRuntime(dir);
  try {
    const rows = await runtime.status();
    assert.deepEqual(
      rows.map((row) => row.id),
      ["buddy", "workbuddy", "qoder", "qodercn"],
    );
    assert.ok(
      rows.every(
        (row) => !row.configured && !row.accounts.length && !row.models.length,
      ),
    );
    await assert.rejects(runtime.startLogin("unregistered"), /不支持/);
    await WebAssembly.compile(
      await fs.readFile(
        new URL("./vendor/qoder-auth-wasm.wasm", import.meta.url),
      ),
    );
  } finally {
    await runtime.close();
    await fs.rm(dir, { recursive: true, force: true });
  }
});

test("authenticated gateway preserves tool IDs, arguments, reasoning, and cache usage", async () => {
  const key = "test-only-local-key".repeat(3);
  const runtime = {
    status: async () => [{ id: "qoder", models: [{ id: "qoder/qfmodel" }] }],
    close: async () => {},
    async *stream() {
      yield { type: "reasoning-delta", text: "思考" };
      yield {
        type: "tool-call-delta",
        index: 3,
        id: "call-1",
        name: "read_file",
        argumentsDelta: '{"path":',
      };
      yield { type: "tool-call-delta", index: 3, argumentsDelta: '"a.txt"}' };
      yield {
        type: "block-end",
        index: 3,
        block: {
          type: "tool-call",
          id: "call-1",
          name: "read_file",
          arguments: '{"path":"a.txt"}',
        },
      };
      yield {
        type: "usage",
        usage: {
          inputTokens: 10,
          cacheReadTokens: 7,
          cacheWriteTokens: 2,
          outputTokens: 3,
          reasoningTokens: 1,
        },
      };
      yield { type: "finish", reason: { kind: "tool-calls" } };
    },
  };
  const app = await serve({ key, runtime });
  try {
    assert.equal((await fetch(`${app.url}/status`)).status, 401);
    for (const stream of [false, true]) {
      const response = await fetch(`${app.url}/v1/chat/completions`, {
        method: "POST",
        headers: { authorization: `Bearer ${key}` },
        body: JSON.stringify({ model: "qoder/qfmodel", stream }),
      });
      const text = await response.text();
      if (stream) {
        assert.match(text, /\[DONE\]/);
        assert.equal((text.match(/arguments/g) ?? []).length, 2);
        assert.match(text, /tool_calls/);
      } else {
        const result = JSON.parse(text);
        assert.equal(
          result.choices[0].message.tool_calls[0].function.arguments,
          '{"path":"a.txt"}',
        );
        assert.equal(result.choices[0].message.tool_calls[0].id, "call-1");
        assert.equal(result.choices[0].finish_reason, "tool_calls");
        assert.equal(result.choices[0].message.reasoning_content, "思考");
        assert.equal(result.usage.prompt_tokens, 19);
        assert.equal(result.usage.total_tokens, 22);
      }
    }
  } finally {
    await app.close();
  }
});

test("upstream failures remain failures after partial stream output", async () => {
  const key = "local-key-for-tests".repeat(3);
  const app = await serve({
    key,
    runtime: {
      close: async () => {},
      async *stream() {
        yield { type: "text-delta", text: "partial" };
        throw new Error("provider failed");
      },
    },
  });
  try {
    const response = await fetch(`${app.url}/v1/chat/completions`, {
      method: "POST",
      headers: { authorization: `Bearer ${key}` },
      body: JSON.stringify({ model: "qoder/qfmodel", stream: true }),
    });
    const text = await response.text();
    assert.match(text, /provider failed/);
    assert.doesNotMatch(text, /\[DONE\]/);
  } finally {
    await app.close();
  }
});
