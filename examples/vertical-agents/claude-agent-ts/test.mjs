import http from "node:http";
import { answer } from "./agent.mjs";
import { assertLiveAnswer, smokePrompt } from "../runtime.mjs";

function mock() {
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      if ((req.url || "").includes("/mcp")) {
        const body = JSON.parse(Buffer.concat(chunks).toString() || "{}");
        res.writeHead(200, {
          "content-type": "application/json",
          "mcp-session-id": "mock-session",
        });
        res.end(
          JSON.stringify({
            jsonrpc: "2.0",
            id: body.id ?? null,
            result:
              body.method === "initialize"
                ? {
                    protocolVersion: "2025-06-18",
                    capabilities: { tools: {} },
                    serverInfo: { name: "platform", version: "1.0.0" },
                  }
                : { tools: [] },
          }),
        );
        return;
      }
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          id: "msg_mock",
          type: "message",
          role: "assistant",
          content: [{ type: "text", text: "cited item-7" }],
          stop_reason: "end_turn",
          usage: { input_tokens: 1, output_tokens: 1 },
        }),
      );
    });
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}

if (process.env.LIVE === "1") {
  const text = await answer(smokePrompt());
  assertLiveAnswer(text);
} else {
  const server = await mock();
  const port = server.address().port;
  process.env.OPENAI_BASE_URL = process.env.BASE_URL = `http://127.0.0.1:${port}`;
  process.env.OPENAI_API_KEY = process.env.API_KEY = "test-key";
  process.env.MCP_URL = `http://127.0.0.1:${port}/mcp`;
  try {
    const text = await answer("Read the cited page.");
    if (!text.includes("cited item-7")) throw new Error(`unexpected answer: ${text}`);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}
console.log("claude-agent-ts ok");
