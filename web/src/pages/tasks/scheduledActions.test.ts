import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { sdkAuthorityScope } from "../../lib/sdkAuthorityScope.ts";
const id = "00000000-0000-0000-0000-000000000001";
const other = "00000000-0000-0000-0000-000000000002";
const task = (status = "active") => ({ task_id: id, status, title: "Fixture schedule", message: "Fixture", model_id: "fixture", next_run_at: "2026-10-15T00:00:00Z", interval_seconds: 60, max_retries: 0, retry_count: 0 });
function deferred() { let resolve!: (value: unknown) => void; let reject!: (error: Error) => void; const promise = new Promise<unknown>((a,b) => { resolve=a; reject=b; }); return {promise,resolve,reject}; }
type Node = { type: unknown; props: Record<string, unknown> };
function nodes(value: unknown): Node[] { if (!value || typeof value !== "object") return []; if (Array.isArray(value)) return value.flatMap(nodes); const n=value as Node; return n.props ? [n,...nodes(n.props.children)] : []; }
function shown(value: unknown): string { if (typeof value === "string" || typeof value === "number") return String(value); if(Array.isArray(value))return value.map(shown).join(" "); return value && typeof value === "object" ? shown((value as Node).props?.children) : ""; }
async function settle() { for(let n=0;n<30;n++)await Promise.resolve(); }
function host(composed = false) {
  type Lane = {slots: unknown[]; cleanups: Array<()=>void>};
  const parent: Lane = {slots:[],cleanups:[]}; const children = new Map<string,Lane>();
  let slots=parent.slots; let cleanups=parent.cleanups; let effects: Array<()=>void>=[]; let cursor=0; let epoch=0;
  const same=(a:unknown[]|undefined,b:unknown[])=>a?.length===b.length&&a.every((v,i)=>Object.is(v,b[i]));
  const react={
    useState(initial:unknown){const lane=slots;const i=cursor++;if(!(i in lane))lane[i]=initial;return[lane[i],(v:unknown)=>{lane[i]=typeof v==="function"?v(lane[i]):v;}];},
    useRef(initial:unknown){const i=cursor++;return slots[i]??(slots[i]={current:initial});},
    useMemo(make:()=>unknown,deps:unknown[]){const i=cursor++;const old=slots[i] as {value:unknown;deps:unknown[]}|undefined;if(!old||!same(old.deps,deps))slots[i]={value:make(),deps};return(slots[i] as {value:unknown}).value;},
    useLayoutEffect(effect:()=>void|(()=>void),deps:unknown[]){const cleanup=cleanups;const i=cursor++;if(!same(slots[i] as unknown[]|undefined,deps)){slots[i]=deps;effects.push(()=>{cleanup[i]?.();cleanup[i]=effect()||(()=>{});});}},
  };
  Object.assign(react,{useEffect:react.useLayoutEffect,useCallback:(fn:unknown,deps:unknown[])=>react.useMemo(()=>fn,deps)});
  let auth={token:"a",isAuthenticated:true,user:{user_id:"user-a",tenant_id:"tenant-a",roles:["user"],permissions:["assistant:chat"]}};
  const subscribers=new Set<(next:typeof auth,old:typeof auth)=>void>();
  const reads:string[]=[];const posts:Array<{path:string;body:unknown}>=[];
  let read: (path:string)=>Promise<unknown>=async()=>({task:task()});let post=async()=>({task_id:id,status:"paused",cancellation_requested:false}) as unknown;
  let available=true;let source:unknown=task();let observed=0;
  let params=new URLSearchParams(`task=${id}`); const timers=new Map<number,()=>void>();let timer=0;
  const modules:Record<string,unknown>={react,"react/jsx-runtime":{jsx:(type:unknown,props:Record<string,unknown>)=>({type,props}),jsxs:(type:unknown,props:Record<string,unknown>)=>({type,props})},
    "react-i18next":{useTranslation:()=>({t:(key:string)=>key})},"@/components/ui/button":{Button:"Button"},"@/components/ui/card":{Card:"Card",CardContent:"CardContent"},"./ScheduledExecutionResult":{ScheduledExecutionResult:"ExecutionResult"},
    "react-router-dom":{useSearchParams:()=>[params,(update:(old:URLSearchParams)=>URLSearchParams)=>{params=update(params);}]},
    "@/lib/utils":{generateUUID:()=>String(++epoch)},"@/lib/sdkAuthorityScope":{sdkAuthorityScope},
    "@/store/useAuthStore":{useAuthStore:Object.assign((fn:(v:typeof auth)=>unknown)=>fn(auth),{getState:()=>auth,subscribe:(fn:(next:typeof auth,old:typeof auth)=>void)=>{subscribers.add(fn);return()=>subscribers.delete(fn);}})},
    "@/lib/api":{api:{get:async(path:string)=>{reads.push(path);return{data:await read(path)};},post:async(path:string,body:unknown)=>{posts.push({path,body});return{data:await post()};}}},
  };
  for(const[name,path]of[["@/api/scheduledTasks","../../api/scheduledTasks.ts"],["@/hooks/useAuthenticatedReadScope","../../hooks/useAuthenticatedReadScope.ts"],["component","./components/ScheduledTaskActions.tsx"],["history","./components/ScheduledHistoryBrowser.tsx"]]){
    const exports={};vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path,import.meta.url),"utf8"),{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.CommonJS,jsx:ts.JsxEmit.ReactJSX}}).outputText,{exports,AbortController,URLSearchParams,document:{visibilityState:"visible",addEventListener:()=>{},removeEventListener:()=>{}},window:{setInterval:(fn:()=>void)=>{timers.set(++timer,fn);return timer;},clearInterval:(id:number)=>timers.delete(id)},require:(name:string)=>{assert.ok(name in modules,name);return modules[name];}});modules[name]=exports;
  }
  function render(){
    cursor=0;effects=[];slots=parent.slots;cleanups=parent.cleanups;
    const component=(modules.component as {ScheduledTaskActions:(props:unknown)=>Node}).ScheduledTaskActions;
    let tree:Node;
    if(!composed) tree=component({taskId:id,task:source,available,onObserved:()=>{observed++;}});
    else {
      const used=new Set<string>();
      tree=(modules.history as {ScheduledHistoryBrowser:(props:unknown)=>Node}).ScheduledHistoryBrowser({revision:0,
        renderActions:(taskId:string,task:unknown,available:boolean)=>({type:component,key:taskId,props:{taskId,task,available,onObserved:()=>{observed++;}}})});
      function expand(value:unknown):unknown {
        if(Array.isArray(value))return value.map(expand);if(!value||typeof value!=="object")return value;
        const n=value as Node & {key?:string};if(!n.props)return value;
        if(n.type===component){const key=n.key!;used.add(key);if(!children.has(key))children.set(key,{slots:[],cleanups:[]});const lane=children.get(key)!;slots=lane.slots;cleanups=lane.cleanups;cursor=0;return component(n.props);}
        return {...n,props:{...n.props,children:expand(n.props.children)}};
      }
      tree=expand(tree) as Node;
      for(const[key,lane]of children)if(!used.has(key)){lane.cleanups.forEach(fn=>fn());children.delete(key);}
    }
    effects.forEach(fn=>fn());return tree;
  }
  async function flush(){let tree=render();for(let n=0;n<4;n++){await settle();tree=render();}return tree;}

  return{posts,reads,api:modules["@/api/scheduledTasks"] as {changeScheduledTask:(id:string,action:string)=>Promise<void>;readSchedule:(id:string)=>Promise<unknown>},render,flush,navigate(value:string){params=new URLSearchParams(`task=${value}`);},tick(){for(const fn of [...timers.values()])fn();},
    text:()=>shown(render()),button(key:string){const n=nodes(render()).find(n=>n.type==="Button"&&shown(n).includes(key));assert.ok(n,key);return n;},
    click(key:string){const n=this.button(key);assert.notEqual(n.props.disabled,true);(n.props.onClick as ()=>void)();},
    setRead(fn:typeof read){read=fn;},setPost(fn:typeof post){post=fn;},setSource(value:unknown,enabled=true){source=value;available=enabled;},
    changeAuth(token:string){const old=auth;auth={...auth,token};subscribers.forEach(fn=>fn(auth,old));},
    unmount(){parent.cleanups.forEach(fn=>fn());children.forEach(lane=>lane.cleanups.forEach(fn=>fn()));},get observed(){return observed;},
  };
}
test("action API rejects mismatched accepted schedule identity",async()=>{const h=host();h.setPost(async()=>({task_id:other,status:"paused",cancellation_requested:false}));await assert.rejects(h.api.changeScheduledTask(id,"pause"));assert.equal(h.posts.length,1);});
test("action API rejects malformed cancellation acknowledgement",async()=>{const h=host();h.setPost(async()=>({task_id:id,status:"cancelled",cancellation_requested:false}));await assert.rejects(h.api.changeScheduledTask(id,"cancel"));assert.equal(h.posts.length,1);});
test("accepted pause is followed by exact read and separately labelled current state",async()=>{const h=host();let reads=0;h.setRead(async()=>({task:task(++reads===1?"active":"paused")}));h.click("scheduled.pause");await settle();assert.equal(h.posts.length,1);assert.equal(h.reads.length,2);assert.match(h.text(),/scheduleActions.accepted/);assert.match(h.text(),/paused/);assert.equal(h.observed,1);});
test("lost response keeps unknown even when current GET matches requested state",async()=>{const h=host();let n=0;h.setRead(async()=>({task:task(++n===1?"active":"paused")}));h.setPost(async()=>{throw Error("lost");});h.click("scheduled.pause");await settle();assert.match(h.text(),/scheduleActions.unknown/);assert.match(h.text(),/paused/);h.click("scheduleActions.recheck");await settle();assert.equal(h.posts.length,1);assert.equal(h.reads.length,3);});
test("failed readback retains accepted action and read-only recovery across detail error",async()=>{const h=host();let n=0;h.setRead(async()=>{if(++n>1)throw Error("read unavailable");return{task:task()};});h.click("scheduled.pause");await settle();h.setSource(null,false);assert.match(h.text(),/scheduleActions.accepted/);assert.match(h.text(),/scheduleActions.unavailable/);h.setRead(async()=>({task:task("paused")}));h.click("scheduleActions.recheck");await settle();assert.equal(h.posts.length,1);assert.match(h.text(),/paused/);});
test("changed preflight state never posts a stale action",async()=>{const h=host();h.setRead(async()=>({task:task("paused")}));h.click("scheduled.pause");await settle();assert.equal(h.posts.length,0);assert.match(h.text(),/scheduleActions.notSent/);assert.match(h.text(),/paused/);});
test("preflight read failure does not dispatch and recheck only reads",async()=>{const h=host();h.setRead(async()=>{throw Error("unavailable");});h.click("scheduled.pause");await settle();assert.equal(h.posts.length,0);assert.match(h.text(),/scheduleActions.notSent/);h.click("scheduleActions.recheck");await settle();assert.equal(h.posts.length,0);});
test("cancel requires consequence confirmation and never claims execution stopped",async()=>{const h=host();h.setPost(async()=>({task_id:id,status:"cancelled",cancellation_requested:true}));h.click("scheduled.cancel");assert.equal(h.posts.length,0);assert.match(h.text(),/scheduleActions.cancelConfirm/);h.click("scheduleActions.confirmCancel");await settle();assert.equal(h.posts[0].path,`/api/v1/scheduled-tasks/${id}/cancel`);assert.match(h.text(),/scheduleActions.cancelLimit/);});
test("dismissed cancel callback cannot send later",async()=>{const h=host();h.click("scheduled.cancel");const submit=h.button("scheduleActions.confirmCancel").props.onClick as ()=>void;h.click("scheduleActions.keep");submit();await settle();assert.equal(h.posts.length,0);});
test("duplicate same-tick and retired completed callbacks do not repeat POST",async()=>{const h=host();const pending=deferred();h.setPost(()=>pending.promise);const click=h.button("scheduled.pause").props.onClick as ()=>void;click();click();await settle();assert.equal(h.posts.length,1);pending.resolve({task_id:id,status:"paused",cancellation_requested:false});await settle();click();await settle();assert.equal(h.posts.length,1);});
for(const stage of ["preflight","post","readback"]){for(const retire of ["auth","unmount"]){test(`${retire} during ${stage} fences late completion`,async()=>{const h=host();const pending=deferred();let n=0;h.setRead(async()=>{n++;if(stage==="preflight"||stage==="readback"&&n===2)return pending.promise;return{task:task()};});if(stage==="post")h.setPost(()=>pending.promise);h.click("scheduled.pause");await settle();if(retire==="auth")h.changeAuth("b");else h.unmount();pending.resolve(stage==="post"?{task_id:id,status:"paused",cancellation_requested:false}:{task:task("paused")});await settle();assert.equal(h.posts.length,stage==="preflight"?0:1);assert.equal(h.observed,0);});}}
test("resume uses exact endpoint and validates active acknowledgement",async()=>{const h=host();h.setSource(task("paused"));let n=0;h.setRead(async()=>({task:task(++n===1?"paused":"active")}));h.setPost(async()=>({task_id:id,status:"active",cancellation_requested:false}));h.click("scheduled.resume");await settle();assert.equal(h.posts[0].path,`/api/v1/scheduled-tasks/${id}/resume`);assert.match(h.text(),/scheduleActions.accepted/);});
test("malformed exact receipt does not become observed status",async()=>{const h=host();let n=0;h.setRead(async()=>({task:++n===1?task():{...task(),status:"invented"}}));h.click("scheduled.pause");await settle();assert.match(h.text(),/scheduleActions.unavailable/);assert.doesNotMatch(h.text(),/invented/);});
test("new action requires explicit review and new preflight",async()=>{const h=host();h.click("scheduled.pause");await settle();assert.equal(h.posts.length,1);assert.equal(nodes(h.render()).some(n=>shown(n)==="tasks.scheduled.pause"),false);h.click("scheduleActions.newAction");h.click("scheduled.pause");await settle();assert.equal(h.posts.length,2);assert.equal(h.reads.length,4);});

