import fs from "node:fs";
import path from "node:path";
import { createServer } from "node:http";
import { randomUUID, timingSafeEqual } from "node:crypto";
import { AsyncLocalStorage } from "node:async_hooks";
import { pathToFileURL } from "node:url";
import { createCredentials } from "./vendor/credentials.mjs";
import { attachments } from "./vendor/images.mjs";
import {
  AccountPool,
  BuddyAuth,
  QoderAuth,
  CODEBUDDY,
  WORKBUDDY,
  QODER,
  QODER_CN,
  registerBuddyLlm,
  registerQoderLlm,
} from "./protocol.mjs";
import {
  toGenerateOptions,
  normalizeReasoningEffort,
  normalizeMaxTokens,
  makeReadImage,
  makeReadImageRequest,
} from "./vendor/business.mjs";

export const products = [
  { product: CODEBUDDY, name: "WorkBuddy 国内版" },
  { product: WORKBUDDY, name: "WorkBuddy 国际版" },
  { product: QODER, name: "Qoder" },
  { product: QODER_CN, name: "Qoder CN" },
];

export async function createRuntime(dataDir) {
  fs.mkdirSync(dataDir, { recursive: true, mode: 0o700 });
  const credentials = createCredentials(dataDir);
  const adapters = new Map(),
    auths = new Map(),
    logins = new Map(),
    cleanups = [];
  const requestAccount = new AsyncLocalStorage();
  const ctx = {
    platform: { channelHome: dataDir },
    credentials,
    attachments,
    logger: Object.fromEntries(
      ["info", "warn", "error"].map((level) => [
        level,
        (message) => process.stderr.write(`[${level}] ${message}\n`),
      ]),
    ),
    get(name) {
      return this[name];
    },
    provide(name, value) {
      this[name] = value;
    },
    effect(body) {
      const cleanup = body();
      if (typeof cleanup === "function") cleanups.push(cleanup);
      return cleanup;
    },
    emit() {},
    llm: {
      registerAdapter(ids, adapter) {
        for (const id of ids) adapters.set(id, adapter);
      },
    },
  };
  const pool = new AccountPool(ctx);
  for (const { product } of products) {
    const isBuddy = ["buddy", "workbuddy"].includes(product.id);
    const auth = isBuddy
      ? new BuddyAuth(ctx, { product })
      : new QoderAuth(ctx, { product });
    auths.set(product.id, auth);
    const resolveCredential = async (model) => {
      const selected = await pool.getAvailableAccount(product.id, model ?? "");
      requestAccount.getStore()?.set(product.id, selected?.entry);
      return selected?.credential;
    };
    const refresh = async () => {
      const entry = requestAccount.getStore()?.get(product.id);
      if (!entry) throw new Error(`${product.id}: 没有可续期的已登录账号`);
      await auth.refreshAccountCredential(entry.credentialRef, pool, entry.id);
    };
    const options = {
      product,
      credentialRef: product.defaultCredentialRef,
      accountPool: pool,
      resolveCredential,
      refresh,
      currentAccountId: () => requestAccount.getStore()?.get(product.id)?.id,
      readImage: makeReadImage(ctx),
      readImageRequest: makeReadImageRequest(ctx),
      ...(isBuddy ? { fetchRemoteModels: () => auth.fetchModels(pool) } : {}),
    };
    if (isBuddy) registerBuddyLlm(ctx, options);
    else registerQoderLlm(ctx, options);
  }
  const requireProduct = (id) => {
    const entry = products.find((row) => row.product.id === id);
    if (!entry)
      throw Object.assign(new Error("不支持的账号渠道"), { statusCode: 400 });
    return entry;
  };
  /**
   * 账号资料缓存（昵称 / 头像 / 积分）。上游资料接口每次返回**重新签名**的头像地址，
   * 所以缓存必须带 TTL：太长会让头像链接过期，太短会拖慢设置页。失败也要缓存，
   * 否则每次刷新都会重复打上游。
   */
  const profileCache = new Map();
  const PROFILE_TTL_MS = 10 * 60 * 1000;

  function profileOrigin(product, credential) {
    const domain =
      typeof credential.domain === "string" ? credential.domain.trim() : "";
    return `https://${domain.length > 0 ? domain : product.apiDomain}`;
  }

  async function readCredential(ref) {
    try {
      const resolved = await credentials.resolve(ref);
      return resolved?.value ? JSON.parse(resolved.value) : undefined;
    } catch {
      return undefined;
    }
  }

  /** 积分字段名各产品不同，也只在上游真的下发时才有。 */
  function pickCredits(data) {
    const keys = [
      "credits",
      "credit",
      "creditBalance",
      "balance",
      "remainingCredits",
      "creditsRemaining",
      "quota",
    ];
    for (const key of keys) {
      const value = data[key];
      if (typeof value === "number" && Number.isFinite(value)) return value;
      if (typeof value === "string" && /^-?\d+(\.\d+)?$/.test(value.trim()))
        return Number(value);
    }
    return undefined;
  }

  async function requestProfile(product, credential) {
    const origin = profileOrigin(product, credential);
    const domain =
      typeof credential.domain === "string" && credential.domain.length > 0
        ? credential.domain
        : product.apiDomain;
    const headers = {
      "X-Domain": domain,
      Authorization: `Bearer ${credential.access_token}`,
      "User-Agent": product.userAgent,
      Accept: "application/json",
    };
    if (
      typeof credential.enterprise_id === "string" &&
      credential.enterprise_id.length > 0
    ) {
      headers["X-Enterprise-Id"] = credential.enterprise_id;
      headers["X-Tenant-Id"] = credential.enterprise_id;
    }
    for (const path of ["/console/account", "/v2/plugin/account"]) {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), 10_000);
      try {
        const response = await fetch(origin + path, {
          headers,
          signal: controller.signal,
        });
        if (!response.ok) continue;
        const body = await response.json();
        const data = body?.data ?? body;
        if (typeof data !== "object" || data === null) continue;
        const nickname =
          typeof data.nickname === "string" ? data.nickname.trim() : "";
        const avatarUrl =
          typeof data.avatarUrl === "string" ? data.avatarUrl : "";
        const credits = pickCredits(data);
        return {
          ...(nickname.length > 0 ? { nickname } : {}),
          ...(avatarUrl.length > 0 ? { avatarUrl } : {}),
          ...(credits !== void 0 ? { credits } : {}),
        };
      } catch {
        continue;
      } finally {
        clearTimeout(timer);
      }
    }
    return void 0;
  }

  /**
   * 账号资料：先问上游，失败再退回凭据里登录时写入的昵称。
   *
   * 为什么要兜底：账号索引里存的是通道自己生成的 UUID（`nickname` 字段只是
   * id 的镜像），真实昵称在凭据文档里；上游不可达时仍应显示用户名而不是裸 UUID。
   */
  async function accountProfile(product, credentialRef) {
    if (typeof credentialRef !== "string" || credentialRef.length === 0)
      return void 0;
    const cached = profileCache.get(credentialRef);
    if (cached !== void 0 && Date.now() - cached.at < PROFILE_TTL_MS)
      return cached.profile;
    const credential = await readCredential(credentialRef);
    let profile =
      credential === void 0
        ? void 0
        : await requestProfile(product, credential);
    if (
      profile === void 0 &&
      typeof credential?.nickname === "string" &&
      credential.nickname.length > 0
    ) {
      profile = { nickname: credential.nickname };
    }
    profileCache.set(credentialRef, { at: Date.now(), profile });
    return profile;
  }

  async function status() {
    return Promise.all(
      products.map(async ({ product, name }) => {
        const accounts = await Promise.all(
          pool.listAccountsByProvider(product.id).map(async (entry) => {
            const profile = await accountProfile(product, entry.credentialRef);
            // 索引里的 nickname 是 id 镜像，只有和 id 不同才值得采信。
            const stored =
              typeof entry.nickname === "string" && entry.nickname !== entry.id
                ? entry.nickname
                : "";
            const display = profile?.nickname || stored || entry.nickname || entry.id;
            return {
              id: entry.id,
              nickname: display,
              ...(profile?.nickname || stored
                ? { username: profile?.nickname || stored }
                : {}),
              ...(profile?.avatarUrl ? { avatarUrl: profile.avatarUrl } : {}),
              ...(profile?.credits !== void 0 ? { credits: profile.credits } : {}),
              enabled: entry.enabled,
              expiresAt: entry.expiresAt,
            };
          }),
        );
        const models = (
          await adapters.get(product.id).listModels(product.id)
        ).map((row) => ({
          id: `${product.id}/${row.id}`,
          name: row.name || row.id,
        }));
        return {
          id: product.id,
          name,
          configured: accounts.some((row) => row.enabled),
          accounts,
          models,
        };
      }),
    );
  }
  async function startLogin(provider) {
    const { product } = requireProduct(provider);
    const existing = [...logins.values()].find(
      (row) => row.provider === provider && row.state === "pending",
    );
    if (existing) return publicLogin(existing);
    const controller = new AbortController();
    const row = {
      id: randomUUID(),
      provider,
      state: "pending",
      url: "",
      controller,
    };
    logins.set(row.id, row);
    const accountId = randomUUID();
    const refName = `${provider.toUpperCase()}_ACCOUNT_${accountId.replaceAll("-", "")}`;
    const fetcher = (url, options = {}) =>
      fetch(url, {
        ...options,
        signal: options.signal
          ? AbortSignal.any([options.signal, controller.signal])
          : controller.signal,
      });
    const opts = { accountId, refName, pool, fetcher };
    const auth = auths.get(provider);
    try {
      let result;
      if (provider === "qoder" || provider === "qodercn") {
        const started = await auth.startLogin(opts);
        row.url = started.loginUrl;
        row.close = started.close;
        result = started.result;
      } else {
        let resolveURL, rejectURL;
        const urlReady = new Promise((resolve, reject) => {
          resolveURL = resolve;
          rejectURL = reject;
        });
        result = auth.login({
          ...opts,
          openBrowser: (url) => {
            row.url = url;
            resolveURL();
          },
        });
        result.catch(rejectURL);
        await urlReady;
      }
      result.then(
        () => {
          row.state = "complete";
        },
        (error) => {
          row.state = controller.signal.aborted ? "cancelled" : "failed";
          row.error = safeError(error);
        },
      );
      result
        .finally(() => {
          void row.close?.();
          setTimeout(() => logins.delete(row.id), 10 * 60_000).unref();
        })
        .catch(() => {});
      return publicLogin(row);
    } catch (error) {
      row.state = "failed";
      row.error = safeError(error);
      controller.abort();
      void row.close?.();
      return publicLogin(row);
    }
  }
  function publicLogin({ id, provider, state, url, error }) {
    return { id, provider, state, url, error };
  }
  const refreshTimer = setInterval(() => {
    for (const auth of auths.values())
      void auth
        .refreshAll(pool)
        .catch((error) => ctx.logger.warn(safeError(error)));
  }, 60_000);
  refreshTimer.unref();
  return {
    status,
    startLogin,
    loginStatus(id) {
      const row = logins.get(id);
      if (!row)
        throw Object.assign(new Error("登录流程已失效"), { statusCode: 404 });
      return publicLogin(row);
    },
    cancel(id) {
      const row = logins.get(id);
      if (row?.state === "pending") {
        row.controller.abort();
        void row.close?.();
        row.state = "cancelled";
      }
      return {};
    },
    async logout(provider, accountId) {
      requireProduct(provider);
      const accounts = pool.listAccountsByProvider(provider);
      for (const account of accounts)
        if (!accountId || account.id === accountId)
          await pool.removeAccount(account.id);
      return {};
    },
    async *stream(body, signal) {
      const [provider, ...parts] = String(body.model ?? "").split("/");
      requireProduct(provider);
      const model = parts.join("/");
      const adapter = adapters.get(provider);
      if (!(await adapter.listModels(provider)).some((row) => row.id === model))
        throw Object.assign(new Error("该模型不存在或账号未登录"), {
          statusCode: 400,
        });
      const store = new Map();
      const call = await requestAccount.run(store, () =>
        adapter.prepareCall(provider, model, signal),
      );
      const effort = normalizeReasoningEffort(
        body.reasoning_effort,
        call.model,
        provider,
        model,
      );
      const maxTokens = body.max_completion_tokens ?? body.max_tokens;
      const options = await toGenerateOptions(
        body,
        signal,
        effort,
        maxTokens === undefined
          ? undefined
          : normalizeMaxTokens(maxTokens, provider, model),
        { bridge: attachments },
        () => {},
      );
      options.sessionId = `codepluse:${String(body.user ?? "session")}`;
      const iterator = call.stream(options)[Symbol.asyncIterator]();
      while (true) {
        const next = await requestAccount.run(store, () => iterator.next());
        if (next.done) break;
        yield next.value;
      }
    },
    async close() {
      clearInterval(refreshTimer);
      for (const row of logins.values()) {
        row.controller.abort();
        await row.close?.();
      }
      for (const cleanup of cleanups.reverse()) await cleanup();
      credentials.dispose();
    },
  };
}

