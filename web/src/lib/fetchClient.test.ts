import assert from "node:assert/strict";
import test from "node:test";
import { createHttpClient, HttpError, isHttpError, sendStreamRequest } from "./fetchClient.ts";
import type { RequestBody } from "../api/contracts.ts";
import { createFetchClient } from "../../../sdk/typescript/generated.ts";

function fixture(t: test.TestContext, reply: (url: string, init: RequestInit) => Response | Promise<Response> = () => Response.json({ ok: true })) {
  const original = globalThis.fetch, calls: { url: string; init: RequestInit }[] = [];
  globalThis.fetch = async (input, init = {}) => { const url=String(input); calls.push({ url, init }); return reply(url, init); };
  t.after(() => { globalThis.fetch=original; });
  return { client: createHttpClient(), calls };
}

test("generated fetch encodes path/query once and keeps real JSON, response headers and cookies", async t => {
  const h=fixture(t,()=>new Response('{"receipt":"native"}',{status:200,headers:{"Content-Type":"application/json","X-Next-Cursor":"opaque"}}));
  const response=await h.client.get<{ receipt:string }>("/api/v1/providers/provider%2Fa",{params:{include_disabled:true,ids:["a","b"],empty:undefined}});
  assert.equal(h.calls.length,1); assert.equal(h.calls[0].url,"/api/v1/providers/provider%2Fa?include_disabled=true&ids=a&ids=b");
  assert.equal(h.calls[0].init.credentials,"same-origin"); assert.equal(response.data.receipt,"native"); assert.equal(response.headers["x-next-cursor"],"opaque");
});
test("typed multipart preserves File name/bytes and repeated binary arrays with a browser boundary",async t=>{
  const h=fixture(t),a=new File(["first"],"one.txt"),b=new File(["second"],"two.txt");
  const body:RequestBody<"POST /api/v1/knowledge/{dataset_id}/documents/batch-upload">={files:[a,b]};
  const progress:unknown[]=[];
  await h.client.post("/api/v1/knowledge/dataset/documents/batch-upload",body,{headers:{"Content-Type":"multipart/form-data"},onUploadProgress:value=>progress.push(value)});
  const request=h.calls[0].init,form=request.body as FormData;
  assert.ok(form instanceof FormData);assert.equal(new Headers(request.headers).has("Content-Type"),false);
  assert.deepEqual(form.getAll("files").map(v=>(v as File).name),["one.txt","two.txt"]);
  assert.deepEqual(await Promise.all(form.getAll("files").map(v=>(v as File).text())),["first","second"]);
  assert.deepEqual(progress,[{loaded:11,total:11,progress:1}],"fetch reports confirmed completion, never simulated intermediate upload progress");
});
test("native fetch sends a reviewed JSON mutation exactly once and omits cookies for explicit SDK credentials",async t=>{
  const h=fixture(t),body={display_name:"Reviewed"};
  await h.client.put("/api/v1/providers/provider",body,{headers:{Authorization:"Bearer synthetic-sdk"},withCredentials:false});
  assert.equal(h.calls.length,1);assert.equal(h.calls[0].init.credentials,"omit");assert.equal(h.calls[0].init.body,JSON.stringify(body));
  assert.equal(new Headers(h.calls[0].init.headers).get("Authorization"),"Bearer synthetic-sdk");
});
for(const status of [401,409,429,500])test(`HTTP ${status} retains the original one-attempt error and receipt without serializing credentials`,async t=>{
  const h=fixture(t,()=>Response.json({detail:"native rejection"},{status,headers:{"Retry-After":"3"}}));
  let error:unknown;try{await h.client.post("/api/v1/providers",{display_name:"name"},{headers:{Authorization:"Bearer synthetic-secret"}});}catch(e){error=e;}
  assert.ok(isHttpError<Record<string,unknown>>(error));assert.ok(error instanceof HttpError);assert.equal(error.response?.status,status);assert.equal(error.response?.data.detail,"native rejection");assert.equal(error.response?.headers["retry-after"],"3");
  assert.equal(h.calls.length,1);assert.doesNotMatch(JSON.stringify(error),/synthetic-secret|Authorization|native rejection/);
});
test("network rejection preserves Error identity and never replays a mutation",async t=>{
  const original=new TypeError("controlled disconnect"),h=fixture(t,()=>{throw original;});
  await assert.rejects(h.client.post("/api/v1/providers",{display_name:"review"}),error=>error===original);assert.equal(h.calls.length,1);assert.equal((original as TypeError&{code:string}).code,"ERR_NETWORK");
});
test("already-aborted and unpublished operations cannot reach fetch",async t=>{
  const h=fixture(t),controller=new AbortController();controller.abort(new Error("cancelled"));
  await assert.rejects(h.client.get("/api/v1/providers",{signal:controller.signal}),e=>e===controller.signal.reason);
  await assert.rejects(h.client.post("/api/v1/unpublished/write",{}),/generated contract/);assert.equal(h.calls.length,0);
});
test("authority and review fences run after async preflight but before the single native write",async t=>{
  const h=fixture(t);let release!:()=>void,current=true,cleaned=0;
  const proof=new Promise<void>(resolve=>{release=resolve;});h.client.interceptors.request.use(config=>{config.preflight=async()=>{await proof;return()=>{cleaned++;};};return config;});
  const pending=h.client.put("/api/v1/providers/provider",{display_name:"review"},{beforeSend:()=>{if(!current)throw new Error("review retired");}});
  assert.equal(h.calls.length,0);current=false;release();await assert.rejects(pending,/review retired/);assert.equal(h.calls.length,0);assert.equal(cleaned,1);
});
test("serialization captures the draft before passkey await",async t=>{
  const h=fixture(t),draft={display_name:"reviewed"};let release!:()=>void;
  const proof=new Promise<void>(resolve=>{release=resolve;});h.client.interceptors.request.use(config=>{config.preflight=async()=>{await proof;return()=>{};};return config;});
  const pending=h.client.put("/api/v1/providers/provider",draft);draft.display_name="later edit";release();await pending;
  assert.equal(h.calls[0].init.body,JSON.stringify({display_name:"reviewed"}));
});
test("SSE uses generated encoding while retaining the raw response, resume cursor and one POST",async t=>{
  const raw=new Response("id: 4\nevent: reset\ndata: {}\n\n",{headers:{"Content-Type":"text/event-stream"}}),h=fixture(t,()=>raw);
  const response=await sendStreamRequest("/api/v2/agent/threads/thread/events?after_sequence=3&limit=1000",{method:"GET",headers:{"Last-Event-ID":"3"}});
  assert.equal(response,raw);assert.equal(new Headers(h.calls[0].init.headers).get("Last-Event-ID"),"3");assert.match(h.calls[0].url,/after_sequence=3/);
  await sendStreamRequest("/api/v1/agents/agent/preview/chat/stream",{method:"POST",body:JSON.stringify({message:"one"}),headers:{"Content-Type":"application/json"}});assert.equal(h.calls.length,2);assert.equal(h.calls[1].init.body,'{"message":"one"}');
});

