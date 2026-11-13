import fs from "node:fs/promises";
import path from "node:path";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { pathToFileURL } from "node:url";
import { jsonRpc } from "./jsonrpc.mjs";

const objectSchema = (properties) => ({
  type: "object",
  properties,
  required: Object.keys(properties),
});
const string = (description) => ({ type: "string", description });
export const localTools = [
  {
    name: "read_file",
    description: "Read a UTF-8 file in the workspace.",
    parameters: objectSchema({ path: string("Workspace relative file path") }),
  },
  {
    name: "write_file",
    description: "Write a complete UTF-8 file in the workspace.",
    parameters: objectSchema({
      path: string("Workspace relative file path"),
      content: string("Complete file content"),
    }),
  },
  {
    name: "list_directory",
    description: "List a directory in the workspace.",
    parameters: objectSchema({
      path: string("Workspace relative directory, or ."),
    }),
  },
  {
    name: "run_shell",
    description:
      "Execute a shell command in the workspace after user permission.",
    parameters: objectSchema({ command: string("Shell command") }),
  },
];

export async function runAgent({
  channel,
  dataDir,
  url,
  key,
  model,
  permission = "readonly",
  input = process.stdin,
  output = process.stdout,
}) {
  if (!["buddy", "workbuddy", "qoder", "qodercn"].includes(channel))
    throw new Error("Invalid agent channel");
  const sessions = new Map();
  const headers = {
    authorization: `Bearer ${key}`,
    "content-type": "application/json",
  };
  const response = await fetch(`${url}/status`, {
    headers,
    signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) throw new Error("Account channel is unavailable");
  const catalog = (await response.json()).find((row) => row.id === channel);
  if (!catalog?.configured || !catalog.models.length)
    throw new Error("请先在智能体设置中登录账号");
  if (!catalog.models.some((row) => row.id === model))
    throw new Error("Selected model does not belong to this agent");
  async function refreshCatalog() {
    const response = await fetch(`${url}/status`, {
      headers,
      signal: AbortSignal.timeout(30_000),
    });
    if (!response.ok) throw new Error("Account channel is unavailable");
    const current = (await response.json()).find((row) => row.id === channel);
    if (!current?.configured) throw new Error("请先在智能体设置中登录账号");
    catalog.models = current.models;
  }
  await fs.mkdir(dataDir, { recursive: true, mode: 0o700 });
  function childEnv(extra = []) {
    const env = { ...process.env };
    delete env.CODEPLUSE_CHANNEL_KEY;
    delete env.CODEPLUSE_CHANNEL_URL;
    for (const item of extra) env[item.name] = item.value;
    return env;
  }
  async function connectMCP(spec) {
    if (!spec.command) throw new Error("Only stdio MCP servers are supported");
    const child = spawn(spec.command, spec.args ?? [], {
      env: childEnv(spec.env),
      windowsHide: true,
      stdio: ["pipe", "pipe", "pipe"],
    });
    child.stderr.on("data", () => {});
    const peer = jsonRpc(child.stdout, child.stdin);
    child.on("error", () => peer.close());
    child.on("exit", () => peer.close());
    const timeout = AbortSignal.timeout(30_000);
    try {
      await peer.call(
        "initialize",
        {
          protocolVersion: "2024-11-05",
          capabilities: {},
          clientInfo: { name: "codepluse-account-agent", version: "1.0.0" },
        },
        timeout,
      );
      peer.notify("notifications/initialized", {});
      const tools = [];
      let cursor;
      do {
        const result = await peer.call(
          "tools/list",
          cursor ? { cursor } : {},
          timeout,
        );
        tools.push(...(result.tools ?? []));
        cursor = result.nextCursor;
      } while (cursor);
      return { peer, child, name: spec.name, tools };
    } catch (error) {
      peer.close();
      child.kill();
      throw error;
    }
  }
  const save = (session) =>
    fs.writeFile(
      path.join(dataDir, `${session.id}.json`),
      JSON.stringify({
        id: session.id,
        cwd: session.cwd,
        model: session.model,
        messages: session.messages,
      }),
      { mode: 0o600 },
    );
  const config = (session) => ({
    models: {
      currentModelId: session.model,
      availableModels: catalog.models.map((row) => ({
        modelId: row.id,
        name: row.name,
      })),
    },
    modes: {
      currentModeId: session.mode,
      availableModes: [
        { id: "readonly", name: "只读" },
        ...(permission === "workspace"
          ? [{ id: "workspace", name: "工作区" }]
          : []),
      ],
    },
    configOptions: [
      {
        id: "model",
        name: "模型",
        category: "model",
        type: "select",
        currentValue: session.model,
        options: catalog.models.map((row) => ({
          value: row.id,
          name: row.name,
        })),
      },
    ],
  });
  const update = (session, value) =>
    rpc.notify("session/update", { sessionId: session.id, update: value });
  async function createSession(params, restore) {
    await refreshCatalog();
    const cwd = await fs.realpath(params.cwd);
    let saved;
    if (restore) {
      if (
        !new RegExp(`^account-${channel}-[a-f0-9-]{36}$`).test(params.sessionId)
      )
        throw new Error("Invalid session ID");
      saved = JSON.parse(
        await fs.readFile(
          path.join(dataDir, `${params.sessionId}.json`),
          "utf8",
        ),
      );
      if (saved.cwd !== cwd)
        throw new Error("Session workspace does not match");
    }
    const session = {
      id: saved?.id ?? `account-${channel}-${randomUUID()}`,
      cwd,
      model: saved?.model ?? model,
      messages: saved?.messages ?? [],
      mode: permission === "workspace" ? "workspace" : "readonly",
      mcp: [],
      tools: new Map(),
      active: null,
    };
    try {
      for (const spec of params.mcpServers ?? []) {
        const server = await connectMCP(spec);
        session.mcp.push(server);
        for (const tool of server.tools) {
          const name = `mcp__${spec.name}__${tool.name}`;
          session.tools.set(name, {
            name,
            description: tool.description,
            parameters: tool.inputSchema,
            server,
            tool: tool.name,
          });
        }
      }
      const old = sessions.get(session.id);
      if (old?.active) throw new Error("Cannot reload an active session");
      for (const server of old?.mcp ?? []) {
        server.peer.close();
        server.child.kill();
      }
      sessions.set(session.id, session);
      await save(session);
      return { sessionId: session.id, ...config(session) };
    } catch (error) {
      for (const server of session.mcp) {
        server.peer.close();
        server.child.kill();
      }
      throw error;
    }
  }
  async function workspacePath(session, file, writing = false) {
    const target = path.resolve(session.cwd, file);
    // Resolve existing parents as well, so symlinks cannot escape the workspace.
    let probe = writing ? path.dirname(target) : target;
    while (true) {
      try {
        probe = await fs.realpath(probe);
        break;
      } catch (error) {
        if (error.code !== "ENOENT" || probe === path.dirname(probe))
          throw error;
        probe = path.dirname(probe);
      }
    }
    const relative = path.relative(session.cwd, probe);
    if (
      relative === ".." ||
      relative.startsWith(`..${path.sep}`) ||
      path.isAbsolute(relative)
    )
      throw new Error("Path is outside the workspace");
    if (writing) {
      try {
        const real = await fs.realpath(target);
        const rel = path.relative(session.cwd, real);
        if (
          rel === ".." ||
          rel.startsWith(`..${path.sep}`) ||
          path.isAbsolute(rel)
        )
          throw new Error("Path is outside the workspace");
      } catch (error) {
        if (error.code !== "ENOENT") throw error;
      }
    }
    return target;
  }
  async function execute(session, call, signal) {
    if (signal.aborted) throw new Error("Cancelled")
    const args = JSON.parse(call.function.arguments || "{}");
    const name = call.function.name;
    if (session.tools.has(name)) {
      const tool = session.tools.get(name);
      return tool.server.peer.call(
        "tools/call",
        { name: tool.tool, arguments: args },
        signal,
      );
    }
    if (name === "read_file")
      return rpc.call(
        "fs/read_text_file",
        {
          sessionId: session.id,
          path: await workspacePath(session, args.path),
        },
        signal,
      );
    if (name === "write_file") {
      if (session.mode !== "workspace")
        throw new Error("Read-only session cannot write files");
      return rpc.call(
        "fs/write_text_file",
        {
          sessionId: session.id,
          path: await workspacePath(session, args.path, true),
          content: args.content,
        },
        signal,
      );
    }
    if (name === "list_directory")
      return (
        await fs.readdir(await workspacePath(session, args.path), {
          withFileTypes: true,
        })
      )
        .slice(0, 500)
        .map((entry) => ({ name: entry.name, directory: entry.isDirectory() }));
    if (name !== "run_shell") throw new Error("Unknown tool");
    if (session.mode !== "workspace")
      throw new Error("Read-only session cannot execute shell commands");
    const approval = await rpc.call(
      "session/request_permission",
      {
        sessionId: session.id,
        toolCall: {
          toolCallId: call.id,
          title: args.command,
          kind: "execute",
          rawInput: args,
        },
        options: [
          { optionId: "allow_once", name: "允许这次", kind: "allow_once" },
          { optionId: "reject_once", name: "拒绝", kind: "reject_once" },
        ],
      },
      signal,
    );
    if (approval.outcome?.optionId !== "allow_once")
      throw new Error("User declined shell execution");
    if (signal.aborted) throw new Error("Cancelled")
    return new Promise((resolve, reject) => {
      const child =
        process.platform === "win32"
          ? spawn(
              "powershell.exe",
              ["-NoProfile", "-NonInteractive", "-Command", args.command],
              { cwd: session.cwd, env: childEnv(), windowsHide: true },
            )
          : spawn("/bin/sh", ["-c", args.command], {
              cwd: session.cwd,
              env: childEnv(),
            });
      let result = "",
        truncated = false;
      const collect = (chunk) => {
        if (result.length < 128_000) result += chunk;
        else truncated = true;
      };
      child.stdout.on("data", collect);
      child.stderr.on("data", collect);
      const abort = () => {
        if (process.platform === "win32")
          spawn("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], {
            windowsHide: true,
            stdio: "ignore",
          }).on("error", () => {});
        else child.kill("SIGKILL");
      };
      signal.addEventListener("abort", abort, { once: true });
      const timeout = setTimeout(abort, 120_000);
      child.on("error", (error) => {
        clearTimeout(timeout);
        signal.removeEventListener("abort", abort);
        reject(error);
      });
      child.on("exit", (code) => {
        clearTimeout(timeout);
        signal.removeEventListener("abort", abort);
        resolve({ exitCode: code, output: result, truncated });
      });
    });
  }
  async function prompt(session, params) {
    if (session.active) throw new Error("Session is already running");
    const controller = new AbortController();
    session.active = controller;
    const userContent = [];
    for (const block of params.prompt ?? []) {
      if (block.type === "text")
        userContent.push({ type: "text", text: block.text });
      else if (block.type === "image")
        userContent.push({
          type: "image_url",
          image_url: { url: `data:${block.mimeType};base64,${block.data}` },
        });
      else if (block.resource?.text)
        userContent.push({ type: "text", text: block.resource.text });
    }
    session.messages.push({ role: "user", content: userContent });
    try {
      const tools = [
        ...localTools.filter(
          (tool) =>
            session.mode === "workspace" ||
            !["write_file", "run_shell"].includes(tool.name),
        ),
        ...session.tools.values(),
      ].map(({ name, description, parameters }) => ({
        type: "function",
        function: { name, description, parameters },
      }));
      for (let step = 0; step < 64; step++) {
        let text = "",
          reasoning = "";
        const calls = [];
        const response = await fetch(`${url}/v1/chat/completions`, {
          method: "POST",
          headers,
          signal: controller.signal,
          body: JSON.stringify({
            model: session.model,
            stream: true,
            user: session.id,
            messages: [
              {
                role: "system",
                content: `You are ${catalog.name}, an independent coding agent in Codepluse. Workspace: ${session.cwd}. Permission: ${session.mode}. Use real tools, report actual results, and use codeg-mcp tools to coordinate and delegate when available. Do not claim work that was not executed.`,
              },
              ...session.messages,
            ],
            tools,
            max_tokens: 8192,
          }),
        });
        if (!response.ok) {
          const error = await response.json();
          throw new Error(
            error.error?.message || `Channel HTTP ${response.status}`,
          );
        }
        let buffer = "";
        const decoder = new TextDecoder();
        for await (const bytes of response.body) {
          buffer += decoder.decode(bytes, { stream: true });
          let boundary;
          while ((boundary = buffer.indexOf("\n\n")) >= 0) {
            const event = buffer.slice(0, boundary);
            buffer = buffer.slice(boundary + 2);
            for (const line of event.split("\n")) {
              if (!line.startsWith("data: ") || line === "data: [DONE]")
                continue;
              const chunk = JSON.parse(line.slice(6));
              if (chunk.error) throw new Error(chunk.error.message);
              const delta = chunk.choices?.[0]?.delta ?? {};
              if (delta.content) {
                text += delta.content;
                update(session, {
                  sessionUpdate: "agent_message_chunk",
                  content: { type: "text", text: delta.content },
                });
              }
              if (delta.reasoning_content) {
                reasoning += delta.reasoning_content;
                update(session, {
                  sessionUpdate: "agent_thought_chunk",
                  content: { type: "text", text: delta.reasoning_content },
                });
              }
              for (const item of delta.tool_calls ?? []) {
                const call = (calls[item.index] ??= {
                  id: "",
                  type: "function",
                  function: { name: "", arguments: "" },
                });
                if (item.id) call.id = item.id;
                if (item.function?.name)
                  call.function.name = item.function.name;
                call.function.arguments += item.function?.arguments ?? "";
              }
            }
          }
        }
        if (!calls.length && !text.trim())
          throw new Error("Model returned no text or tool calls");
        if (calls.some((call) => !call?.id || !call.function.name))
          throw new Error("Model returned an incomplete tool call");
        session.messages.push({
          role: "assistant",
          content: text || null,
          ...(reasoning ? { reasoning_content: reasoning } : {}),
          ...(calls.length ? { tool_calls: calls } : {}),
        });
        if (!calls.length) {
          await save(session);
          return { stopReason: "end_turn" };
        }
        for (const call of calls) {
          if (!call.id || !call.function.name)
            throw new Error("Model returned an incomplete tool call");
          update(session, {
            sessionUpdate: "tool_call",
            toolCallId: call.id,
            title: call.function.name,
            kind: call.function.name === "run_shell" ? "execute" : "other",
            status: "in_progress",
            rawInput: call.function.arguments,
          });
          let result,
            failed = false;
          try {
            if (controller.signal.aborted) throw new Error("Cancelled");
            result = await execute(session, call, controller.signal);
          } catch (error) {
            failed = true;
            result = { error: error.message };
          }
          const resultText = JSON.stringify(result).slice(0, 120_000);
          session.messages.push({
            role: "tool",
            tool_call_id: call.id,
            content: resultText,
          });
          update(session, {
            sessionUpdate: "tool_call_update",
            toolCallId: call.id,
            status: failed ? "failed" : "completed",
            content: [
              { type: "content", content: { type: "text", text: resultText } },
            ],
          });
        }
        await save(session);
        if (controller.signal.aborted) return { stopReason: "cancelled" };
      }
      throw new Error(
        "Reached the 64-step tool limit; continue the session to resume",
      );
    } catch (error) {
      if (controller.signal.aborted) return { stopReason: "cancelled" };
      throw error;
    } finally {
      session.active = null;
      await save(session);
    }
  }
  const rpc = jsonRpc(input, output, async (method, params) => {
    if (method === "initialize")
      return {
        protocolVersion: 1,
        agentInfo: {
          name: `codepluse-${channel}`,
          title: catalog.name,
          version: "1.0.0",
        },
        agentCapabilities: {
          loadSession: true,
          promptCapabilities: { image: true, embeddedContext: true },
        },
        authMethods: [],
      };
    if (method === "session/new") return createSession(params, false);
    if (method === "session/load") return createSession(params, true);
    const session = sessions.get(params.sessionId);
    if (!session) throw new Error("Unknown session");
    if (method === "session/cancel") {
      session.active?.abort();
      return {};
    }
    if (method === "session/set_mode") {
      if (
        ![
          "readonly",
          ...(permission === "workspace" ? ["workspace"] : []),
        ].includes(params.modeId)
      )
        throw new Error("Unsupported permission mode");
      session.mode = params.modeId;
      return {};
    }
    if (
      method === "session/set_model" ||
      method === "session/set_config_option"
    ) {
      if (session.active)
        throw new Error("Cannot change the model during an active turn");
      await refreshCatalog();
      const selected = params.modelId ?? params.value;
      if (params.configId && params.configId !== "model")
        throw new Error("Unsupported configuration");
      if (!catalog.models.some((row) => row.id === selected))
        throw new Error("Model does not belong to this agent");
      session.model = selected;
      await save(session);
      return config(session);
    }
    if (method === "session/prompt") return prompt(session, params);
    throw new Error("Unsupported method");
  });
  input.on("end", () => {
    for (const session of sessions.values()) {
      session.active?.abort();
      for (const server of session.mcp) {
        server.peer.close();
        server.child.kill();
      }
    }
  });
  return rpc;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  await runAgent({
    channel: process.argv[2],
    dataDir: path.resolve(process.argv[3]),
    url: process.env.CODEPLUSE_CHANNEL_URL,
    key: process.env.CODEPLUSE_CHANNEL_KEY,
    model: process.env.CODEPLUSE_CHANNEL_MODEL,
    permission: process.env.CODEPLUSE_CHANNEL_PERMISSION,
  });
}
