import { createServer } from "node:http";
import { afterEach, describe, expect, it } from "vitest";

import {
  LOCAL_PROXY_TOKEN_ENV,
  responsesToChat,
  startChatCompatibilityProxy,
  type ChatCompatibilityProxy,
} from "./chat_responses_proxy.js";
import type { ProviderProfile } from "./config.js";

const cleanups: Array<() => Promise<void>> = [];
afterEach(async () => {
  while (cleanups.length) await cleanups.pop()!();
});

async function mockChatProvider(
  handler: (request: { url: string; headers: Record<string, string | string[] | undefined>; body: any; attempt: number }) => {
    status?: number;
    chunks?: string[];
    holdOpen?: boolean;
    onClose?: () => void;
  },
) {
  let attempt = 0;
  const server = createServer(async (request, response) => {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const result = handler({
      url: request.url ?? "",
      headers: request.headers,
      body: JSON.parse(Buffer.concat(chunks).toString("utf8")),
      attempt: ++attempt,
    });
    response.writeHead(result.status ?? 200, { "Content-Type": "text/event-stream" });
    if (result.holdOpen) response.flushHeaders();
    for (const chunk of result.chunks ?? []) response.write(chunk);
    if (result.holdOpen) response.once("close", () => result.onClose?.());
    else response.end();
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("mock provider did not bind");
  cleanups.push(() => new Promise<void>((resolve) => {
    server.close(() => resolve());
    server.closeAllConnections();
  }));
  return { baseUrl: `http://127.0.0.1:${address.port}/v1`, attempts: () => attempt };
}

function profile(baseUrl: string): ProviderProfile {
  return {
    name: "Chat-only provider",
    model: "chat-model",
    base_url: baseUrl,
    wire_api: "chat_completions",
    auth: { type: "bearer", api_key_env: "CHAT_PROVIDER_KEY" },
    query_params: { "api-version": "2026-08-01" },
    request_max_retries: 1,
    stream_idle_timeout_ms: 2_000,
    allow_insecure_localhost: true,
  };
}

async function startProxy(provider: ProviderProfile): Promise<ChatCompatibilityProxy> {
  const proxy = await startChatCompatibilityProxy(provider, { CHAT_PROVIDER_KEY: "synthetic-secret" });
  cleanups.push(() => proxy.close());
  return proxy;
}

describe("Responses to Chat Completions compatibility", () => {
  it("projects text, reasoning, usage, and tool deltas into Responses SSE", async () => {
    const provider = await mockChatProvider(({ url, headers, body }) => {
      expect(url).toBe("/v1/chat/completions?api-version=2026-08-01");
      expect(headers.authorization).toBe("Bearer synthetic-secret");
      expect(body.model).toBe("chat-model");
      expect(body.messages).toEqual([{ role: "user", content: "hello" }]);
      expect(body.tools[0].function.name).toBe("lookup");
      return { chunks: [
        'data: {"choices":[{"delta":{"reasoning_content":"think","content":"Hi "}}]}\r\n\r\n',
        'data: {"choices":[{"delta":{"content":"there","tool_calls":[{"index":0,"function":{"name":"lookup","arguments":"{\\"q\\":"}}]}}]}\n\n',
        'data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"arguments":"\\"x\\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}}\n\n',
        "data: [DONE]\n\n",
      ] };
    });
    const proxy = await startProxy(profile(provider.baseUrl));
    const response = await fetch(`${proxy.baseUrl}/responses`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${proxy.token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        model: "chat-model",
        input: "hello",
        stream: true,
        tools: [{ type: "function", name: "lookup", description: "Lookup", parameters: { type: "object" } }],
      }),
    });
    expect(response.status).toBe(200);
    const text = await response.text();
    expect(text).toContain("event: response.created");
    expect(text).toContain('"delta":"Hi "');
    expect(text).toContain("response.reasoning_summary_text.delta");
    expect(text).toContain('"text":"think"');
    expect(text).toContain('"name":"lookup"');
    expect(text).toContain('"arguments":"{\\"q\\":\\"x\\"}"');
    expect(text).toContain('"input_tokens":3');
    expect(text).toContain("event: response.completed");
    expect(text).not.toContain("synthetic-secret");
    const events = parseSseEvents(text);
    const toolEvents = events.filter((event) => String(event.type).includes("function_call") || event.item?.type === "function_call");
    expect(new Set(toolEvents.map((event) => event.item_id ?? event.item?.call_id))).toEqual(new Set(["call_1"]));
  });

  it("translates tool transcript input for the next agent turn", () => {
    const result = responsesToChat({
      model: "chat-model",
      stream: true,
      instructions: "be concise",
      input: [
        { type: "message", role: "user", content: [{ type: "input_text", text: "find x" }] },
        { type: "function_call", call_id: "call_1", name: "lookup", arguments: "{\"q\":\"x\"}" },
        { type: "function_call_output", call_id: "call_1", output: "found" },
      ],
    });
    expect(result.messages).toEqual([
      { role: "system", content: "be concise" },
      { role: "user", content: "find x" },
      { role: "assistant", content: "", tool_calls: [{ id: "call_1", type: "function", function: { name: "lookup", arguments: "{\"q\":\"x\"}" } }] },
      { role: "tool", tool_call_id: "call_1", name: "lookup", content: "found" },
    ]);

    const contentItems = responsesToChat({
      model: "chat-model",
      stream: true,
      input: [
        { type: "function_call", call_id: "call_2", name: "lookup", arguments: "{}" },
        { type: "function_call_output", call_id: "call_2", output: [
          { type: "input_text", text: "part one" },
          { type: "input_text", text: " and two" },
        ] },
      ],
    });
    expect((contentItems.messages as any[])[1].content).toBe("part one and two");
    expect(() => responsesToChat({
      model: "chat-model",
      stream: true,
      input: [
        { type: "function_call", call_id: "call_3", name: "lookup", arguments: "{}" },
        { type: "function_call_output", call_id: "call_3", output: [
          { type: "input_image", image_url: "data:image/png;base64,eA==" },
        ] },
      ],
    })).toThrow(/responses_function_output_unsupported/);
  });

  it("flattens Runtime namespaces for Chat providers", () => {
    const result = responsesToChat({
      model: "chat-model",
      stream: true,
      input: "hello",
      tools: [
        {
          type: "namespace",
          name: "mcp",
          tools: [
            {
              type: "function",
              name: "write",
              description: "Write one value",
              parameters: { type: "object", properties: {} },
            },
          ],
        },
      ],
    });

    expect((result.tools as any[]).map((tool) => tool.function.name)).toEqual([expect.stringMatching(/^ns_[a-f0-9]{10}_write$/)]);
  });

  it("fails closed for hosted tools that Chat providers cannot represent", () => {
    expect(() => responsesToChat({
      model: "chat-model",
      stream: true,
      input: "search the web",
      tools: [{ type: "web_search" }],
    })).toThrow(/responses_tool_unsupported/);
  });

  it("fails closed for image input instead of silently dropping it", () => {
    expect(() => responsesToChat({
      model: "chat-model",
      stream: true,
      input: [{ type: "message", role: "user", content: [{ type: "input_image", image_url: "https://example.test/x.png" }] }],
    })).toThrow(/responses_content_unsupported/);
  });

  it("rejects unknown fields, opaque reasoning history, and non-boolean parallel tools", () => {
    expect(() => responsesToChat({
      model: "chat-model", input: "hello", stream: true, previous_response_id: "resp_1",
    } as any)).toThrow(/responses_fields_unsupported/);
    expect(() => responsesToChat({
      model: "chat-model", stream: true, input: [{ type: "reasoning", summary: [], encrypted_content: "opaque" }],
    })).toThrow(/responses_reasoning_history_unsupported/);
    expect(() => responsesToChat({
      model: "chat-model", stream: true, input: "hello",
      tools: [{ type: "function", name: "lookup" }], parallel_tool_calls: "false",
    })).toThrow(/responses_parallel_tool_calls_invalid/);
  });

  it("retries a pre-stream 429 once and never exposes the provider error body", async () => {
    const provider = await mockChatProvider(({ attempt }) => attempt === 1
      ? { status: 429, chunks: ["provider private diagnostic"] }
      : { chunks: ['data: {"choices":[{"delta":{"content":"ok"}}]}\n\ndata: [DONE]\n\n'] });
    const proxy = await startProxy(profile(provider.baseUrl));
    const response = await fetch(`${proxy.baseUrl}/responses`, {
      method: "POST",
      headers: { Authorization: `Bearer ${proxy.token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ model: "chat-model", input: "hello", stream: true }),
    });
    const text = await response.text();
    expect(provider.attempts()).toBe(2);
    expect(text).toContain('"delta":"ok"');
    expect(text).not.toContain("provider private diagnostic");
  });

  it("rejects callers without the ephemeral loopback token", async () => {
    const provider = await mockChatProvider(() => ({ chunks: [] }));
    const proxy = await startProxy(profile(provider.baseUrl));
    const response = await fetch(`${proxy.baseUrl}/responses`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: "chat-model", input: "hello", stream: true }),
    });
    expect(response.status).toBe(401);
    expect(await response.text()).not.toContain(LOCAL_PROXY_TOKEN_ENV);
  });

  it("fails an incomplete provider stream instead of fabricating completion", async () => {
    const provider = await mockChatProvider(() => ({
      chunks: ['data: {"choices":[{"delta":{"content":"partial"}}]}\n\n'],
    }));
    const proxy = await startProxy(profile(provider.baseUrl));
    const response = await fetch(`${proxy.baseUrl}/responses`, {
      method: "POST",
      headers: { Authorization: `Bearer ${proxy.token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ model: "chat-model", input: "hello", stream: true }),
    });
    const text = await response.text();
    expect(text).toContain('"delta":"partial"');
    expect(text).toContain("provider_stream_incomplete");
    expect(text).toContain("event: response.failed");
    expect(text).not.toContain("event: response.completed");
    const sequences = [...text.matchAll(/"sequence_number":(\d+)/g)].map((match) => Number(match[1]));
    expect(sequences).toEqual(sequences.map((_, index) => index));
  });

  it.each(["length", "content_filter"])("projects finish_reason=%s as failure", async (finishReason) => {
    const provider = await mockChatProvider(() => ({
      chunks: [
        `data: {"choices":[{"delta":{"content":"partial"},"finish_reason":"${finishReason}"}]}\n\n`,
        "data: [DONE]\n\n",
      ],
    }));
    const proxy = await startProxy(profile(provider.baseUrl));
    const response = await fetch(`${proxy.baseUrl}/responses`, {
      method: "POST",
      headers: { Authorization: `Bearer ${proxy.token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ model: "chat-model", input: "hello", stream: true }),
    });
    const text = await response.text();
    expect(text).toContain(`provider_finish_${finishReason}`);
    expect(text).toContain("event: response.failed");
    expect(text).not.toContain("event: response.completed");
  });

  it("cancels the provider stream when the native client disconnects", async () => {
    let providerClosed!: () => void;
    const closed = new Promise<void>((resolve) => { providerClosed = resolve; });
    const provider = await mockChatProvider(() => ({
      chunks: ['data: {"choices":[{"delta":{"content":"first"}}]}\n\n'],
      holdOpen: true,
      onClose: providerClosed,
    }));
    const proxy = await startProxy(profile(provider.baseUrl));
    const controller = new AbortController();
    const response = await fetch(`${proxy.baseUrl}/responses`, {
      method: "POST",
      headers: { Authorization: `Bearer ${proxy.token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ model: "chat-model", input: "hello", stream: true }),
      signal: controller.signal,
    });
    await response.body!.getReader().read();
    controller.abort();
    await expect(Promise.race([
      closed.then(() => "closed"),
      new Promise<string>((resolve) => setTimeout(() => resolve("timeout"), 750)),
    ])).resolves.toBe("closed");
  });

  it("fails an idle stream and closes the provider connection", async () => {
    let providerClosed!: () => void;
    const closed = new Promise<void>((resolve) => { providerClosed = resolve; });
    const provider = await mockChatProvider(() => ({
      holdOpen: true,
      onClose: providerClosed,
    }));
    const chatProfile = profile(provider.baseUrl);
    chatProfile.stream_idle_timeout_ms = 50;
    const proxy = await startProxy(chatProfile);
    const response = await fetch(`${proxy.baseUrl}/responses`, {
      method: "POST",
      headers: { Authorization: `Bearer ${proxy.token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ model: "chat-model", input: "hello", stream: true }),
    });
    const text = await response.text();
    expect(text).toContain("provider_stream_idle_timeout");
    expect(text).toContain("event: response.failed");
    expect(text).not.toContain("event: response.completed");
    await expect(Promise.race([
      closed.then(() => "closed"),
      new Promise<string>((resolve) => setTimeout(() => resolve("timeout"), 750)),
    ])).resolves.toBe("closed");
  });

  it.each([
    {
      name: "changed tool id",
      chunks: [
        'data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"lookup","arguments":"{}"}}]}}]}\n\n',
        'data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_2","function":{"arguments":""}}]},"finish_reason":"tool_calls"}]}\n\n',
        "data: [DONE]\n\n",
      ],
      code: "provider_tool_id_changed",
    },
    {
      name: "missing tool id",
      chunks: [
        'data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"lookup","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}\n\n',
        "data: [DONE]\n\n",
      ],
      code: "provider_tool_identity_missing",
    },
  ])("fails closed for $name", async ({ chunks, code }) => {
    const provider = await mockChatProvider(() => ({ chunks }));
    const proxy = await startProxy(profile(provider.baseUrl));
    const response = await fetch(`${proxy.baseUrl}/responses`, {
      method: "POST",
      headers: { Authorization: `Bearer ${proxy.token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ model: "chat-model", input: "hello", stream: true }),
    });
    const text = await response.text();
    expect(text).toContain(code);
    expect(text).toContain("event: response.failed");
    expect(text).not.toContain("event: response.completed");
  });
});