test("generated public client returns a void accepted receipt while retaining binary Blob responses",async()=>{
  const client=createFetchClient(async()=>new Response(null,{status:202}));
  const receipt=await client.request("POST /api/v1/telemetry/browser",{body:{session_id:"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",records:[{kind:"metric",name:"LCP",route:"/",value:1}]}});
  assert.equal(receipt,undefined);
  const binary=createFetchClient(async()=>new Response(new Blob([]),{headers:{"Content-Type":"application/octet-stream"}}));
  const file=await binary.request("GET /api/v1/assistant/artifacts/{artifact_id}/download",{path:{artifact_id:"file"}});
  assert.ok(file instanceof Blob);assert.equal(file.size,0);
});


test("V2 observation preserves a header-only gap cursor, bounded replay limit and explicit zero precedence", async t => {
  const h=fixture(t,()=>new Response("",{headers:{"Content-Type":"text/event-stream"}}));
  await sendStreamRequest("/api/v2/agent/threads/thread/events?limit=7&turn_id=turn",{method:"GET",headers:{"Last-Event-ID":"432",Accept:"text/event-stream"}});
  const headerOnly=new URL(h.calls[0].url,"https://console.invalid");
  assert.equal(headerOnly.searchParams.has("after_sequence"),false);
  assert.equal(headerOnly.searchParams.get("limit"),"7");
  assert.equal(headerOnly.searchParams.get("turn_id"),"turn");
  assert.equal(new Headers(h.calls[0].init.headers).get("Last-Event-ID"),"432");
  await sendStreamRequest("/api/v2/agent/threads/thread/events?after_sequence=0&limit=1000",{method:"GET",headers:{"Last-Event-ID":"stale-ignored-header"}});
  assert.equal(new URL(h.calls[1].url,"https://console.invalid").searchParams.get("after_sequence"),"0");
  assert.equal(new Headers(h.calls[1].init.headers).get("Last-Event-ID"),"stale-ignored-header");
  assert.deepEqual(h.calls.map(call=>call.init.method),["GET","GET"]);
  assert.equal(h.calls.some(call=>call.init.body!==undefined),false);
});
