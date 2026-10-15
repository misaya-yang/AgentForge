import assert from "node:assert/strict";
import { createHash, webcrypto } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { sdkAuthorityScope } from "../../../lib/sdkAuthorityScope.ts";
const body = "Historical text 中文";
const hash = createHash("sha256").update(body).digest("hex");
type Tree = { type: unknown; props: Record<string, unknown> };
type Lane = { slots: unknown[]; cleanups: Array<()=>void> };
type Request = { kind:string; args:unknown[]; resolve:(value:unknown)=>void; reject:(error:unknown)=>void };
function nodes(value:unknown):Tree[]{if(Array.isArray(value))return value.flatMap(nodes);if(!value||typeof value!=="object")return[];const n=value as Tree;return n.props?[n,...nodes(n.props.children)]:[];}
function text(value:unknown):string{if(typeof value==="string"||typeof value==="number")return String(value);if(Array.isArray(value))return value.map(text).join(" ");return value&&typeof value==="object"?text((value as Tree).props?.children):"";}
async function settle(){for(let i=0;i<3;i++)await new Promise(resolve=>setTimeout(resolve,0));}
function version(n=1,changes:Record<string,unknown>={}){return{version_id:`v${n}`,document_id:"doc",version_number:n,content_hash:hash,created_at:"2026-10-15",word_count:3,change_type:"updated",...changes};}
function page(rows=[version(2),version(1)]){return{versions:rows,total:rows.length,current_version:2};}
function host(){
 const parent:Lane={slots:[],cleanups:[]};let child:Lane|null=null;let lane=parent;let cursor=0;let effects:Array<()=>void>=[];
 let props={datasetId:"dataset",documentId:"doc",documentTitle:"Document",open:true,onOpenChange:(open:boolean)=>{props.open=open;}};
 let auth={token:"a",isAuthenticated:true,user:{user_id:"user-a",tenant_id:"tenant-a",roles:["member"],permissions:["knowledge:read"]}};
 const subscribers=new Set<(a:typeof auth,b:typeof auth)=>void>();const requests:Request[]=[];const timers=new Map<number,()=>void>();let timer=0;
 const memo=(make:()=>unknown,deps:unknown[])=>{const i=cursor++;const old=lane.slots[i] as {value:unknown;deps:unknown[]}|undefined;if(!old||deps.some((v,n)=>!Object.is(v,old.deps[n])))lane.slots[i]={value:make(),deps};return(lane.slots[i]as{value:unknown}).value;};
 const effect=(fn:()=>void|(()=>void),deps:unknown[])=>{const owner=lane;const i=cursor++;const old=owner.slots[i]as unknown[]|undefined;if(!old||deps.some((v,n)=>!Object.is(v,old[n]))){owner.slots[i]=deps;effects.push(()=>{owner.cleanups[i]?.();owner.cleanups[i]=fn()||(()=>{});});}};
 const react={useState(initial:unknown){const owner=lane;const i=cursor++;if(!(i in owner.slots))owner.slots[i]=initial;return[owner.slots[i],(next:unknown)=>{owner.slots[i]=typeof next==="function"?next(owner.slots[i]):next;}];},useMemo:memo,useCallback:(fn:unknown,deps:unknown[])=>memo(()=>fn,deps),useRef:(value:unknown)=>memo(()=>({current:value}),[]),useEffect:effect,useLayoutEffect:effect};
 const jsx=(type:unknown,props:Record<string,unknown>)=>({type,props});const named=new Proxy({},{get:(_,name)=>String(name)});
 const request=(kind:string)=>(...args:unknown[])=>new Promise((resolve,reject)=>requests.push({kind,args,resolve,reject}));
 const modules:Record<string,unknown>={react,"react/jsx-runtime":{jsx,jsxs:jsx},"react-i18next":{useTranslation:()=>({t:(key:string)=>key,i18n:{language:"en-US"}})},"lucide-react":named,
 "@/store/useAuthStore":{useAuthStore:Object.assign((select:(value:typeof auth)=>unknown)=>select(auth),{getState:()=>auth,subscribe:(fn:(a:typeof auth,b:typeof auth)=>void)=>{subscribers.add(fn);return()=>subscribers.delete(fn);}})},"@/lib/sdkAuthorityScope":{sdkAuthorityScope},"@/hooks/use-toast":{toast:()=>{}},
 "@/api/knowledge":{listDocumentVersions:request("list"),compareDocumentVersions:request("compare"),restoreDocumentVersion:request("restore"),getActiveDocumentSource:request("active-source")},"@/lib/api":{api:{get:request("source")}},
 "@/components/ui/button":named,"@/components/ui/badge":named,"@/components/ui/dialog":named,"@/components/ui/alert-dialog":named};
 function compile(relative:string){const exports:Record<string,(props:unknown)=>Tree>={};vm.runInNewContext(ts.transpileModule(readFileSync(new URL(relative,import.meta.url),"utf8"),{compilerOptions:{module:ts.ModuleKind.CommonJS,jsx:ts.JsxEmit.ReactJSX,target:ts.ScriptTarget.ES2022}}).outputText,{exports,require:(name:string)=>{assert.ok(name in modules,name);return modules[name];},crypto:webcrypto,TextEncoder,Uint8Array,AbortController,window:{setInterval:(fn:()=>void)=>{timers.set(++timer,fn);return timer;},clearInterval:(id:number)=>timers.delete(id)}});return exports;}
 modules["./useDocumentVersionNavigation"]=compile("./useDocumentVersionNavigation.ts");
 const source=compile("../../assistant/components/SourceDocumentDialog.tsx");modules["@/pages/assistant/components/SourceDocumentDialog"]=source;
 const history=compile("./DocumentVersionHistory.tsx").DocumentVersionHistory;
 function render(){lane=parent;cursor=0;effects=[];let used=false;const tree=history(props);
  function expand(value:unknown):unknown{if(Array.isArray(value))return value.map(expand);if(!value||typeof value!=="object")return value;const n=value as Tree;if(!n.props)return value;
   if(n.type===source.SourceDocumentDialog){used=true;child??={slots:[],cleanups:[]};lane=child;cursor=0;return source.SourceDocumentDialog(n.props);}
   return{...n,props:{...n.props,children:expand(n.props.children)}};}
  const result=expand(tree)as Tree;if(!used&&child){child.cleanups.forEach(fn=>fn());child=null;}effects.forEach(fn=>fn());return result;
 }
 return{requests,render,text:()=>text(render()),async load(rows=[version(2),version(1)]){render();requests.filter(r=>r.kind==="list").at(-1)!.resolve(page(rows));await settle();render();},
 button(key:string,index=0){const buttons=nodes(render()).filter(n=>n.type==="Button"&&text(n).includes(key));assert.ok(buttons[index],key);return buttons[index];},click(key:string,index=0){const b=this.button(key,index);assert.notEqual(b.props.disabled,true);(b.props.onClick as()=>void)();render();},
 close(){const dialog=nodes(render()).find(n=>n.type==="Dialog")!;(dialog.props.onOpenChange as(open:boolean)=>void)(false);render();},
 changeAuth(token:string){const old=auth;auth={...auth,token};subscribers.forEach(fn=>fn(auth,old));},changeDocument(documentId:string){props={...props,documentId};render();},
 source:()=>requests.filter(r=>r.kind==="source").at(-1)!,unmount(){parent.cleanups.forEach(fn=>fn());child?.cleanups.forEach(fn=>fn());},timers:()=>timers.size};
}
test("history opens the exact selected historical version without restore",async()=>{const h=host();await h.load();h.click("versionHistory.viewSource",1);assert.match(String(h.source().args[0]),/\/versions\/1\/source$/);assert.equal((h.source().args[1]as{params:{hash:string}}).params.hash,hash);h.source().resolve({data:{document_id:"doc",version_number:1,content:body}});await settle();assert.match(h.text(),/Historical text 中文/);assert.equal(h.requests.filter(r=>r.kind==="restore").length,0);h.unmount();});
test("history source opening is read only and has no current-source fallback",async()=>{const h=host();await h.load();h.click("versionHistory.viewSource",1);h.source().reject(Error("denied"));await settle();assert.match(h.text(),/assistant.sourceUnavailable/);assert.equal(h.requests.filter(r=>r.kind==="active-source"||r.kind==="restore").length,0);h.unmount();});
for(const changes of [{content_hash:""},{content_hash:"bad"},{change_type:"pending_restore"},{change_type:"future_kind"}])test(`unsupported source row ${JSON.stringify(changes)} is disabled`,async()=>{const h=host();await h.load([version(1,changes)]);const b=h.button("versionHistory.viewSource");assert.equal(b.props.disabled,true);assert.equal(b.props.title,"knowledge.versionHistory.sourceUnavailable");(b.props.onClick as()=>void)();h.render();assert.equal(h.requests.filter(r=>r.kind==="source").length,0);h.unmount();});
for(const response of [{document_id:"other",version_number:1,content:body},{document_id:"doc",version_number:2,content:body},{document_id:"doc",version_number:1,content:"changed"}])test(`historical response identity mismatch ${JSON.stringify(response)} stays unavailable`,async()=>{const h=host();await h.load();h.click("versionHistory.viewSource",1);h.source().resolve({data:response});await settle();assert.match(h.text(),/assistant.sourceUnavailable/);assert.doesNotMatch(h.text(),/Historical text 中文|changed/);h.unmount();});
for(const change of ["close","document","auth","aba","compare","refresh"])test(`${change} retires a delayed source and stale row handler`,async()=>{const h=host();await h.load();const old=h.button("versionHistory.viewSource",1).props.onClick as()=>void;h.click("versionHistory.viewSource",1);const pending=h.source();if(change==="close")h.close();else if(change==="document")h.changeDocument("other");else if(change==="auth"||change==="aba"){h.changeAuth("b");if(change==="aba")h.changeAuth("a");h.render();}else if(change==="compare")h.click("versionHistory.compare");else h.click("common.refresh");old();h.render();pending.resolve({data:{document_id:"doc",version_number:1,content:body}});await settle();assert.doesNotMatch(h.text(),/Historical text 中文/);assert.equal(h.requests.filter(r=>r.kind==="source").length,1);assert.equal(h.requests.filter(r=>r.kind==="restore").length,0);h.unmount();});
test("restore confirmation dismisses preview without dispatching restore",async()=>{const h=host();await h.load();h.click("versionHistory.viewSource",1);h.click("versionHistory.rollback");h.source().resolve({data:{document_id:"doc",version_number:1,content:body}});await settle();assert.doesNotMatch(h.text(),/Historical text 中文/);assert.equal(h.requests.filter(r=>r.kind==="restore").length,0);h.unmount();});
