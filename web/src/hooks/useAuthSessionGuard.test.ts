import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createStore } from "zustand/vanilla";
import type { ConsoleSessionResponse } from "../api/auth.ts";

function fixture() {
  let generation=0,calls=0;
  let resolve!:(value:ConsoleSessionResponse)=>void,reject!:(error:Error)=>void;
  const pending=new Promise<ConsoleSessionResponse>((yes,no)=>{resolve=yes;reject=no;});
  const store=createStore(()=>({hydrated:true,token:null as string|null,isAuthenticated:false,user:null as ConsoleSessionResponse["user"]|null,sessionValidation:"idle",rememberMe:false,csrfToken:null as string|null,
    setSessionValidation(value:string){store.setState({sessionValidation:value});},
    setAuth(csrfToken:string,user:ConsoleSessionResponse["user"]){generation++;store.setState({csrfToken,user,token:`console-session:${generation}`,isAuthenticated:true,sessionValidation:"validated"});},
    clearAuth(){generation++;store.setState({token:null,csrfToken:null,isAuthenticated:false,user:null,sessionValidation:"validated"});},
  }));
  const effects:Array<()=>void>=[];
  const useStore=Object.assign((selector:(state:ReturnType<typeof store.getState>)=>unknown)=>selector(store.getState()),{getState:store.getState});
  const modules:Record<string,unknown>={react:{useEffect(callback:()=>void){effects.push(callback);}},"@/api/auth":{getConsoleSession(){calls++;return pending;}},"@/store/useAuthStore":{useAuthStore:useStore,consoleAuthGeneration:()=>generation}};
  const exports:Record<string,unknown>={};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./useAuthSessionGuard.ts",import.meta.url),"utf8"),{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.CommonJS}}).outputText,{exports,require:(name:string)=>{assert.ok(name in modules,name);return modules[name];}});
  return {store,render(){(exports.useAuthSessionGuard as ()=>void)();effects.splice(0).forEach(effect=>effect());},resolve,reject,calls:()=>calls};
}
const session:ConsoleSessionResponse={csrf_token:"synthetic-csrf",expires_at:9999999999,user:{user_id:"actor",tenant_id:"tenant-a",email:null,display_name:null,department:null,roles:["user"],permissions:["unfiltered"],effective_permissions:[],tier:"normal",force_password_change:false}};
const settle=async()=>{await Promise.resolve();await Promise.resolve();await Promise.resolve();};
test("reload bootstraps cookie identity/CSRF before validation, with no StrictMode duplicate GET",async()=>{
  const h=fixture();h.render();h.render();assert.equal(h.calls(),1);assert.equal(h.store.getState().sessionValidation,"checking");assert.equal(h.store.getState().isAuthenticated,false);
  h.resolve(session);await settle();const state=h.store.getState();assert.equal(state.isAuthenticated,true);assert.equal(state.csrfToken,"synthetic-csrf");assert.equal(state.user!.tenant_id,"tenant-a");assert.deepEqual([...state.user!.permissions],[]);
});
test("late bootstrap cannot restore a session after null→login→logout ABA",async()=>{
  const h=fixture();h.render();h.store.getState().setAuth("new-csrf",session.user);h.store.getState().clearAuth();h.resolve(session);await settle();assert.equal(h.store.getState().isAuthenticated,false);assert.equal(h.store.getState().csrfToken,null);
});
test("revoked cookie bootstrap clears validation without retry",async()=>{
  const h=fixture();h.render();h.reject(Error("synthetic 401"));await settle();assert.equal(h.store.getState().sessionValidation,"validated");assert.equal(h.store.getState().isAuthenticated,false);assert.equal(h.calls(),1);
});
