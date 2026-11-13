import readline from "node:readline";
const input = readline.createInterface({ input: process.stdin });
input.on("line", (line) => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  let result = {};
  if (request.method === "initialize")
    result = {
      protocolVersion: "2024-11-05",
      capabilities: { tools: {} },
      serverInfo: { name: "test-team", version: "1" },
    };
  if (request.method === "tools/list")
    result = {
      tools: [
        {
          name: "check_user_feedback",
          description: "Read teammate progress",
          inputSchema: { type: "object", properties: {} },
        },
      ],
    };
  if (request.method === "tools/call")
    result = {
      content: [{ type: "text", text: "peer-task: review completed" }],
    };
  process.stdout.write(
    `${JSON.stringify({ jsonrpc: "2.0", id: request.id, result })}\n`,
  );
});
