import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as zustand from "zustand";
import * as transport from "../lib/consoleSessionTransport.ts";
import { sdkAuthorityScope } from "../lib/sdkAuthorityScope.ts";
import type { AuthState } from "./useAuthStore.ts";

function fixture(blocked=false) {
  const values=new Map([["agent-gateway-auth",JSON.stringify({state:{token:"PRIVATE_OLD_ACCESS_TOKEN",isAuthenticated:true,user:{user_id:"forged"}}})]]);
  let writes=0,cleanups=0;
  const storage={get length(){return values.size;},key:(i:number)=>[...values.keys()][i]??null,
    getItem(){throw Error("auth must never read persisted credentials");},setItem(){writes++;throw Error("auth must never persist");},removeItem(key:string){if(blocked)throw Error("storage blocked");values.delete(key);}};
  const modules:Record<string,unknown>={zustand,"@/lib/consoleSessionTransport":transport,"@/lib/sdkAuthorityScope":{sdkAuthorityScope},"@/lib/knowledgeUploadRecovery":{clearUploadRecoverySession(){cleanups++;}}};
  const exports:Record<string,unknown>={};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./useAuthStore.ts",import.meta.url),"utf8").replaceAll("import.meta.hot","__hot"),{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.CommonJS}}).outputText,
    {exports,require:(name:string)=>{assert.ok(name in modules,name);return modules[name];},localStorage:storage,sessionStorage:storage,__hot:undefined,Error});
  const store=exports.useAuthStore as {getState():AuthState;setState(value:Partial<AuthState>):void};
  return {store,values,writes:()=>writes,cleanups:()=>cleanups,generation:exports.consoleAuthGeneration as ()=>number};
}
const user={user_id:"actor-a",email:null,display_name:null,department:null,roles:["user"],permissions:["console:dashboard:view"],tier:"normal"};
test("Console starts unauthenticated, purges legacy bearer, and never persists session/CSRF",()=>{
  const h=fixture();assert.equal(h.store.getState().isAuthenticated,false);assert.equal(h.store.getState().token,null);assert.equal(h.values.size,0);
  assert.equal(h.store.getState().sessionValidation,"idle");
  h.store.getState().setAuth("csrf-only-memory",user,false,true);
  assert.match(h.store.getState().token!,/^console-session:/);assert.equal(h.store.getState().csrfToken,"csrf-only-memory");
  assert.equal(h.store.getState().isAuthenticated,true);assert.equal(h.store.getState().rememberMe,true);assert.equal(h.writes(),0);assert.equal(h.values.size,0);
  h.store.getState().clearAuth();assert.equal(h.store.getState().csrfToken,null);assert.equal(h.store.getState().token,null);assert.equal(h.store.getState().sessionValidation,"validated");
});
test("Web Storage being unavailable does not prevent cookie bootstrap/logout",()=>{
  const h=fixture(true);h.store.getState().setAuth("csrf-only-memory",user,false);h.store.getState().clearAuth();assert.equal(h.writes(),0);assert.equal(h.store.getState().isAuthenticated,false);
});
test("same-user refresh and null→login→null ABA invalidate outstanding generations",()=>{
  const h=fixture();const initial=h.generation();h.store.getState().setAuth("csrf-a",user,false);const a=h.store.getState().token;
  h.store.getState().setAuth("csrf-b",user,false);assert.notEqual(h.store.getState().token,a);assert.equal(h.cleanups(),2);
  h.store.getState().clearAuth();assert.equal(h.store.getState().token,null);assert.ok(h.generation()>initial);
});
test("invalid session bootstrap cannot authenticate or retain a bearer",()=>{
  const h=fixture();assert.throws(()=>h.store.getState().setAuth("",user,false),/Invalid Console session/);assert.equal(h.store.getState().token,null);assert.equal(h.writes(),0);
});

test("same-user tenant changes fence work even with an unchanged session marker",()=>{
  const h=fixture();h.store.getState().setAuth("csrf-a",{...user,tenant_id:"tenant-a"},false);const before=h.generation(),marker=h.store.getState().token;
  h.store.getState().updateUser({tenant_id:"tenant-b"});assert.equal(h.store.getState().token,marker);assert.ok(h.generation()>before);assert.equal(h.store.getState().user!.tenant_id,"tenant-b");
});
test("explicit anonymous logout retires an outstanding bootstrap generation",()=>{
  const h=fixture();const before=h.generation();h.store.getState().clearAuth();assert.ok(h.generation()>before);
});