function historyRead(path:string) {
  if(path.endsWith("/executions"))return {executions:[],next_cursor:null};
  if(path.endsWith(id))return {task:task()};
  if(path.endsWith(other))return {task:{...task(),task_id:other,title:"Other schedule"}};
  return {tasks:[task()],next_cursor:null};
}
test("composed history keeps the accepted action receipt through background exact GET failure",async()=>{
  const h=host(true);h.setRead(async path=>historyRead(path));await h.flush();h.click("scheduled.pause");await settle();await h.flush();
  assert.match(h.text(),/scheduleActions.accepted/);
  h.setRead(async path=>{if(path.endsWith(id))throw Error("unavailable");return historyRead(path);});h.tick();await h.flush();
  assert.match(h.text(),/scheduled.exactUnavailable/);assert.match(h.text(),/scheduleActions.accepted/);assert.equal(h.posts.length,1);
});
test("composed selection change disposes a retained cancellation confirmation",async()=>{
  const h=host(true);h.setRead(async path=>historyRead(path));await h.flush();h.click("scheduled.cancel");const confirm=h.button("scheduleActions.confirmCancel").props.onClick as ()=>void;
  h.navigate(other);await h.flush();confirm();await settle();assert.equal(h.posts.length,0);assert.doesNotMatch(h.text(),/scheduleActions.cancelConfirm/);assert.match(h.text(),/Other schedule/);
});
test("composed A to B to A selection does not revive a late action response",async()=>{
  const h=host(true);h.setRead(async path=>historyRead(path));const pending=deferred();h.setPost(()=>pending.promise);await h.flush();h.click("scheduled.pause");await settle();assert.equal(h.posts.length,1);
  h.navigate(other);await h.flush();h.navigate(id);await h.flush();pending.resolve({task_id:id,status:"paused",cancellation_requested:false});await settle();await h.flush();
  assert.equal(h.observed,0);assert.doesNotMatch(h.text(),/scheduleActions.accepted/);assert.equal(h.posts.length,1);
});
