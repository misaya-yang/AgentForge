import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Tree = { type: unknown; props: Record<string, unknown>; key?: string };
const taskId = "00000000-0000-0000-0000-000000000001";
const otherTask = "00000000-0000-0000-0000-000000000002";
const runId = "00000000-0000-0000-0000-000000000003";
const at = "2026-10-13T00:00:00Z";
const task = (id = taskId) => ({ task_id: id, title: `Schedule ${id}`, message: "Instructions", model_id: "fixture-model", status: "active", next_run_at: at, interval_seconds: 60, max_retries: 0, retry_count: 0 });
const execution = (id = runId) => ({ execution_id: id, task_id: taskId, session_id: "session", run_id: "run", status: "succeeded", attempt: 0, error_code: null, cancel_requested: false, created_at: at, finished_at: at });
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (value: Error) => void; const promise = new Promise<T>((yes,no) => { resolve=yes; reject=no; }); return {promise,resolve,reject}; }
function all(tree: unknown, predicate: (node: Tree) => boolean): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(value => all(value,predicate));
  const node=tree as Tree; return [...(predicate(node)?[node]:[]),...all(node.props?.children,predicate)];
}
async function settle() { for (let i=0;i<40;i++) await Promise.resolve(); }
function host(initial = "", initiallyVisible = true) {
  let params=new URLSearchParams(initial); let dirty=true; let cursor=0; let tree: Tree;
  const slots: unknown[]=[]; const effects: (()=>void)[]=[]; const cleanups: (()=>void)[]=[];
  const timers=new Map<number,()=>void>(); let timer=0;
  const requests: {path:string; params?:Record<string,unknown>; signal?:AbortSignal}[]=[];
  const writes: unknown[]=[];
  const listeners = new Set<() => void>();
  const document = { visibilityState: initiallyVisible ? "visible" : "hidden",
    addEventListener: (_name: string, callback: () => void) => listeners.add(callback),
    removeEventListener: (_name: string, callback: () => void) => listeners.delete(callback) };
  let read: (path:string, params?:Record<string,unknown>)=>Promise<unknown> = async path => {
    if (path.endsWith(`/executions/${runId}`)) return {execution:execution()};
    if (path.endsWith("/executions")) return {executions:[execution()],next_cursor:null};
    if (path.endsWith(taskId)) return {task:task()};
    return {tasks:[task()],next_cursor:null};
  };
  const same=(a?:unknown[],b?:unknown[])=>a?.length===b?.length && a?.every((v,i)=>Object.is(v,b?.[i]));
  const react={
    useState(initial:unknown) { const i=cursor++; if (!(i in slots)) slots[i]=initial;
      return [slots[i],(value:unknown)=>{ slots[i]=typeof value==="function"?value(slots[i]):value; dirty=true; }]; },
    useEffect(run:()=>void|(()=>void),deps:unknown[]) { const i=cursor++; if (!same(slots[i] as unknown[],deps)) {slots[i]=deps; effects.push(()=>{cleanups[i]?.();cleanups[i]=run()||(()=>{});});} },
  };
  const jsx=(type:unknown,props:Record<string,unknown>,key?:string)=>({type,props,key});
  const named=new Proxy({}, {get:(_t,key)=>String(key)});
  const modules:Record<string,unknown>={
    react,"react/jsx-runtime":{jsx,jsxs:jsx},
    "react-router-dom":{useSearchParams:()=>[params,(next:(current:URLSearchParams)=>URLSearchParams)=>{params=next(params);dirty=true;}]},
    "react-i18next":{useTranslation:()=>({t:(key:string)=>key})},
    "./ScheduledExecutionResult":{ScheduledExecutionResult:"Result"},
    "@/lib/api":{api:{async get(path:string,options:{params?:Record<string,unknown>;signal?:AbortSignal}={}) { requests.push({path,...options});return {data:await read(path,options.params)};},post(){throw Error("Unexpected write");}}},
  };
  function load(file:string) {const exports:Record<string,unknown>={};vm.runInNewContext(ts.transpileModule(readFileSync(new URL(file,import.meta.url),"utf8"),{compilerOptions:{jsx:ts.JsxEmit.ReactJSX,target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.CommonJS}}).outputText,{exports,require:(name:string)=>{if(name.startsWith("@/components/ui/"))return named;assert.ok(name in modules,`Unexpected ${name}`);return modules[name];},AbortController,URLSearchParams,Date,Error,document,window:{setInterval:(f:()=>void)=>{timers.set(++timer,f);return timer;},clearInterval:(id:number)=>timers.delete(id)}});return exports;}
  modules["@/api/scheduledTasks"]=load("../../api/scheduledTasks.ts");
  const page=load("./components/ScheduledHistoryBrowser.tsx").ScheduledHistoryBrowser as (props:unknown)=>Tree;
  const props={revision:0,busy:false,act:(...args:unknown[])=>writes.push(args)};
  async function flush() {for(let i=0;i<10;i++){if(dirty){dirty=false;cursor=0;tree=page(props);while(effects.length)effects.shift()!();}await settle();if(!dirty)break;}return tree!;}
  return {requests,writes,api:modules["@/api/scheduledTasks"] as Record<string, (...args:unknown[])=>Promise<unknown>>,
    async visibility(visible: boolean) { document.visibilityState = visible ? "visible" : "hidden"; for (const listener of listeners) listener(); await flush(); },
    visibilityBeforeRender(visible: boolean) { document.visibilityState = visible ? "visible" : "hidden"; for (const listener of listeners) listener(); },
    timers: () => [...timers.values()], listenerCount: () => listeners.size,
    setRead:(value:typeof read)=>{read=value;},flush,tree:()=>tree!,params:()=>params,
    async navigate(value:string){params=new URLSearchParams(value);dirty=true;await flush();},
    async tick(){for(const fn of [...timers.values()])fn();await flush();},
    async click(key:string,index=0){const button=all(tree!,n=>n.type==="Button"&&all(n.props.children,x=>x.type===key).length>0)[index]??all(tree!,n=>n.type==="Button"&&n.props.children===key)[index];assert.ok(button,`Missing ${key}`);assert.equal(button.props.disabled===true,false);(button.props.onClick as ()=>void)();await flush();},
    unmount(){for(const clean of cleanups)clean?.();},
  };
}
function results(h:ReturnType<typeof host>) { return all(h.tree(),n=>n.type==="Result"); }
function has(h:ReturnType<typeof host>,value:string) { return JSON.stringify(h.tree()).includes(value); }

