import http from "node:http";
import { answer } from "./agent.mjs";
import { assertLiveAnswer, smokePrompt } from "../runtime.mjs";

function mock() {
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      const raw = Buffer.concat(chunks).toString() || "{}";
      if (req.method === "GET" || req.method === "DELETE") {
        res.writeHead(req.method === "DELETE" ? 204 : 405);
        res.end();
        return;
      }
      if ((req.url || "").includes("/mcp")) {
        const body = JSON.parse(raw);
        if (body.method === "notifications/initialized") {
          res.writeHead(202);
          res.end();
          return;
        }
        const result =
          body.method === "initialize"
            ? {
                protocolVersion: "2025-06-18",
                capabilities: { tools: {} },
                serverInfo: { name: "platform", version: "1.0.0" },
              }
            : body.method === "tools/list"
              ? { tools: [] }
              : { content: [{ type: "text", text: "cited item-7" }] };
        res.writeHead(200, {
          "content-type": "application/json",
          "mcp-session-id": "mock-session",
        });
        res.end(JSON.stringify({ jsonrpc: "2.0", id: body.id, result }));
        return;
      }
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          choices: [{ message: { role: "assistant", content: "cited item-7" } }],
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
  process.env.OPENAI_BASE_URL = process.env.BASE_URL = `http://127.0.0.1:${port}/v1`;
  process.env.OPENAI_API_KEY = process.env.API_KEY = "test-key";
  process.env.MCP_URL = `http://127.0.0.1:${port}/mcp`;
  try {
    const text = await answer("Read the cited page.");
    if (!text.includes("cited item-7")) throw new Error(`unexpected answer: ${text}`);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}
console.log("adk-ts ok");
