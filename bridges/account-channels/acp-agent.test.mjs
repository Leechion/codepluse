import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { PassThrough } from "node:stream";
import { createServer } from "node:http";
import { runAgent } from "./acp-agent.mjs";
import { jsonRpc } from "./jsonrpc.mjs";
import { fileURLToPath } from "node:url";

async function setup(t, respond, permission = "readonly") {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), "codepluse-acp-"));
  const bodies = [],
    updates = [];
  const server = createServer(async (req, res) => {
    if (req.url === "/status") {
      res.end(
        JSON.stringify([
          {
            id: "qoder",
            name: "Qoder",
            configured: true,
            models: [
              { id: "qoder/a", name: "A" },
              { id: "qoder/b", name: "B" },
            ],
          },
        ]),
      );
      return;
    }
    let raw = "";
    for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw);
    bodies.push(body);
    await respond(body, res, bodies.length);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const input = new PassThrough(),
    output = new PassThrough();
  const host = jsonRpc(output, input, async (method, params) => {
    if (method === "session/update") {
      updates.push(params.update);
      return {};
    }
    if (method === "fs/read_text_file") {
      assert.equal(params.path, path.join(dir, "a.txt"));
      return { content: "fixture data" };
    }
    if (method === "fs/write_text_file") throw new Error("Unexpected write");
    if (method === "session/request_permission")
      return { outcome: { outcome: "selected", optionId: "reject_once" } };
    throw new Error(`Unexpected method ${method}`);
  });
  const agent = await runAgent({
    channel: "qoder",
    dataDir: path.join(dir, "sessions"),
    url: `http://127.0.0.1:${server.address().port}`,
    key: "test",
    model: "qoder/a",
    permission,
    input,
    output,
  });
  await fs.writeFile(path.join(dir, "a.txt"), "fixture data");
  t.after(async () => {
    input.end();
    agent.close();
    host.close();
    output.end();
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
    await fs.rm(dir, { recursive: true, force: true });
  });
  const initialized = await host.call("initialize", { protocolVersion: 1 });
  assert.equal(initialized.agentInfo.name, "codepluse-qoder");
  const session = await host.call("session/new", { cwd: dir, mcpServers: [] });
  return { host, session, dir, bodies, updates };
}
function sse(res, delta) {
  res.setHeader("content-type", "text/event-stream");
  res.end(
    `data: ${JSON.stringify({ choices: [{ delta }] })}\n\ndata: [DONE]\n\n`,
  );
}

test("independent ACP agent executes a real host file tool, preserves tool history, and resumes its session", async (t) => {
  const app = await setup(t, async (body, res, turn) => {
    if (turn === 1)
      sse(res, {
        tool_calls: [
          {
            index: 0,
            id: "call-read",
            function: { name: "read_file", arguments: '{"path":"a.txt"}' },
          },
        ],
      });
    else {
      assert.ok(
        body.messages.some(
          (message) =>
            message.role === "tool" &&
            message.tool_call_id === "call-read" &&
            message.content.includes("fixture data"),
        ),
      );
      sse(res, { content: "读取完成" });
    }
  });
  const result = await app.host.call("session/prompt", {
    sessionId: app.session.sessionId,
    prompt: [{ type: "text", text: "Read a.txt" }],
  });
  assert.equal(result.stopReason, "end_turn");
  assert.equal(app.bodies.length, 2);
  assert.ok(
    app.updates.some(
      (update) =>
        update.sessionUpdate === "tool_call_update" &&
        update.status === "completed",
    ),
  );
  assert.ok(app.updates.some((update) => update.content?.text === "读取完成"));
  assert.ok(
    !app.bodies[0].tools.some((tool) => tool.function.name === "run_shell"),
  );
  const restored = await app.host.call("session/load", {
    sessionId: app.session.sessionId,
    cwd: app.dir,
    mcpServers: [],
  });
  assert.equal(restored.sessionId, app.session.sessionId);
  await assert.rejects(
    app.host.call("session/set_config_option", {
      sessionId: app.session.sessionId,
      configId: "model",
      value: "qodercn/a",
    }),
    /does not belong/,
  );
  const changed = await app.host.call("session/set_config_option", {
    sessionId: app.session.sessionId,
    configId: "model",
    value: "qoder/b",
  });
  assert.equal(changed.models.currentModelId, "qoder/b");
});

test("workspace agent honors rejected shell permission and returns the real refusal to the model", async (t) => {
  const app = await setup(
    t,
    async (body, res, turn) => {
      if (turn === 1)
        sse(res, {
          tool_calls: [
            {
              index: 0,
              id: "shell-1",
              function: {
                name: "run_shell",
                arguments: '{"command":"echo should-not-run"}',
              },
            },
          ],
        });
      else {
        assert.match(body.messages.at(-1).content, /User declined/);
        sse(res, { content: "权限已拒绝" });
      }
    },
    "workspace",
  );
  await app.host.call("session/prompt", {
    sessionId: app.session.sessionId,
    prompt: [{ type: "text", text: "Run command" }],
  });
  assert.ok(
    app.updates.some(
      (update) =>
        update.sessionUpdate === "tool_call_update" &&
        update.status === "failed",
    ),
  );
});

test("ACP cancellation aborts the actual model HTTP request", async (t) => {
  let began;
  const ready = new Promise((resolve) => {
    began = resolve;
  });
  const app = await setup(t, async (_, res) => {
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.write(": waiting\n\n");
    began();
  });
  const running = app.host.call("session/prompt", {
    sessionId: app.session.sessionId,
    prompt: [{ type: "text", text: "Wait" }],
  });
  await ready;
  await app.host.call("session/cancel", { sessionId: app.session.sessionId });
  assert.equal((await running).stopReason, "cancelled");
});

test("independent agent communicates with a real stdio MCP process and feeds its result back to its own model", async (t) => {
  const app = await setup(t, async (body, res, turn) => {
    if (turn === 1) {
      assert.ok(
        body.tools.some(
          (tool) =>
            tool.function.name === "mcp__codeg-mcp__check_user_feedback",
        ),
      );
      sse(res, {
        tool_calls: [
          {
            index: 0,
            id: "team-1",
            function: {
              name: "mcp__codeg-mcp__check_user_feedback",
              arguments: "{}",
            },
          },
        ],
      });
    } else {
      assert.match(body.messages.at(-1).content, /peer-task: review completed/);
      sse(res, { content: "同僚已完成审核" });
    }
  });
  const session = await app.host.call("session/new", {
    cwd: app.dir,
    mcpServers: [
      {
        name: "codeg-mcp",
        command: process.execPath,
        args: [fileURLToPath(new URL("./fixtures/mcp.mjs", import.meta.url))],
        env: [],
      },
    ],
  });
  const result = await app.host.call("session/prompt", {
    sessionId: session.sessionId,
    prompt: [{ type: "text", text: "Read teammate progress" }],
  });
  assert.equal(result.stopReason, "end_turn");
  assert.ok(
    app.updates.some(
      (row) =>
        row.sessionUpdate === "tool_call_update" && row.status === "completed",
    ),
  );
});