describe("tool identity and authorization boundaries", () => {
  const requestBody = (tools: unknown[], input: unknown = "hello") => ({ model: "chat-model", input, stream: true, tools });
  const functions = (names: string[]) => names.map(name => ({ type: "function", name, parameters: { type: "object" } }));
  const wireFrame = (toolCalls: unknown[], finished = false) => `data: ${JSON.stringify({ choices: [{ delta: { tool_calls: toolCalls }, ...(finished ? { finish_reason: "tool_calls" } : {}) }] })}\n\n`;
  const post = (proxy: ChatCompatibilityProxy, body: unknown) => fetch(`${proxy.baseUrl}/responses`, {
    method: "POST", headers: { Authorization: `Bearer ${proxy.token}`, "Content-Type": "application/json" }, body: JSON.stringify(body),
  });

  it.each([
    { name: "get_data", parts: ["get", "_data"] },
    { name: "get", parts: ["get"] },
  ])("projects the exact $name identity when allowed tool names share a prefix", async ({ name, parts }) => {
    const provider = await mockChatProvider(() => ({ chunks: [
      ...parts.map(part => wireFrame([{ index: 0, id: "call_prefix", function: { name: part } }])),
      wireFrame([{ index: 0, function: { arguments: '{"key":1}' } }], true),
      "data: [DONE]\n\n",
    ] }));
    const proxy = await startProxy(profile(provider.baseUrl));
    const response = await post(proxy, requestBody(functions(["get", "get_data"])));
    const events = parseSseEvents(await response.text());
    const added = events.filter(event => event.type === "response.output_item.added" && event.item?.type === "function_call");
    const terminal = events.filter(event => event.type === "response.output_item.done" && event.item?.type === "function_call");
    expect(added.map(event => event.item.name)).toEqual([name]);
    expect(terminal.map(event => ({ name: event.item.name, arguments: event.item.arguments }))).toEqual([{ name, arguments: '{"key":1}' }]);
    expect(events.filter(event => event.type === "response.completed").length).toBe(1);
    expect(events.some(event => event.type === "response.failed")).toBe(false);
  });

  it("rejects a call ID reused across indexes before a successful terminal", async () => {
    const provider = await mockChatProvider(() => ({ chunks: [wireFrame([
      { index: 0, id: "same_call", function: { name: "read_data", arguments: "{}" } },
      { index: 1, id: "same_call", function: { name: "write_data", arguments: "{}" } },
    ], true), "data: [DONE]\n\n"] }));
    const proxy = await startProxy(profile(provider.baseUrl));
    const response = await post(proxy, requestBody(functions(["read_data", "write_data"])));
    const events = parseSseEvents(await response.text());
    expect(events.filter(event => event.type === "response.failed").map(event => event.response.error.code)).toEqual(["provider_tool_id_duplicate"]);
    expect(events.some(event => event.type === "response.completed")).toBe(false);
    expect(events.some(event => event.type === "response.function_call_arguments.done")).toBe(false);
  });

  it("keeps same-named namespaces distinct through output and the next-turn transcript", async () => {
    const tools = ["files", "records"].map(name => ({ type: "namespace", name, tools: functions(["lookup"]) }));
    const requests: any[] = [];
    let aliases: string[] = [];
    const provider = await mockChatProvider(({ body, attempt }) => {
      requests.push(body);
      if (attempt > 1) return { chunks: ['data: {"choices":[{"delta":{"content":"finished"},"finish_reason":"stop"}]}\n\n', "data: [DONE]\n\n"] };
      aliases = body.tools.map((tool: any) => tool.function.name);
      return { chunks: [wireFrame(aliases.map((name, index) => ({ index, id: `call_${index}`, function: { name, arguments: "{}" } })), true), "data: [DONE]\n\n"] };
    });
    const proxy = await startProxy(profile(provider.baseUrl));
    const first = await post(proxy, requestBody(tools));
    const events = parseSseEvents(await first.text());
    const completed = events.find(event => event.type === "response.completed");
    const calls = completed.response.output.filter((item: any) => item.type === "function_call");
    expect(new Set(aliases).size).toBe(2);
    expect(calls.map((call: any) => ({ namespace: call.namespace, name: call.name }))).toEqual([
      { namespace: "files", name: "lookup" }, { namespace: "records", name: "lookup" },
    ]);
    const transcript = [...calls, ...calls.map((call: any) => ({ type: "function_call_output", call_id: call.call_id, output: "found" }))];
    const second = await post(proxy, requestBody(tools, transcript));
    expect((await second.text()).includes("event: response.completed")).toBe(true);
    const assistants = requests[1].messages.filter((message: any) => message.role === "assistant");
    expect(assistants).toHaveLength(1);
    expect(assistants[0].tool_calls.map((call: any) => call.function.name)).toEqual(aliases);
    expect(requests[1].messages.filter((message: any) => message.role === "tool").map((message: any) => ({ name: message.name, call_id: message.tool_call_id }))).toEqual(aliases.map((name, index) => ({ name, call_id: `call_${index}` })));
  });

  it.each([false, true])("tool_choice=none sends no callable tools even with history=%s", async (withHistory) => {
    let outbound: any;
    const provider = await mockChatProvider(({ body }) => {
      outbound = body;
      return { chunks: ['data: {"choices":[{"delta":{"content":"plain text"},"finish_reason":"stop"}]}\n\n', "data: [DONE]\n\n"] };
    });
    const proxy = await startProxy(profile(provider.baseUrl));
    const input = withHistory ? [
      { type: "function_call", call_id: "old_call", namespace: "files", name: "lookup", arguments: "{}" },
      { type: "function_call_output", call_id: "old_call", output: "old result" },
    ] : "hello";
    const response = await post(proxy, { ...requestBody([{ type: "namespace", name: "files", tools: functions(["lookup"]) }], input), tool_choice: "none", parallel_tool_calls: true });
    expect((await response.text()).includes("event: response.completed")).toBe(true);
    expect(outbound.tools).toBeUndefined();
    expect(outbound.tool_choice).toBeUndefined();
    expect(outbound.parallel_tool_calls).toBeUndefined();
  });
});

