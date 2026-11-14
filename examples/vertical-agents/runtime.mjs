// Public configuration only. Provider credentials belong to the gateway, never to these clients.
export function config() {
  const baseURL = (process.env.OPENAI_BASE_URL || process.env.BASE_URL || "http://127.0.0.1:8080/v1").replace(/\/+$/, "");
  const apiKey = process.env.OPENAI_API_KEY || process.env.API_KEY;
  if (!apiKey) throw new Error("Set OPENAI_API_KEY to a platform gateway key");
  const origin = new URL(baseURL);
  if (process.env.LIVE === "1" && !["127.0.0.1", "localhost", "[::1]"].includes(origin.hostname)) {
    throw new Error("LIVE examples must use the local platform gateway, not a provider endpoint");
  }
  return { baseURL, apiKey, model: process.env.MODEL || "qwen3.8-flash", mcpURL: process.env.MCP_URL || `${origin.origin}/mcp` };
}

export const evidence = { calls: [], itemIds: new Set() };
export function resetEvidence() { evidence.calls.length = 0; evidence.itemIds.clear(); }
export function recordTool(name, result) {
  if (result?.isError) throw new Error(`Platform tool ${name} failed`);
  evidence.calls.push(name);
  function visit(value) {
    if (!value || typeof value !== "object") return;
    if (typeof value.item_id === "string") evidence.itemIds.add(value.item_id);
    if (typeof value.itemId === "string") evidence.itemIds.add(value.itemId);
    for (const child of Object.values(value)) {
      if (typeof child === "string") {
        try { visit(JSON.parse(child)); } catch { /* Tool text need not be JSON. */ }
      } else visit(child);
    }
  }
  visit(result);
  return result;
}
export function assertLiveAnswer(text) {
  if (!evidence.calls.length) throw new Error("LIVE agent did not execute a platform tool");
  if (![...evidence.itemIds].some(id => text.includes(id))) {
    throw new Error("LIVE answer must cite an item id actually returned by the platform tool; seed an accessible space page and set QUERY or ITEM_ID");
  }
}
export function smokePrompt() {
  return process.env.PROMPT || (process.env.ITEM_ID
    ? `Use space_read_page to read item ${process.env.ITEM_ID}. Summarize it and cite its exact item_id in [item_id] brackets.`
    : `Use space_search to search for ${JSON.stringify(process.env.QUERY || "platform")}. Summarize one matching page and cite its exact item_id in [item_id] brackets. Do not invent sources.`);
}

// The platform's stateless legacy MCP transport. JSON and SSE responses are both accepted.
export async function platformTools() {
  const c = config();
  let session;
  let id = 0;
  async function rpc(method, params, notification = false) {
    const response = await fetch(c.mcpURL, {
      method: "POST",
      headers: { authorization: `Bearer ${c.apiKey}`, "content-type": "application/json", accept: "application/json, text/event-stream", "mcp-protocol-version": "2025-11-25", ...(session ? { "mcp-session-id": session } : {}) },
      body: JSON.stringify({ jsonrpc: "2.0", ...(notification ? {} : { id: ++id }), method, params }),
      signal: AbortSignal.timeout(30000),
    });
    if (!response.ok) throw new Error(`Platform MCP ${method} failed (${response.status})`);
    session = response.headers.get("mcp-session-id") || session;
    if (notification || response.status === 202 || response.status === 204) return {};
    const raw = await response.text();
    const body = response.headers.get("content-type")?.includes("text/event-stream")
      ? raw.split(/\r?\n/).filter(line => line.startsWith("data:")).map(line => line.slice(5).trim()).filter(line => line && line !== "[DONE]").map(line => JSON.parse(line)).find(value => value.id === id)
      : JSON.parse(raw);
    if (!body || body.error) throw new Error(`Platform MCP ${method} returned an error`);
    return body.result;
  }
  await rpc("initialize", { protocolVersion: "2025-11-25", capabilities: {}, clientInfo: { name: "agentforge-example", version: "1.0.0" } });
  await rpc("notifications/initialized", {}, true);
  const listed = await rpc("tools/list", {});
  // This sample is read-only. Writing pages requires the separate approval flow.
  return (listed.tools || []).filter(tool => ["space_list", "space_search", "space_read_page"].includes(tool.name)).map(tool => ({
    ...tool,
    execute: async args => recordTool(tool.name, await rpc("tools/call", { name: tool.name, arguments: args })),
  }));
}

// ADK's BaseLlm bridge and the documented Claude OpenAI alternative share this protocol path.
export async function chatWithTools(prompt) {
  const c = config();
  const tools = await platformTools();
  const messages = [{ role: "system", content: "Use the platform tools to answer. Cite only exact item_id values returned by tools in [item_id] brackets." }, { role: "user", content: prompt }];
  for (let turn = 0; turn < 8; turn++) {
    const response = await fetch(`${c.baseURL}/chat/completions`, {
      method: "POST", headers: { authorization: `Bearer ${c.apiKey}`, "content-type": "application/json" },
      body: JSON.stringify({ model: c.model, messages, ...(tools.length ? { tools: tools.map(tool => ({ type: "function", function: { name: tool.name, description: tool.description, parameters: tool.inputSchema } })) } : {}) }),
      signal: AbortSignal.timeout(120000),
    });
    if (!response.ok) throw new Error(`Platform chat failed (${response.status})`);
    const body = await response.json();
    const message = body.choices?.[0]?.message;
    if (!message) throw new Error("Platform returned no assistant message");
    messages.push(message);
    if (!message.tool_calls?.length) return message.content || "";
    for (const call of message.tool_calls) {
      const tool = tools.find(tool => tool.name === call.function.name);
      if (!tool) throw new Error("Model requested a tool outside the read-only sample");
      const result = await tool.execute(JSON.parse(call.function.arguments));
      messages.push({ role: "tool", tool_call_id: call.id, content: JSON.stringify(result) });
    }
  }
  throw new Error("Agent exceeded eight tool rounds");
}
