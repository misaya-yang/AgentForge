import { randomUUID } from "node:crypto";
import { expect, test } from "./support/real-api";
import { buildAuthHeaders, getApiUrl, readE2ETestUser } from "./support/helpers";

const MODERN = "2026-07-28";
const LEGACY = "2025-11-25";
type Tool = { name: string; annotations?: { readOnlyHint?: boolean }; inputSchema?: { type?: string } };
type Rpc = { jsonrpc: string; id: number; result?: { protocolVersion?: string; supportedVersions?: string[]; tools?: Tool[]; isError?: boolean; structuredContent?: Record<string, unknown> }; error?: { code: number; message: string } };

test("builtin MCP negotiates both protocols and reads a real owned page with a revocable knowledge key", async ({ request, baseURL }, testInfo) => {
  test.setTimeout(180_000);
  if (!baseURL || new URL(baseURL).protocol !== "https:") throw new Error("Live platform MCP requires trusted HTTPS");
  const api = getApiUrl();
  const endpoint = new URL("/mcp", baseURL).href;
  const admin = await buildAuthHeaders(request);
  const password = (await readE2ETestUser()).password;
  const marker = `mcp-browser-${Date.now()}`;
  const pageTitle = `${marker} page`;
  const markdown = `# ${marker}\n\nThis content belongs only to the isolated MCP browser verification.\n`;
  const transactions: Array<{ method: string; tool?: string; protocol: string; request_id: string; status: number; isError?: boolean }> = [];
  let keyId: string | undefined, spaceId: string | undefined, datasetId: string | undefined, key = "";
  let issuedAttempted = false, spaceAttempted = false, datasetAttempted = false;
  let legacySession: string | undefined;
  let keyHeader = "X-API-Key";
  let sequence = 1;
  const rpc = async (method: string, params: Record<string, unknown>, version = MODERN, notification = false) => {
    const requestId = randomUUID();
    const response = await request.post(endpoint, {
      headers: {
        [keyHeader]: key, "Content-Type": "application/json", Accept: "application/json, text/event-stream",
        "MCP-Protocol-Version": version, "MCP-Method": method, "X-Request-ID": requestId,
        ...(version === LEGACY && legacySession ? { "MCP-Session-Id": legacySession } : {}),
        ...(typeof params.name === "string" ? { "MCP-Name": params.name } : {}),
      },
      data: {
        jsonrpc: "2.0", ...(!notification ? { id: sequence++ } : {}), method,
        params: version === LEGACY ? params : {
          ...params, _meta: { "io.modelcontextprotocol/protocolVersion": MODERN, "io.modelcontextprotocol/clientCapabilities": {} },
        },
      },
    });
    transactions.push({ method, ...(typeof params.name === "string" ? { tool: params.name } : {}), protocol: version, request_id: requestId, status: response.status() });
    return response;
  };
  const call = async (name: string, args: Record<string, unknown>, version = MODERN) => {
    const response = await rpc("tools/call", { name, arguments: args }, version);
    expect(response.status(), `${name} HTTP status`).toBe(200);
    const body = await response.json() as Rpc;
    expect(body.error, `${name} RPC error`).toBeUndefined();
    expect(body.result?.isError, `${name} delegated REST outcome`).toBe(false);
    transactions.at(-1)!.isError = body.result?.isError;
    return body.result!.structuredContent!;
  };
  try {
    const metadata = await request.get(new URL("/.well-known/oauth-protected-resource/mcp", baseURL).href);
    expect(metadata.status()).toBe(200);
    const discoveryDocument = await metadata.json() as { resource: string; authorization_servers?: string[]; auth_methods_supported: string[]; api_key_header: string };
    expect(discoveryDocument.resource).toBe(endpoint);
    expect(discoveryDocument).not.toHaveProperty("authorization_servers");
    expect(discoveryDocument).not.toHaveProperty("bearer_methods_supported");
    expect(discoveryDocument.auth_methods_supported).toEqual(["api_key"]);
    expect(typeof discoveryDocument.api_key_header).toBe("string");
    keyHeader = discoveryDocument.api_key_header;
    expect(keyHeader.toLowerCase()).toBe("x-api-key");
    const unauthenticated = await request.post(endpoint, { data: { jsonrpc: "2.0", id: 0, method: "initialize", params: {} } });
    expect(unauthenticated.status()).toBe(401);
    expect(unauthenticated.headers()["www-authenticate"]).toContain(`ApiKey header="${keyHeader}"`);
    const jwt = await request.post(endpoint, { headers: admin, data: { jsonrpc: "2.0", id: 0, method: "initialize", params: {} } });
    expect(jwt.status(), "Explicit API-key-only mode must not accept a Console JWT").toBe(401);

    // The key is issued by the real API; its plaintext remains only in memory.
    issuedAttempted = true;
    const issued = await request.post(`${api}/api/v1/api-keys`, {
      headers: admin,
      data: { name: marker, scopes: ["knowledge:read"], expires_at: new Date(Date.now() + 3_600_000).toISOString() },
    });
    expect(issued.status()).toBe(200);
    const receipt = await issued.json() as { key_id: string; api_key: string; scopes: string[] };
    keyId = receipt.key_id; key = receipt.api_key;
    expect(typeof keyId).toBe("string"); expect(typeof key).toBe("string"); expect(Boolean(key)).toBe(true);
    expect(receipt.scopes).toEqual(["knowledge:read"]);

    const initialized = await rpc("initialize", { protocolVersion: LEGACY, capabilities: {}, clientInfo: { name: "AgentForge browser verification", version: "1" } }, LEGACY);
    expect(initialized.status()).toBe(200);
    expect((await initialized.json() as Rpc).result?.protocolVersion).toBe(LEGACY);
    legacySession = initialized.headers()["mcp-session-id"];
    expect(Boolean(legacySession)).toBe(true);
    expect((await rpc("notifications/initialized", {}, LEGACY, true)).status()).toBe(202);
    const legacyList = await rpc("tools/list", {}, LEGACY);
    expect(legacyList.status()).toBe(200);
    expect((await legacyList.json() as Rpc).result?.tools?.some(tool => tool.name === "space_list")).toBe(true);
    const discovery = await rpc("server/discover", {});
    expect(discovery.status()).toBe(200);
    expect((await discovery.json() as Rpc).result?.supportedVersions).toContain(MODERN);
    const modernList = await rpc("tools/list", {});
    expect(modernList.status()).toBe(200);
    const tools = (await modernList.json() as Rpc).result?.tools;
    for (const name of ["space_list", "space_search", "space_read_page"]) {
      expect(tools?.find(tool => tool.name === name)).toMatchObject({ annotations: { readOnlyHint: true }, inputSchema: { type: "object" } });
    }

    // Create the disposable fixture through normal REST authority. No approval
    // header or synthetic MCP permission/AI execution receipt is supplied.
    spaceAttempted = true;
    const createdSpace = await request.post(`${api}/api/v1/knowledge/spaces`, { headers: admin, data: { name: marker, kind: "team", visibility: "private" } });
    expect(createdSpace.status()).toBe(201);
    spaceId = (await createdSpace.json() as { space_id: string }).space_id;
    expect(typeof spaceId).toBe("string");
    const createdPage = await request.post(`${api}/api/v1/knowledge/spaces/${spaceId}/items`, { headers: admin, data: { title: pageTitle, kind: "page", content_format: "markdown" } });
    expect(createdPage.status()).toBe(201);
    const itemId = (await createdPage.json() as { item_id: string }).item_id;
    const initial = await request.get(`${api}/api/v1/knowledge/space-items/${itemId}/content`, { headers: admin });
    expect(initial.status()).toBe(200);
    const etag = initial.headers().etag;
    expect(typeof etag).toBe("string");
    const saved = await request.put(`${api}/api/v1/knowledge/space-items/${itemId}/content`, { headers: { ...admin, "If-Match": etag }, data: { content_format: "markdown", blocks: [], markdown } });
    expect(saved.status()).toBe(200);
    const listed = await call("space_list", {});
    expect((listed.spaces as Array<{ space_id: string; name: string }>).find(space => space.space_id === spaceId)).toMatchObject({ name: marker });
    const found = await call("space_search", { query: marker, space_id: spaceId });
    expect((found.items as Array<{ item_id: string }>).some(item => item.item_id === itemId)).toBe(true);
    expect(await call("space_read_page", { item_id: itemId })).toMatchObject({ item_id: itemId, markdown });
    expect(await call("space_read_page", { item_id: itemId }, LEGACY)).toMatchObject({ item_id: itemId, markdown });

    // Index a new task-owned local embedding sample instead of relying on a
    // historical dataset, paid provider, or a machine-specific fixture file.
    datasetAttempted = true;
    const createdDataset = await request.post(`${api}/api/v1/knowledge/datasets`, {
      headers: admin,
      data: { name: marker, embedding_provider: "local", embedding_model: "hash-384", embedding_dimension: 384 },
    });
    expect(createdDataset.status()).toBe(200);
    datasetId = (await createdDataset.json() as { dataset_id: string }).dataset_id;
    expect(datasetId).toMatch(/^kb_[A-Za-z0-9_-]+$/);
    const query = `The isolated ${marker} sample verifies canonical MCP knowledge identifiers.`;
    const document = await request.post(`${api}/api/v1/knowledge/${datasetId}/documents/text`, {
      headers: admin, data: { title: marker, content: query },
    });
    expect(document.status()).toBe(200);
    const uploaded = await document.json() as { document_id: string };
    expect(typeof uploaded.document_id).toBe("string");
    await expect.poll(async () => {
      const segments = await request.get(`${api}/api/v1/knowledge/${datasetId}/segments`, { headers: admin });
      expect(segments.status()).toBe(200);
      const body = await segments.json() as Array<{ document_id: string }>;
      expect(Array.isArray(body)).toBe(true);
      return body.some(segment => segment.document_id === uploaded.document_id);
    }, { timeout: 90_000, intervals: [500, 1_000, 2_000] }).toBe(true);
    for (const version of [MODERN, LEGACY]) {
      const retrieved = await call("knowledge_retrieve", { dataset_id: datasetId, query, top_k: 3 }, version);
      expect((retrieved.results as Array<{ document_id: string }>).some(result => result.document_id === uploaded.document_id)).toBe(true);
    }

    const denied = await rpc("tools/call", { name: "space_create_page", arguments: { space_id: spaceId, title: `${marker} denied` } });
    expect(denied.status()).toBe(200);
    expect((await denied.json() as Rpc).error).toMatchObject({ code: -32602, message: "approval_required" });
    const tree = await request.get(`${api}/api/v1/knowledge/spaces/${spaceId}/tree`, { headers: admin });
    expect(tree.status()).toBe(200);
    expect((await tree.json() as { items: Array<{ title: string }> }).items.some(item => item.title === `${marker} denied`)).toBe(false);
    const revoked = await request.post(`${api}/api/v1/api-keys/${keyId}/revoke`, { headers: admin });
    expect(revoked.status()).toBe(200);
    expect((await rpc("tools/list", {})).status()).toBe(401);
  } finally {
    key = "";
    const cleanup: Array<{ resource: string; status: number }> = [];
    // A lost acknowledgement never causes a second issuance/create POST. Find
    // only this test's unique marker through authorized metadata before cleanup.
    try {
      try {
        if (datasetAttempted && !datasetId) {
          const listed = await request.get(`${api}/api/v1/knowledge/datasets`, { headers: admin });
          if (listed.ok()) datasetId = (await listed.json() as Array<{ name: string; dataset_id: string }>).find(row => row.name === marker)?.dataset_id;
        }
        if (datasetId) {
          let deleted = await request.delete(`${api}/api/v1/knowledge/datasets/${datasetId}`, { headers: admin, data: { password, reason: "Isolated MCP browser verification cleanup" } });
          for (let retry = 0; deleted.status() === 409 && retry < 10; retry += 1) {
            await new Promise(resolve => setTimeout(resolve, 1_000));
            deleted = await request.delete(`${api}/api/v1/knowledge/datasets/${datasetId}`, { headers: admin, data: { password, reason: "Isolated MCP browser verification cleanup" } });
          }
          cleanup.push({ resource: "dataset", status: deleted.status() });
        }
      } finally {
        if (spaceAttempted && !spaceId) {
          const listed = await request.get(`${api}/api/v1/knowledge/spaces`, { headers: admin });
          if (listed.ok()) spaceId = (await listed.json() as { spaces: Array<{ name: string; space_id: string }> }).spaces.find(row => row.name === marker)?.space_id;
        }
        if (spaceId) cleanup.push({ resource: "space", status: (await request.delete(`${api}/api/v1/knowledge/spaces/${spaceId}`, { headers: admin })).status() });
      }
    } finally {
      try {
        if (issuedAttempted && !keyId) {
          const listed = await request.get(`${api}/api/v1/api-keys?include_inactive=true`, { headers: admin });
          if (listed.ok()) keyId = (await listed.json() as Array<{ name: string; key_id: string }>).find(row => row.name === marker)?.key_id;
        }
        if (keyId) cleanup.push({ resource: "key", status: (await request.delete(`${api}/api/v1/api-keys/${keyId}`, { headers: admin })).status() });
      } finally {
        await testInfo.attach("mcp-transactions", { body: JSON.stringify({ transactions, cleanup }), contentType: "application/json" });
      }
    }
    for (const result of cleanup) expect(result.status, `${result.resource} cleanup`).toBe(result.resource === "space" ? 204 : 200);
  }
});
