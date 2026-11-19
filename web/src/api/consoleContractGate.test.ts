import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { checkConsoleContract } from "../../scripts/check-api-contract.mjs";
const models = `export interface ApiOperations {
 "GET /api/v1/items/{id}": { body: never };
 "POST /api/v1/items/{id}/pause": { body: { reason?: string } };
 "POST /api/v1/items/{id}/resume": { body: { reason?: string } };
 "POST /api/v1/no-schema": { body: unknown };
 "DELETE /api/v1/items/{id}": { body: { confirmation: string } };
}`;
function scan(code: string) { return checkConsoleContract(models, [["fixture.ts", code]]); }
test("contract gate expands arrow helpers, path IDs, conditionals and literal action unions", () => {
 const result = scan(`const itemPath = (id: string) => \`/api/v1/items/\${id}\`;
 function act(id: string, action: "pause" | "resume") { api.post(\`\${itemPath(id)}/\${action}\`, {}); }
 api.get(condition ? itemPath(id) : \`/api/v1/items/\${other}\`);`);
 assert.equal(result.checked, 3); assert.deepEqual(result.issues, []); assert.deepEqual(result.schemaGaps, []);
});
test("one documented alternative cannot hide an unsupported action or conditional branch", () => {
 const result = scan(`function act(action: "pause" | "unsupported") { api.post(\`/api/v1/items/\${id}/\${action}\`, {}); }
 api.get(flag ? \`/api/v1/items/\${id}\` : "/api/v1/undocumented");`);
 assert.equal(result.issues.length, 2); assert.match(result.issues[0].operation, /unsupported/);
});
test("request bodies on streams, DELETE configs and api.request remain checked", () => {
 const result = scan(`sseFetch("/api/v1/no-schema", { method: "POST", body: JSON.stringify({}) });
 api.delete(\`/api/v1/items/\${id}\`, { data: { confirmation: "proof" } });
 api.request({ method: "post", url: "/api/v1/no-schema", data: {} });`);
 assert.equal(result.checked, 3); assert.equal(result.schemaGaps.length, 2); assert.deepEqual(result.issues, []);
});
test("unknown dynamic HTTP URLs fail closed and generated request keys are checked", () => {
 const result = scan(`api.get(serverUrl); request("POST /api/v1/no-schema", { body: {} }); request("GET /api/v1/not-real", {});`);
 assert.equal(result.issues.length, 2); assert.equal(result.schemaGaps.length, 1);
});
test("unresolved actions cannot masquerade as a documented literal action", () => {
 const result = scan(`api.post(\`/api/v1/items/\${id}/\${serverAction}\`, {});`);
 assert.equal(result.issues.length, 1);
});

test("opaque aliases and any/unknown unions cannot conceal missing wire schemas", () => {
 const generated = `export namespace Models { export type Opaque = unknown; } export type Hidden = Models.Opaque;
 export interface ApiOperations { "POST /api/v1/opaque": { body: Hidden }; "POST /api/v1/any": { body: any }; "POST /api/v1/union": { body: { name: string } | unknown }; }`;
 const result = checkConsoleContract(generated, [["fixture.ts", `api.post("/api/v1/opaque", {}); api.post("/api/v1/any", {}); api.post("/api/v1/union", {});`]]);
 assert.equal(result.schemaGaps.length, 3);
});

test("opaque catch-all dispatchers cannot document an otherwise unknown Console operation", () => {
 const result = checkConsoleContract(`export interface ApiOperations { "GET /api/v1/knowledge/{path}": {body: never} }`, [["fixture.ts", `api.get("/api/v1/knowledge/invented");`]]);
 assert.equal(result.issues.length, 1);
});

test("the production build used by CI runs the operation gate before bundling", () => {
 const pkg = JSON.parse(readFileSync(new URL("../../package.json", import.meta.url), "utf8"));
 const commands = pkg.scripts.build.split(" && ");
 assert.equal(commands[0], "node scripts/check-api-contract.mjs");
 assert.ok(commands.indexOf("vite build") > 0);
 assert.ok(commands.includes("node scripts/check-bundle-budget.mjs"));
});

test("aliased clients and injected absolute-origin fetches retain the full operation/body gate",()=>{
  const result=scan(`apiClient.get("/api/v1/items/one");
  options.fetch(\`\${options.origin}/api/v1/no-schema\`, {method:"POST",body:JSON.stringify({})});
  options.fetch(\`\${options.origin}/api/v1/not-published\`);`);
  assert.equal(result.checked,3);assert.equal(result.schemaGaps.length,1);assert.equal(result.issues.length,1);
});
