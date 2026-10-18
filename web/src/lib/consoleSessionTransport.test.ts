import assert from "node:assert/strict";
import test from "node:test";
import { createConsoleFetch } from "./consoleSessionTransport.ts";

function fixture() {
  let session={token:"console-session:1" as string|null,csrfToken:"synthetic-csrf" as string|null,isAuthenticated:true,generation:1};
  let cleared=0;
  let respond:(response:Response)=>void=()=>{};
  const calls:Array<{input:RequestInfo|URL;init?:RequestInit}>=[];
  const transport=createConsoleFetch(((input,init)=>{calls.push({input,init});return new Promise<Response>(done=>{respond=done;});}) as typeof fetch,
    "https://console.example",()=>session,()=>{cleared++;});
  return {transport,calls,get session(){return session;},setSession(value:Partial<typeof session>){session={...session,...value};},respond(status=200){respond(new Response("",{status}));},cleared:()=>cleared};
}
test("raw fetch replaces UI marker with cookie+CSRF while preserving body, trace and cancellation",async()=>{
  const h=fixture(),controller=new AbortController();
  const pending=h.transport("/api/v1/providers",{method:"POST",headers:{Authorization:"Bearer console-session:1",traceparent:"synthetic-trace"},body:'{"display_name":"reviewed"}',signal:controller.signal});
  const init=h.calls[0].init!;const headers=new Headers(init.headers);
  assert.equal(init.credentials,"same-origin");assert.equal(init.body,'{"display_name":"reviewed"}');assert.equal(init.signal,controller.signal);
  assert.equal(headers.get("Authorization"),null);assert.equal(headers.get("X-CSRF-Token"),"synthetic-csrf");assert.equal(headers.get("traceparent"),"synthetic-trace");
  h.respond();await pending;assert.equal(h.calls.length,1);
});
test("missing CSRF blocks writes before transport and never retries",async()=>{
  const h=fixture();h.setSession({csrfToken:null});
  await assert.rejects(h.transport("/api/v1/providers",{method:"DELETE"}),/CSRF bootstrap/);assert.equal(h.calls.length,0);
  const read=h.transport("/api/v1/providers");h.respond(403);assert.equal((await read).status,403);assert.equal(h.calls.length,1);
});
test("external SDK bearer remains explicit and never receives Console CSRF or cookie credentials",async()=>{
  const h=fixture();const pending=h.transport("https://sdk.example/v1/responses",{method:"POST",headers:{Authorization:"Bearer sdk-api-key","X-CSRF-Token":"must-not-leak"}});
  const headers=new Headers(h.calls[0].init!.headers);
  assert.equal(headers.get("Authorization"),"Bearer sdk-api-key");assert.equal(headers.get("X-CSRF-Token"),null);assert.equal(h.calls[0].init!.credentials,"omit");
  h.respond();await pending;
});
test("owned current 401 expires UI; delayed A→B→A 401 cannot expire a replacement",async()=>{
  const h=fixture();const old=h.transport("/api/v1/providers");
  h.setSession({token:"console-session:2",generation:2});h.setSession({token:"console-session:1",generation:3});
  h.respond(401);await old;assert.equal(h.cleared(),0);
  const current=h.transport("/api/v1/providers");h.respond(401);await current;assert.equal(h.cleared(),1);assert.equal(h.calls.length,2);
});
test("telemetry 401 cannot log the product out",async()=>{
  const h=fixture();const pending=h.transport("/api/v1/telemetry/browser",{method:"POST"});h.respond(401);await pending;assert.equal(h.cleared(),0);
});

test("same-origin SDK bearer is never mixed with a Console cookie or CSRF, nor expires it on 401",async()=>{
  const h=fixture();const pending=h.transport("/v1/responses",{method:"POST",headers:{Authorization:"Bearer sdk-api-key"}});
  assert.equal(h.calls[0].init!.credentials,"omit");assert.equal(new Headers(h.calls[0].init!.headers).get("X-CSRF-Token"),null);
  h.respond(401);await pending;assert.equal(h.cleared(),0);
});

test("a stale raw-fetch UI marker cannot be promoted into the replacement cookie session",async()=>{
  const h=fixture();h.setSession({token:"console-session:2",csrfToken:"csrf-b",generation:2});
  await assert.rejects(h.transport("/api/v1/providers",{method:"POST",headers:{Authorization:"Bearer console-session:1"},body:"old actor operation"}),/session changed/);
  assert.equal(h.calls.length,0);
});

test("raw Request body materialization cannot dispatch under a replacement account", async () => {
  const h=fixture();
  const request=new Request("https://console.example/api/v1/providers/provider",{method:"PUT",headers:{"Content-Type":"application/json"},body:'{"display_name":"old draft"}'});
  const pending=h.transport(request);
  h.setSession({token:"console-session:2",generation:2});
  await assert.rejects(pending,/session changed before request/);
  assert.equal(h.calls.length,0,"the asynchronous Request clone must not turn an old draft into a new-account write");
});
test("raw same-origin consumers reject undocumented API operations before native fetch",async()=>{
  const h=fixture();
  await assert.rejects(h.transport("/api/v1/not-published"),/generated contract/);
  assert.equal(h.calls.length,0);
});
test("generated raw-fetch encoding retains repeated query filters without double-encoding identifiers",async()=>{
  const h=fixture(),pending=h.transport("/api/v1/providers/provider%2Fa?ids=one&ids=two");
  const url=new URL(String(h.calls[0].input),"https://console.example");
  assert.equal(url.pathname,"/api/v1/providers/provider%2Fa");assert.deepEqual(url.searchParams.getAll("ids"),["one","two"]);
  h.respond();await pending;assert.equal(h.calls.length,1);
});

test("raw Fetch RequestInit null body remains an absent body on read requests",async()=>{
  const h=fixture(),pending=h.transport("/api/v1/providers",{method:"GET",body:null});
  assert.equal(h.calls[0].init?.body,undefined);h.respond();await pending;assert.equal(h.calls.length,1);
});