function safeError(error) {
  return String(error?.message ?? error)
    .replace(/(Bearer\s+)[^\s"']+/gi, "$1[redacted]")
    .replace(
      /((?:access_token|refresh_token|security_oauth_token|authorization)["']?\s*[:=]\s*["']?)[^\s,"'}]+/gi,
      "$1[redacted]",
    )
    .slice(0, 600);
}

export async function serve({ dataDir, key, runtime, port = 0 }) {
  if (!key || key.length < 32) throw new Error("本地渠道服务缺少访问密钥");
  runtime ??= await createRuntime(dataDir);
  const server = createServer(async (req, res) => {
    const supplied = Buffer.from(req.headers.authorization ?? "");
    const expected = Buffer.from(`Bearer ${key}`);
    if (
      supplied.length !== expected.length ||
      !timingSafeEqual(supplied, expected)
    ) {
      res.writeHead(401);
      res.end();
      return;
    }
    const controller = new AbortController();
    res.on("close", () => controller.abort());
    const json = (value) => {
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify(value));
    };
    try {
      let body = {};
      if (req.method === "POST") {
        const chunks = [];
        let length = 0;
        for await (const chunk of req) {
          length += chunk.length;
          if (length > 32 * 1024 * 1024)
            throw Object.assign(new Error("请求体过大"), { statusCode: 413 });
          chunks.push(chunk);
        }
        body = JSON.parse(Buffer.concat(chunks).toString() || "{}");
      }
      if (req.method === "GET" && req.url === "/status")
        return json(await runtime.status());
      if (req.method === "GET" && req.url === "/health")
        return json({ ok: true });
      if (req.method === "POST" && req.url === "/login/start")
        return json(await runtime.startLogin(body.provider));
      if (req.method === "GET" && req.url.startsWith("/login/"))
        return json(runtime.loginStatus(req.url.slice(7)));
      if (req.method === "POST" && req.url === "/login/cancel")
        return json(runtime.cancel(body.id));
      if (req.method === "POST" && req.url === "/logout")
        return json(await runtime.logout(body.provider, body.accountId));
      if (req.method === "GET" && req.url === "/v1/models")
        return json({
          object: "list",
          data: (await runtime.status()).flatMap((row) =>
            row.models.map((model) => ({
              ...model,
              object: "model",
              owned_by: row.id,
            })),
          ),
        });
      if (req.method !== "POST" || req.url !== "/v1/chat/completions") {
        res.writeHead(404);
        res.end();
        return;
      }
      const id = `chatcmpl-${randomUUID()}`,
        created = Math.floor(Date.now() / 1000),
        toolSlots = new Map();
      let text = "",
        reasoning = "",
        finish = "stop",
        usage;
      const calls = [];
      const send = (delta) => {
        if (!body.stream) return;
        if (!res.headersSent)
          res.writeHead(200, {
            "content-type": "text/event-stream",
            "cache-control": "no-cache",
          });
        res.write(
          `data: ${JSON.stringify({ id, object: "chat.completion.chunk", created, model: body.model, choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`,
        );
      };
      for await (const chunk of runtime.stream(body, controller.signal)) {
        if (chunk.type === "text-delta") {
          text += chunk.text;
          send({ content: chunk.text });
        }
        if (chunk.type === "reasoning-delta") {
          const delta = chunk.text ?? chunk.reasoning ?? "";
          reasoning += delta;
          send({ reasoning_content: delta });
        }
        if (
          chunk.type === "tool-call-delta" ||
          (chunk.type === "block-end" && chunk.block?.type === "tool-call")
        ) {
          let index = toolSlots.get(chunk.index);
          const full = chunk.type === "block-end";
          if (index === undefined) {
            index = calls.length;
            toolSlots.set(chunk.index, index);
            calls.push({
              id: "",
              type: "function",
              function: { name: "", arguments: "" },
            });
          }
          const call = calls[index],
            delta = { index, type: "function", function: {} };
          const block = full ? chunk.block : chunk;
          if (block.id && call.id !== block.id) {
            call.id = block.id;
            delta.id = block.id;
          }
          if (block.name && call.function.name !== block.name) {
            call.function.name = block.name;
            delta.function.name = block.name;
          }
          let args = full
            ? (block.arguments ?? "")
            : (chunk.argumentsDelta ?? "");
          if (full && call.function.arguments) {
            if (!args.startsWith(call.function.arguments))
              throw new Error("工具参数流与最终参数不一致");
            args = args.slice(call.function.arguments.length);
          }
          if (args) {
            call.function.arguments += args;
            delta.function.arguments = args;
          }
          send({ tool_calls: [delta] });
          finish = "tool_calls";
        }
        if (chunk.type === "usage") usage = chunk.usage;
        if (chunk.type === "finish") {
          if (["error", "aborted"].includes(chunk.reason?.kind))
            throw new Error(chunk.reason.failure?.message || "模型推理失败");
          if (chunk.reason?.kind === "max-tokens") finish = "length";
        }
      }
      const input = usage
        ? (usage.inputTokens ?? 0) +
          (usage.cacheReadTokens ?? 0) +
          (usage.cacheWriteTokens ?? 0)
        : 0;
      if (!text.trim() && !calls.length) {
        const detail =
          finish === "length"
            ? "输出 token 额度已用尽，通常是被思考内容占满"
            : `finish_reason=${finish}`;
        throw new Error(
          `模型未返回正文或工具调用（${detail}；思考 ${reasoning.length} 字符）`,
        );
      }
      const output = usage?.outputTokens ?? 0;
      const usageOut = usage
        ? {
            prompt_tokens: input,
            completion_tokens: output,
            total_tokens: Math.max(usage.totalTokens ?? 0, input + output),
            prompt_tokens_details: {
              cached_tokens: usage.cacheReadTokens ?? 0,
            },
            completion_tokens_details: {
              reasoning_tokens: usage.reasoningTokens ?? 0,
            },
          }
        : undefined;
      if (body.stream) {
        if (!res.headersSent) send({ role: "assistant" });
        res.write(
          `data: ${JSON.stringify({ id, object: "chat.completion.chunk", created, model: body.model, choices: [{ index: 0, delta: {}, finish_reason: finish }], usage: usageOut })}\n\ndata: [DONE]\n\n`,
        );
        res.end();
      } else
        json({
          id,
          object: "chat.completion",
          created,
          model: body.model,
          choices: [
            {
              index: 0,
              message: {
                role: "assistant",
                content: text || null,
                ...(reasoning ? { reasoning_content: reasoning } : {}),
                ...(calls.length ? { tool_calls: calls } : {}),
              },
              finish_reason: finish,
            },
          ],
          usage: usageOut,
        });
    } catch (error) {
      if (controller.signal.aborted) return;
      if (res.headersSent)
        res.end(
          `data: ${JSON.stringify({ error: { message: safeError(error) } })}\n\n`,
        );
      else {
        res.statusCode = error.statusCode ?? 502;
        json({ error: { message: safeError(error) } });
      }
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", resolve);
  });
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    async close() {
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
      await runtime.close();
    },
  };
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  const dataDir = path.resolve(process.argv[2]);
  fs.mkdirSync(dataDir, { recursive: true, mode: 0o700 });
  const lockFile = path.join(dataDir, "service.lock"),
    runtimeFile = path.join(dataDir, "runtime.json");
  try {
    const old = JSON.parse(fs.readFileSync(lockFile, "utf8"));
    try {
      process.kill(old.pid, 0);
      throw new Error("账号渠道服务已在运行");
    } catch (error) {
      if (error.code !== "ESRCH") throw error;
    }
    fs.unlinkSync(lockFile);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  fs.writeFileSync(lockFile, JSON.stringify({ pid: process.pid }), {
    flag: "wx",
    mode: 0o600,
  });
  process.on("exit", () => {
    try {
      if (JSON.parse(fs.readFileSync(lockFile, "utf8")).pid === process.pid) {
        fs.unlinkSync(lockFile);
        fs.unlinkSync(runtimeFile);
      }
    } catch {}
  });
  const app = await serve({ dataDir, key: process.env.CODEPLUSE_CHANNEL_KEY });
  fs.writeFileSync(
    runtimeFile,
    JSON.stringify({
      url: app.url,
      key: process.env.CODEPLUSE_CHANNEL_KEY,
      pid: process.pid,
    }),
    { mode: 0o600 },
  );
  process.stdout.write(`${JSON.stringify({ url: app.url })}\n`);
  const shutdown = () => {
    void app.close().finally(() => process.exit(0));
  };
  process.stdin.resume();
  process.stdin.on("end", shutdown);
  process.on("SIGTERM", shutdown);
  process.on("SIGINT", shutdown);
}
