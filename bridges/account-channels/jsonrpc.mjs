import readline from "node:readline";

export function jsonRpc(
  input,
  output,
  handler = async () => {
    throw new Error("Unsupported method");
  },
) {
  const pending = new Map();
  let sequence = 0,
    closed = false;
  const send = (value) => {
    if (!closed)
      output.write(`${JSON.stringify({ jsonrpc: "2.0", ...value })}\n`);
  };
  const lines = readline.createInterface({ input, crlfDelay: Infinity });
  lines.on("line", (line) => {
    if (line.length > 8 * 1024 * 1024) return;
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      return;
    }
    if (!message.method) {
      const call = pending.get(message.id);
      if (call) {
        pending.delete(message.id);
        call.finish();
        message.error
          ? call.reject(new Error(message.error.message))
          : call.resolve(message.result);
      }
    } else {
      void Promise.resolve()
        .then(() => handler(message.method, message.params ?? {}))
        .then(
          (result) => {
            if (message.id !== undefined)
              send({ id: message.id, result: result ?? {} });
          },
          (error) => {
            if (message.id !== undefined)
              send({
                id: message.id,
                error: { code: -32000, message: error.message },
              });
          },
        );
    }
  });
  const close = () => {
    if (closed) return;
    closed = true;
    for (const call of pending.values()) {
      call.finish();
      call.reject(new Error("RPC connection closed"));
    }
    pending.clear();
  };
  lines.on("close", close);
  return {
    notify(method, params) {
      send({ method, params });
    },
    call(method, params, signal) {
      if (closed) return Promise.reject(new Error("RPC connection closed"));
      if (signal?.aborted) return Promise.reject(new Error("Cancelled"));
      const id = ++sequence;
      return new Promise((resolve, reject) => {
        const abort = () => {
          pending.delete(id);
          signal?.removeEventListener("abort", abort);
          send({
            method: "notifications/cancelled",
            params: { requestId: id, reason: "Cancelled" },
          });
          reject(new Error("Cancelled"));
        };
        pending.set(id, {
          resolve,
          reject,
          finish: () => signal?.removeEventListener("abort", abort),
        });
        signal?.addEventListener("abort", abort, { once: true });
        send({ id, method, params });
      });
    },
    close() {
      close();
      lines.close();
    },
  };
}