describe("Codex refresh multi-turn compatibility", () => {
  const post = (proxy: ChatCompatibilityProxy, input: unknown) => fetch(`${proxy.baseUrl}/responses`, {
    method: "POST",
    headers: { Authorization: `Bearer ${proxy.token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ model: "chat-model", input, stream: true }),
  });

  it("replays its own plain reasoning and assistant output on the next turn", async () => {
    const requests: any[] = [];
    const provider = await mockChatProvider(({ body }) => {
      requests.push(body);
      return { chunks: [
        'data: {"choices":[{"delta":{"reasoning_content":"first "}}]}\n\n',
        'data: {"choices":[{"delta":{"reasoning_content":"second","content":"answer"},"finish_reason":"stop"}]}\n\n',
        "data: [DONE]\n\n",
      ] };
    });
    const proxy = await startProxy(profile(provider.baseUrl));
    const first = parseSseEvents(await (await post(proxy, "question")).text());
    const output = first.find(event => event.type === "response.completed").response.output;
    const reasonDone = first.findIndex(event => event.type === "response.reasoning_summary_text.done");
    expect(first.slice(reasonDone).some(event => event.type === "response.reasoning_summary_text.delta")).toBe(false);
    expect(first[reasonDone].text).toBe("first second");
    const second = await post(proxy, [{ type: "message", role: "user", content: "question" }, ...output, { type: "message", role: "user", content: "continue" }]);
    expect(await second.text()).toContain("event: response.completed");
    expect(requests[1].messages).toEqual([
      { role: "user", content: "question" },
      { role: "assistant", content: "answer", reasoning_content: "first second" },
      { role: "user", content: "continue" },
    ]);
  });

  it("does not retry a transport failure after the provider accepted the request", async () => {
    let attempts = 0;
    const server = createServer(async (request) => {
      for await (const _chunk of request) { /* consume accepted request */ }
      attempts++;
      request.socket.destroy();
    });
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
    cleanups.push(() => new Promise<void>(resolve => server.close(() => resolve())));
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("fixture did not bind");
    const proxy = await startProxy(profile(`http://127.0.0.1:${address.port}/v1`));
    const response = await post(proxy, "question");
    expect(response.status).toBe(502);
    expect(await response.json()).toMatchObject({ error: { code: "provider_outcome_unknown" } });
    expect(attempts).toBe(1);
  });
});

function parseSseEvents(payload: string): any[] {
  return payload.split(/\r?\n\r?\n/).flatMap((frame) => {
    const data = frame.split(/\r?\n/).find((line) => line.startsWith("data:"));
    return data ? [JSON.parse(data.slice(5).trim())] : [];
  });
}