test("older schedules page with exact cursor and browser Back preserves URL traversal",async()=>{
 const h=host();h.setRead(async(_path,p)=>p?.before_id?{tasks:[task(otherTask)],next_cursor:null}:{tasks:[task()],next_cursor:{before_at:at,before_id:taskId}});
 await h.flush();await h.click("tasks.scheduled.older");assert.equal(h.params().get("schedule_before_id"),taskId);
 assert.equal(h.requests.at(-1)?.params?.before_at,at);assert.ok(has(h,otherTask));assert.equal(h.writes.length,0);
 await h.navigate("");assert.ok(has(h,taskId));h.unmount();
});
test("deep link reads exact old schedule and execution outside newest list",async()=>{
 const h=host(`task=${taskId}&execution=${runId}`);h.setRead(async path=>path.endsWith(`/executions/${runId}`)?{execution:execution()}:path.endsWith("/executions")?{executions:[],next_cursor:null}:path.endsWith(taskId)?{task:task()}:{tasks:[task(otherTask)],next_cursor:null});
 await h.flush();assert.equal(results(h).length,1);assert.equal(results(h)[0].props.initiallyOpen,true);assert.ok(has(h,"tasks.scheduled.linkedOutsidePage"));assert.equal(h.writes.length,0);h.unmount();
});
test("exact execution already on current page is shown only once",async()=>{const h=host(`task=${taskId}&execution=${runId}`);await h.flush();assert.equal(results(h).length,1);h.unmount();});
test("execution pagination retains selected task and schedule cursor",async()=>{
 const h=host(`task=${taskId}&schedule_before_at=${at}&schedule_before_id=${otherTask}`);h.setRead(async path=>path.endsWith("/executions")?{executions:[execution()],next_cursor:{before_at:at,before_id:runId}}:path.endsWith(taskId)?{task:task()}:{tasks:[],next_cursor:null});
 await h.flush();await h.click("tasks.scheduled.older");assert.equal(h.params().get("execution_before_id"),runId);assert.equal(h.params().get("schedule_before_id"),otherTask);assert.equal(h.params().get("task"),taskId);h.unmount();
});
test("malformed cursor never sends ambiguous pagination request and Latest recovers",async()=>{
 const h=host("schedule_before_id=bad");await h.flush();assert.equal(h.requests.length,0);assert.ok(has(h,"tasks.scheduled.invalidHistoryLink"));await h.click("tasks.scheduled.latest");assert.equal(h.requests.length,1);h.unmount();
});
test("failed exact read hides task instructions and actions while keeping independent list",async()=>{
 const h=host(`task=${taskId}`);h.setRead(async path=>{if(path.endsWith(taskId))throw Error("denied");return path.endsWith("/executions")?{executions:[execution()],next_cursor:null}:{tasks:[task(otherTask)],next_cursor:null};});
 await h.flush();assert.ok(has(h,otherTask));assert.ok(has(h,"tasks.scheduled.exactUnavailable"));assert.equal(has(h,"Instructions"),false);assert.equal(results(h).length,0);assert.equal(has(h,"tasks.scheduled.pause"),false);h.unmount();
});
test("list failure remains distinct from successful empty page and refresh only reads",async()=>{
 const h=host();h.setRead(async()=>{throw Error("offline");});await h.flush();assert.ok(has(h,"tasks.scheduled.historyReadError"));assert.equal(has(h,"tasks.scheduled.emptyPage"),false);
 h.setRead(async()=>({tasks:[],next_cursor:null}));await h.click("tasks.scheduled.refresh");assert.ok(has(h,"tasks.scheduled.emptyPage"));assert.equal(h.writes.length,0);h.unmount();
});
test("old navigation response cannot populate a returned same-ID visit",async()=>{
 const pending=deferred<unknown>();const h=host(`task=${taskId}`);let count=0;h.setRead(async path=>path.endsWith(taskId)?(++count===1?pending.promise:{task:{...task(),title:"Current visit"}}):path.endsWith(otherTask)?{task:task(otherTask)}:path.endsWith("/executions")?{executions:[],next_cursor:null}:{tasks:[],next_cursor:null});
 await h.flush();await h.navigate(`task=${otherTask}`);await h.navigate(`task=${taskId}`);pending.resolve({task:{...task(),title:"Abandoned visit"}});await h.flush();assert.ok(has(h,"Current visit"));assert.equal(has(h,"Abandoned visit"),false);h.unmount();
});
test("polling skips outstanding reads longer than one interval",async()=>{
 const pending=deferred<unknown>();const h=host();h.setRead(async()=>pending.promise);await h.flush();await h.tick();await h.tick();assert.equal(h.requests.length,1);assert.equal(h.requests[0].signal?.aborted,false);
 pending.resolve({tasks:[task()],next_cursor:null});await h.flush();assert.ok(has(h,taskId));h.unmount();
});
test("poll keeps result mounted while rereading and removes it on failed authority read",async()=>{
 const h=host(`task=${taskId}`);await h.flush();const initial=results(h)[0];assert.ok(initial);const pending=deferred<unknown>();h.setRead(async path=>path.endsWith(taskId)?pending.promise:path.endsWith("/executions")?{executions:[execution()],next_cursor:null}:{tasks:[task()],next_cursor:null});
 await h.tick();assert.equal(results(h)[0]?.key,initial.key);assert.equal(results(h)[0]?.type,initial.type);
 pending.reject(Error("revoked"));await h.flush();assert.equal(results(h).length,0);h.unmount();
});
test("unmount aborts in-flight reads and late success cannot expose receipts",async()=>{
 const pending=deferred<unknown>();const h=host();h.setRead(async()=>pending.promise);await h.flush();h.unmount();assert.equal(h.requests[0].signal?.aborted,true);pending.resolve({tasks:[task()],next_cursor:null});await settle();assert.equal(has(h,taskId),false);
});
test("API adapter rejects wrong schedule and cross-task execution identities",async()=>{
 const h=host();h.setRead(async()=>({task:task(otherTask),execution:{...execution(),task_id:otherTask}}));await assert.rejects(h.api.readSchedule(taskId));await assert.rejects(h.api.readScheduledExecution(taskId,runId));
});
test("legacy list omitting cursor keeps rows and discloses bounded server behavior",async()=>{
 const h=host();h.setRead(async()=>({tasks:[task()]}));await h.flush();assert.ok(has(h,"tasks.scheduled.legacyHistory"));assert.equal(has(h,"tasks.scheduled.older"),false);h.unmount();
});
test("malformed server cursor fails rather than inventing a page boundary",async()=>{
 const h=host();h.setRead(async()=>({tasks:[task()],next_cursor:{before_at:at,before_id:"invalid"}}));await h.flush();assert.ok(has(h,"tasks.scheduled.historyReadError"));assert.equal(has(h,taskId),false);h.unmount();
});

