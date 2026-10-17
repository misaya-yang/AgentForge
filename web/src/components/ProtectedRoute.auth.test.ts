import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as React from "react";
import * as jsx from "react/jsx-runtime";
import { renderToString } from "react-dom/server";
function render(validation:"idle"|"checking"|"validated",authenticated:boolean) {
  const state={hydrated:true,sessionValidation:validation,isAuthenticated:authenticated,user:authenticated?{roles:["user"]}:null,hasAnyPermission:()=>true,hasAllPermissions:()=>true};
  const modules:Record<string,unknown>={"react/jsx-runtime":jsx,"react-router-dom":{useLocation:()=>({pathname:"/guarded"}),Navigate:({to}:{to:string})=>React.createElement("span",{"data-redirect":to})},"@/store/useAuthStore":{useAuthStore:(select:(state:typeof state)=>unknown)=>select(state)},"@/components/SystemStatusPage":{ForbiddenPage:()=>null}};
  const exports:Record<string,unknown>={};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./ProtectedRoute.tsx",import.meta.url),"utf8"),{compilerOptions:{module:ts.ModuleKind.CommonJS,jsx:ts.JsxEmit.ReactJSX}}).outputText,{exports,require:(name:string)=>{assert.ok(name in modules,name);return modules[name];}});
  return renderToString(React.createElement(exports.ProtectedRoute as React.ComponentType<{children:React.ReactNode}>,null,React.createElement("main",null,"Protected content")));
}
test("cookie bootstrap idle/checking cannot render or prematurely redirect protected content",()=>{
  for(const validation of ["idle","checking"] as const)for(const authenticated of [false,true]){
    const html=render(validation,authenticated);assert.match(html,/Loading/);assert.doesNotMatch(html,/Protected content|data-redirect/);
  }
});
test("validated absent/revoked session follows the original login redirect",()=>{
  const html=render("validated",false);assert.match(html,/data-redirect="\/login"/);assert.doesNotMatch(html,/Protected content/);
});
test("validated cookie session renders the original protected child",()=>{
  const html=render("validated",true);assert.match(html,/Protected content/);assert.doesNotMatch(html,/Loading|data-redirect/);
});
