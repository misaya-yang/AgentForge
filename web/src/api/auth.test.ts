import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import type * as Auth from "./auth.ts";
function fixture() {
  const calls:Array<{method:string;path:string;body:unknown}>=[];
  let response:unknown={session_type:"cookie",csrf_token:"synthetic-csrf",user:{user_id:"actor"}};
  const call=async(method:string,path:string,body?:unknown)=>{calls.push({method,path,body});return {data:response};};
  const api={get:(path:string)=>call("GET",path),post:(path:string,body?:unknown)=>call("POST",path,body),delete:(path:string)=>call("DELETE",path)};
  const exports:Record<string,unknown>={};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./auth.ts",import.meta.url),"utf8"),{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.CommonJS}}).outputText,{exports,require:(name:string)=>{assert.equal(name,"../lib/api");return {api};},Error});
  return {auth:exports as typeof Auth,calls,setResponse(value:unknown){response=value;}};
}
test("browser login/bootstrap/logout/password use exactly the agreed BFF endpoints",async()=>{
  const h=fixture(),credentials={email:"fixture@example.invalid",password:"synthetic-password"};
  const logged=await h.auth.login(credentials);assert.equal(logged.csrf_token,"synthetic-csrf");assert.equal("access_token" in logged,false);
  await h.auth.getConsoleSession();await h.auth.logout();
  const change={current_password:"old",new_password:"new",confirm_password:"new"};await h.auth.changePassword(change);
  assert.deepEqual(h.calls,[{method:"POST",path:"/api/auth/session",body:credentials},{method:"GET",path:"/api/auth/session",body:undefined},{method:"DELETE",path:"/api/auth/session",body:undefined},{method:"POST",path:"/api/auth/session/password",body:change}]);
});
test("legacy bearer-only or incomplete responses fail closed",async()=>{
  const h=fixture();h.setResponse({access_token:"PRIVATE_ACCESS_TOKEN",user:{user_id:"actor"}});
  await assert.rejects(h.auth.login({email:"fixture@example.invalid",password:"synthetic"}),/Invalid Console/);
  await assert.rejects(h.auth.getConsoleSession(),/Invalid Console/);
});
