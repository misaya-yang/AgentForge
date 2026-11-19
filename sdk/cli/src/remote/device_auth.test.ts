import { EventEmitter } from "node:events";
import { PassThrough } from "node:stream";
import { describe,it,expect } from "vitest";
import { gatewayOrigin,gatewayAccess,hostedConfig,runGatewayAuth } from "./device_auth.js";
import { DEFAULT_CONFIG } from "../types/config.js";
function processFixture(reply:unknown){
  const requests:any[]=[];
  const spawnProcess=((binary:string,args:string[],options:any)=>{
    const stdin=new PassThrough(),stdout=new PassThrough(),stderr=new PassThrough();
    const child=Object.assign(new EventEmitter(),{stdin,stdout,stderr,kill(){return true}});
    let input="";stdin.on("data",c=>input+=c);stdin.on("finish",()=>{requests.push({binary,args,options,input});stdout.write(JSON.stringify(reply)+"\n");stdout.end();child.emit("close",0)});
    return child;
  }) as any;
  return {requests,options:{binary:()=>"/signed/platformctl",spawnProcess,env:{}}};
}
describe("Gateway device login and native credential pipes",()=>{
  it("rejects URL credentials, redirects in origins and nonloopback HTTP",()=>{
    for(const origin of ["https://u:p@gateway.test","https://gateway.test/?key=secret","http://gateway.test","http://localhost"])expect(()=>gatewayOrigin(origin)).toThrow();
    expect(gatewayOrigin("http://127.0.0.1:1234")).toBe("http://127.0.0.1:1234");
  });
  it("gets scoped access through private pipes, without argv secrets",async()=>{
    const f=processFixture({access_token:"synthetic-access",tenant_id:"tenant-one",user_id:"user-one"});
    const token=await gatewayAccess("https://gateway.test",f.options);expect(token.tenant_id).toBe("tenant-one");
    expect(f.requests[0].args).toEqual(["--credential-stdio"]);
    expect(JSON.parse(f.requests[0].input)).toEqual({operation:"access_token",origin:"https://gateway.test",client_id:"agentforge-cli"});
    expect(JSON.stringify(f.requests[0].args)).not.toContain("synthetic-access");
  });
  it("refuses a different configured tenant and never falls back to saved plaintext keys",async()=>{
    const f=processFixture({access_token:"synthetic-access",tenant_id:"tenant-one",user_id:"user-one"});
    await expect(hostedConfig({...DEFAULT_CONFIG,base_url:"https://gateway.test",tenant_id:"other",api_key:"old-plaintext"},f.options)).rejects.toThrow(/tenant/);
    const current=await hostedConfig({...DEFAULT_CONFIG,base_url:"https://gateway.test",api_key:"old-plaintext"},f.options);
    expect(current.api_key).toBe("");expect(current.access_token).toBe("synthetic-access");
  });
  it("uses explicitly supplied CI environment keys and does not launch vault helper",async()=>{
    const config=await hostedConfig({...DEFAULT_CONFIG,base_url:"https://gateway.test"},{env:{AI_GATEWAY_API_KEY:"ci-synthetic"},binary:()=>{throw new Error("must not start")}});
    expect(config.api_key).toBe("ci-synthetic");expect(config.access_token).toBeUndefined();
  });
  it("only emits sanitized login progress and errors",async()=>{
    const f=processFixture({authenticated:true,tenant_id:"t",access_token:"must-not-print",refresh_token:"must-not-print"});
    let text="";const stream={write(chunk:any){text+=chunk;return true}};
    expect(await runGatewayAuth(["status","--base-url","https://gateway.test"],f.options,stream as any,stream as any)).toBe(0);
    expect(text).not.toContain("must-not-print");
    expect(f.requests[0].args).toEqual(["auth","status","--origin","https://gateway.test","--client-id","agentforge-cli"]);
  });
  it("removes native loader injection variables before starting the signed helper",async()=>{
    const f=processFixture({access_token:"synthetic-access",tenant_id:"t",user_id:"u"});
    await gatewayAccess("https://gateway.test",{...f.options,env:{LD_PRELOAD:"untrusted.so",DYLD_INSERT_LIBRARIES:"untrusted.dylib",RUST_LOG:"trace",DBUS_SESSION_BUS_ADDRESS:"unix:path=/fixture"}});
    expect(f.requests[0].options.env).toEqual({DBUS_SESSION_BUS_ADDRESS:"unix:path=/fixture"});
  });
  it("does not accept truncated successful auth progress",async()=>{
    const spawnProcess=((_binary:string,_args:string[],_options:any)=>{
      const stdin=new PassThrough(),stdout=new PassThrough(),stderr=new PassThrough();
      const child=Object.assign(new EventEmitter(),{stdin,stdout,stderr,kill(){return true}});
      stdin.on("finish",()=>{stdout.write('{"authenticated":true');child.emit("close",0)});return child;
    }) as any;
    let text="";const stream={write(c:any){text+=c;return true}};
    expect(await runGatewayAuth(["status","--base-url","https://gateway.test"],{binary:()=>"/fixture/helper",spawnProcess,env:{}},stream as any,stream as any)).toBe(1);
    expect(text).toContain("Invalid authentication response");expect(text).not.toContain('"authenticated":true');
  });
  it("shares one rotating refresh across concurrent clients and preserves scoped authority",async()=>{
    const f=processFixture({access_token:"rotated-access",tenant_id:"tenant-one",user_id:"user-one",expires_at:1});
    const config=await hostedConfig({...DEFAULT_CONFIG,base_url:"https://gateway.test"},f.options);
    const tokens=await Promise.all([config.access_token_provider!(),config.access_token_provider!()]);
    expect(tokens).toEqual(["rotated-access","rotated-access"]);
    expect(f.requests).toHaveLength(2); // Initial read plus a single shared refresh/read.
  });

});