test("hidden deep-link mount sends zero reads and one foreground refresh per enabled resource", async () => {
  const h = host(`task=${taskId}&execution=${runId}`, false);
  await h.flush(); assert.equal(h.requests.length, 0); assert.ok(has(h, "tasks.scheduleVisibility.paused"));
  for (let minute = 0; minute < 4; minute++) for (let tick = 0; tick < 4; tick++) await h.tick();
  assert.equal(h.requests.length, 0);
  await h.visibility(true); assert.equal(h.requests.length, 4); assert.equal(new Set(h.requests.map(request => request.path)).size, 4);
  await h.visibility(true); assert.equal(h.requests.length, 4); assert.ok(has(h, "tasks.scheduleVisibility.observed"));
  assert.equal(h.writes.length, 0); h.unmount(); assert.equal(h.listenerCount(), 0);
});

test("hide retains same-visit receipts without polling; queued old timer cannot duplicate resumed GET", async () => {
  const h = host(`task=${taskId}&execution=${runId}`); await h.flush(); const oldTimers = h.timers();
  assert.equal(h.requests.length, 4); await h.visibility(false); assert.equal(h.timers().length, 0);
  assert.ok(has(h, "Instructions")); assert.equal(has(h, "tasks.scheduled.loadingSchedules"), false);
  for (let tick = 0; tick < 16; tick++) await h.tick(); assert.equal(h.requests.length, 4);
  await h.visibility(true); assert.equal(h.requests.length, 8);
  for (const timer of oldTimers) timer(); await h.flush(); assert.equal(h.requests.length, 8);
  await h.tick(); assert.equal(h.requests.length, 12); assert.equal(h.writes.length, 0); h.unmount();
});

