import { pathToFileURL } from "node:url";

const sdkRoot = process.env.MCP_SDK_ROOT ?? new URL("./mcp-client-home/node_modules/@modelcontextprotocol/sdk/", import.meta.url).pathname;
const { Client } = await import(pathToFileURL(`${sdkRoot}/dist/esm/client/index.js`).href);
const { StreamableHTTPClientTransport } = await import(pathToFileURL(`${sdkRoot}/dist/esm/client/streamableHttp.js`).href);

const base = process.env.MCP_URL ?? "http://127.0.0.1:8096/mcp";
const token = process.env.MCP_BEARER ?? "";
const title = process.env.MCP_PAGE_TITLE ?? "M2 page";

const anonymous = await fetch(base, {
  method: "POST",
  headers: { "content-type": "application/json", accept: "application/json" },
  body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "server/discover", params: {} }),
});
if (anonymous.status !== 401 || !String(anonymous.headers.get("www-authenticate") ?? "").includes("resource_metadata")) {
  console.error("expected 401 with resource_metadata");
  process.exit(1);
}

await legacy();
await current();
console.log("mcp-smoke ok");

async function legacy() {
  const transport = new StreamableHTTPClientTransport(new URL(base), {
    requestInit: { headers: { authorization: `Bearer ${token}` } },
  });
  const client = new Client({ name: "agentforge-mcp-smoke", version: "1" });
  await client.connect(transport);
  const listed = await client.listTools();
  if (!listed.tools.some((tool) => tool.name === "space_search")) throw new Error("legacy tools/list");
  const called = await client.callTool({ name: "space_search", arguments: { query: title } });
  const text = JSON.stringify(called);
  if (!text.includes(title)) throw new Error("legacy space_search");
  await client.close();
}

async function current() {
  const transport = new StreamableHTTPClientTransport(new URL(base), {
    requestInit: { headers: { authorization: `Bearer ${token}` } },
    fetch: async (url, init) => {
      const body = JSON.parse(String(init?.body ?? "{}"));
      const headers = new Headers(init?.headers);
      headers.set("mcp-protocol-version", "2026-07-28");
      headers.set("mcp-method", String(body.method ?? ""));
      body.params ??= {};
      body.params._meta = {
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": body.params.capabilities ?? {},
      };
      return fetch(url, { ...init, headers, body: JSON.stringify(body) });
    },
  });
  const result = await rpc(transport, "server/discover", {});
  if (!JSON.stringify(result).includes("2026-07-28")) throw new Error("discover");
  const tools = await rpc(transport, "tools/list", {});
  if (!JSON.stringify(tools).includes("space_search")) throw new Error("current tools/list");
  const found = await rpc(transport, "tools/call", { name: "space_search", arguments: { query: title } });
  if (!JSON.stringify(found).includes(title)) throw new Error("current space_search");
  await transport.close();
}

function rpc(transport, method, params) {
  return new Promise((resolve, reject) => {
    const id = Math.floor(Math.random() * 1_000_000);
    transport.onmessage = (message) => {
      if (message.id === id) resolve(message.result ?? message);
    };
    transport.onerror = reject;
    transport.send({ jsonrpc: "2.0", id, method, params }).catch(reject);
  });
}
