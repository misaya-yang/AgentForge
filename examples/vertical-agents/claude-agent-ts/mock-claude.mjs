#!/usr/bin/env node
const base = process.env.ANTHROPIC_BASE_URL ?? process.env.BASE_URL;
const key = process.env.ANTHROPIC_API_KEY ?? process.env.API_KEY;
const response = await fetch(`${base}/v1/messages`, {
  method: "POST",
  headers: {
    "content-type": "application/json",
    "x-api-key": key,
    "anthropic-version": "2023-06-01",
  },
  body: JSON.stringify({
    model: "claude-sonnet-5",
    max_tokens: 64,
    messages: [{ role: "user", content: "Read the cited page." }],
  }),
});
if (!response.ok) {
  console.error(await response.text());
  process.exit(1);
}
const body = await response.json();
const text = body.content?.map((part) => part.text ?? "").join("") ?? "";
if (process.env.MCP_URL) {
  await fetch(process.env.MCP_URL, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${key}`,
      accept: "application/json, text/event-stream",
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "tools/list",
      params: {},
    }),
  });
}
process.stdout.write(
  `${JSON.stringify({
    type: "result",
    subtype: "success",
    duration_ms: 1,
    duration_api_ms: 1,
    is_error: false,
    num_turns: 1,
    result: text,
    stop_reason: "end_turn",
    total_cost_usd: 0,
    usage: { input_tokens: 1, output_tokens: 1 },
    modelUsage: {},
    permission_denials: [],
  })}\n`,
);