test("visibility change closes the pre-render timer race and aborts the old read lifetime", async () => {
  const first = deferred<unknown>(); const h = host(); h.setRead(async () => first.promise); await h.flush();
  const oldTimer = h.timers()[0]; h.visibilityBeforeRender(false); oldTimer(); await h.flush();
  assert.equal(h.requests.length, 1); assert.equal(h.requests[0].signal?.aborted, true);
  h.setRead(async () => ({ tasks: [{ ...task(), title: "Fresh foreground" }], next_cursor: null }));
  await h.visibility(true); first.resolve({ tasks: [{ ...task(), title: "Hidden old reply" }], next_cursor: null }); await h.flush();
  assert.ok(has(h, "Fresh foreground")); assert.equal(has(h, "Hidden old reply"), false); assert.equal(h.requests.length, 2); h.unmount();
});

test("foreground refresh remains single-flight and failed authority read is not presented as fresh", async () => {
  const h = host(`task=${taskId}`); await h.flush(); await h.visibility(false);
  const pending = deferred<unknown>(); h.setRead(async path => path.endsWith(taskId) ? pending.promise : path.endsWith("/executions") ? { executions: [], next_cursor: null } : { tasks: [], next_cursor: null });
  await h.visibility(true); const count = h.requests.length; assert.ok(has(h, "tasks.scheduleVisibility.refreshing"));
  await h.visibility(true); await h.tick();
  assert.equal(h.requests.filter(request => request.path.endsWith(taskId)).length, 2);
  assert.equal(h.requests.length, count + 2); // Only independently completed list/history reads poll.
  pending.reject(Error("access changed")); await h.flush();
  assert.ok(has(h, "tasks.scheduleVisibility.unavailable")); assert.equal(has(h, "Instructions"), false); assert.equal(results(h).length, 0); h.unmount();
});

test("hidden navigation defers reads and never resurfaces the previous selection", async () => {
  const h = host(`task=${taskId}`); await h.flush(); await h.visibility(false); const count = h.requests.length;
  await h.navigate(`task=${otherTask}`); assert.equal(h.requests.length, count); assert.equal(has(h, "Instructions"), false);
  h.setRead(async path => path.endsWith(otherTask) ? { task: task(otherTask) } : path.endsWith("/executions") ? { executions: [], next_cursor: null } : { tasks: [], next_cursor: null });
  await h.visibility(true); assert.equal(h.requests.length, count + 3); assert.ok(has(h, otherTask)); assert.equal(h.writes.length, 0); h.unmount();
});
