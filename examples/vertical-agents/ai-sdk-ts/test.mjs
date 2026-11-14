import http from "node:http";
import { answer } from "./agent.mjs";
import { assertLiveAnswer, smokePrompt } from "../runtime.mjs";

function frames() {
  const events = [
    { type: "start", messageId: "msg-smoke" },
    { type: "start-step" },
    { type: "text-start", id: "txt_msg-smoke" },
    { type: "text-delta", id: "txt_msg-smoke", delta: "cited item-7" },
    { type: "text-end", id: "txt_msg-smoke" },
    { type: "finish-step" },
    { type: "finish", finishReason: "stop" },
  ];
  return `${events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("")}data: [DONE]\n\n`;
}

function mock() {
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      if ((req.url || "").includes("/mcp")) {
        const body = JSON.parse(Buffer.concat(chunks).toString() || "{}");
        res.writeHead(200, { "content-type": "application/json" });
        res.end(
          JSON.stringify({
            jsonrpc: "2.0",
            id: body.id,
            result: { tools: [{ name: "space_read_page", inputSchema: { type: "object" } }] },
          }),
        );
        return;
      }
      if ((req.url || "").includes("/api/v1/ai-sdk/chat")) {
        res.writeHead(200, {
          "content-type": "text/event-stream",
          "x-vercel-ai-ui-message-stream": "v1",
        });
        res.end(frames());
        return;
      }
      if ((req.url || "").includes("/responses")) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(
          JSON.stringify({
            id: "resp_mock",
            object: "response",
            created_at: 0,
            status: "completed",
            model: "gpt-4.1-mini",
            output: [
              {
                type: "message",
                id: "msg_1",
                status: "completed",
                role: "assistant",
                content: [
                  { type: "output_text", text: "cited item-7", annotations: [] },
                ],
              },
            ],
            output_text: "cited item-7",
            usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 },
          }),
        );
        return;
      }
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          id: "chatcmpl-mock",
          object: "chat.completion",
          choices: [
            {
              index: 0,
              message: { role: "assistant", content: "cited item-7" },
              finish_reason: "stop",
            },
          ],
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
console.log("ai-sdk-ts ok");
