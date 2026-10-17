import assert from "node:assert/strict";
import test from "node:test";
import type { BrowserContext, FullConfig } from "@playwright/test";
import { authenticateConsoleContext, existingCredentials, requireHttpsConsoleOrigin, CONSOLE_COOKIE } from "./support/bff-session.ts";
import globalSetup from "./global.setup.ts";
const origin="https://console.example";
const credentials={email:"existing@example.invalid",password:"synthetic-only-password"};
function fixture(change:{loginStatus?:number;login?:Record<string,unknown>;cookie?:Record<string,unknown>|null;session?:Record<string,unknown>;bootstrapStatus?:number}={}) {
  const user={user_id:"existing-user",tenant_id:"tenant-a",force_password_change:false};
  const login=change.login??{session_type:"cookie",csrf_token:"synthetic-csrf",user,force_password_change:false};
  const session=change.session??{csrf_token:"synthetic-csrf",user,expires_at:Date.now()/1000+10800};
  const cookie=change.cookie===null?null:{name:CONSOLE_COOKIE,value:"opaque-synthetic",domain:"console.example",path:"/",httpOnly:true,secure:true,sameSite:"Lax",...change.cookie};
  const calls:Array<{method:string;url:string;options:Record<string,unknown>}>=[];
  const context={request:{
    async post(url:string,options:Record<string,unknown>){calls.push({method:"POST",url,options});return {status:()=>change.loginStatus??200,json:async()=>login};},
    async get(url:string,options:Record<string,unknown>){calls.push({method:"GET",url,options});return {status:()=>change.bootstrapStatus??200,json:async()=>session};},
  },async cookies(url:string){assert.equal(url,origin);return cookie?[cookie]:[];}} as unknown as BrowserContext;
  return {context,calls};
}
test("Console existing-account selection never provisions, guesses passwords or mutates credentials",()=>{
  assert.deepEqual(existingCredentials({persistedCredentials:credentials}),credentials);
  for(const options of [{persistedCredentials:null},{persistedCredentials:credentials,providedEmail:"partial"},{persistedCredentials:credentials,provisionAccounts:true}]) assert.throws(()=>existingCredentials(options),/requires configured|provided together|existing-account-only/);
});
test("HTTP, URL credentials, foreign paths and absent HTTPS/BFF configuration fail before login",async()=>{
  for(const url of [undefined,"http://127.0.0.1:8081","https://user:password@console.example","https://console.example/path","https://console.example/?token=x"]){
    assert.throws(()=>requireHttpsConsoleOrigin(url),/HTTPS/);
  }
  const h=fixture();await assert.rejects(authenticateConsoleContext(h.context,"http://console.example",credentials),/HTTPS/);assert.equal(h.calls.length,0);
  assert.equal(requireHttpsConsoleOrigin(origin),origin);
});
test("real setup rejects HTTP and TLS bypass before credential file or browser actions",async()=>{
  for(const use of [{baseURL:"http://127.0.0.1:8081"},{baseURL:origin,ignoreHTTPSErrors:true}]){
    await assert.rejects(globalSetup({projects:[{use}]} as unknown as FullConfig),/HTTPS|TLS bypass/);
  }
});
test("BFF cookie jar login/bootstrap use exact endpoints and same-origin policy without bearer",async()=>{
  const h=fixture();const session=await authenticateConsoleContext(h.context,origin,credentials);
  assert.equal(session.user.tenant_id,"tenant-a");assert.equal(session.csrf_token,"synthetic-csrf");
  assert.deepEqual(h.calls.map(({method,url})=>({method,url})),[{method:"POST",url:`${origin}/api/auth/session`},{method:"GET",url:`${origin}/api/auth/session`}]);
  for(const request of h.calls){assert.deepEqual(request.options.headers,{Origin:origin,"Sec-Fetch-Site":"same-origin"});assert.equal(request.options.maxRedirects,0);assert.equal("Authorization" in (request.options.headers as object),false);}
});
test("API-only, redirects and legacy bearer responses cannot silently seed a Console",async()=>{
  for(const change of [{loginStatus:404},{loginStatus:302},{login:{access_token:"PRIVATE_SYNTHETIC",user:{user_id:"existing-user"}}},{login:{session_type:"cookie",csrf_token:"synthetic-csrf",user:{user_id:"existing-user"},force_password_change:true}}]){
    const h=fixture(change);await assert.rejects(authenticateConsoleContext(h.context,origin,credentials),/BFF|cannot change credentials/);assert.equal(h.calls.length,1);
  }
});
test("every __Host cookie attribute is required, with no client-side or plaintext fallback",async()=>{
  for(const cookie of [null,{secure:false},{httpOnly:false},{sameSite:"None"},{path:"/api"},{domain:".console.example"},{value:""}]){
    const h=fixture({cookie});await assert.rejects(authenticateConsoleContext(h.context,origin,credentials),/cookie is missing|cookie.*scope/);assert.equal(h.calls.length,1);
  }
});
test("GET bootstrap checks cookie-bound user, tenant, CSRF, expiry and revocation",async()=>{
  const good={user:{user_id:"existing-user",tenant_id:"tenant-a"},csrf_token:"synthetic-csrf",expires_at:Date.now()/1000+10800};
  for(const change of [{bootstrapStatus:401},{session:{...good,access_token:"PRIVATE_SYNTHETIC"}},{session:{...good,user:{...good.user,user_id:"foreign-user"}}},{session:{...good,user:{user_id:"existing-user"}}},{session:{...good,csrf_token:"foreign-csrf"}},{session:{...good,expires_at:1}},{session:{...good,user:{...good.user,force_password_change:true}}}]){
    const h=fixture(change);await assert.rejects(authenticateConsoleContext(h.context,origin,credentials),/bootstrap|cannot change credentials/);assert.equal(h.calls.length,2);
  }
});

test("API bearer helper refuses a Console cookie jar before reading account credentials",async()=>{
  const {loginThroughApi}=await import("./support/helpers.ts");
  const context={storageState:async()=>({cookies:[{name:CONSOLE_COOKIE}],origins:[]})} as unknown as import("@playwright/test").APIRequestContext;
  await assert.rejects(loginThroughApi(context),/isolated APIRequestContext without Console cookies/);
});
