/** Hosted login delegates to the release-signed native OS-vault helper. No token
 * is written into the independent Runtime home or the legacy JSON config. */
import { spawn } from "node:child_process";
import { isIP } from "node:net";
import { closeSync, constants, fstatSync, openSync, readSync } from "node:fs";
import { resolveClientAuthHelper } from "../native/launcher.js";
import type { CLIConfig } from "../types/config.js";
export function gatewayOrigin(raw:string):string {
  const url=new URL(raw);
  const host=url.hostname.replace(/^\[|\]$/g,"");
  if (url.username || url.password || url.search || url.hash || url.pathname!=="/"
      || !(url.protocol==="https:" || url.protocol==="http:" && isIP(host)>0 && (host==="::1" || host.startsWith("127.")))) throw new Error("Use HTTPS or literal loopback Gateway origin");
  return url.origin;
}
export interface HelperDependencies { binary?:()=>string; spawnProcess?:typeof spawn; env?:NodeJS.ProcessEnv; signal?:AbortSignal }
async function helper(args:string[],input:string|undefined,options:HelperDependencies={},progress?:(value:unknown)=>void):Promise<unknown> {
  const binary=(options.binary??resolveClientAuthHelper)();
  return new Promise((resolve,reject)=>{
    const env={...(options.env??process.env)};
    // A verified executable must not load user-selected native libraries at startup.
    for(const key of Object.keys(env))if(/^(?:DYLD_|LD_)/i.test(key)||/^(?:RUST_LOG|RUST_BACKTRACE)$/i.test(key))delete env[key];
    const child=(options.spawnProcess??spawn)(binary,args,{stdio:["pipe","pipe","pipe"],windowsHide:true,env});
    let output="",size=0; let settled=false,events=0;
    const deadline=setTimeout(()=>{child.kill();finish(new Error("Authentication timed out; inspect login status"))},args[1]==="login"?660000:90000);deadline.unref();
    let forceKill:NodeJS.Timeout|undefined;
    const finish=(error?:Error)=>{if(settled)return;settled=true;clearTimeout(deadline);options.signal?.removeEventListener("abort",abort);if(error){child.kill();forceKill=setTimeout(()=>child.kill("SIGKILL"),1000);forceKill.unref();reject(error)}else resolve(undefined)};
    const abort=()=>{child.kill();finish(new Error("Authentication cancelled"))};
    options.signal?.addEventListener("abort",abort,{once:true}); if(options.signal?.aborted)abort();
    child.stdout!.on("data",(chunk:Buffer)=>{
      size+=chunk.length;if(size>65536){child.kill();finish(new Error("Invalid authentication response"));return;}
      output+=chunk.toString("utf8");
      if(progress){let end;while((end=output.indexOf("\n"))>=0){const line=output.slice(0,end);output=output.slice(end+1);try{progress(JSON.parse(line));events++}catch{child.kill();finish(new Error("Invalid authentication response"));}}}
    });
    // Do not relay native helper stderr: it may include local paths or OS details.
    child.stderr!.resume();
    child.once("error",()=>finish(new Error("Native credential helper unavailable")));
    child.once("close",code=>{
      clearTimeout(deadline);if(forceKill)clearTimeout(forceKill);if(settled)return;
      if(code!==0){finish(new Error("Authentication failed; use gateway auth login"));return;}
      try {if(progress && (output.trim() || !events))throw new Error("Incomplete authentication output");const value=progress?undefined:JSON.parse(output);settled=true;options.signal?.removeEventListener("abort",abort);resolve(value)}
      catch{finish(new Error("Invalid authentication response"))}
    });
    child.stdin!.on("error",()=>finish(new Error("Credential pipe closed")));
    child.stdin!.end(input);
  });
}
export async function gatewayAccess(origin:string,options:HelperDependencies={}):Promise<{access_token:string;tenant_id:string;user_id:string;expires_at?:number}> {
  const value=await helper(["--credential-stdio"],JSON.stringify({operation:"access_token",origin:gatewayOrigin(origin),client_id:"agentforge-cli"}),options) as Record<string,unknown>;
  if(!value || typeof value.access_token!=="string" || !value.access_token || /[\r\n\0]/.test(value.access_token)
      || typeof value.tenant_id!=="string" || !value.tenant_id || typeof value.user_id!=="string" || !value.user_id)throw new Error("Invalid scoped login response");
  return value as {access_token:string;tenant_id:string;user_id:string;expires_at?:number};
}
export async function hostedConfig(config:CLIConfig,options:HelperDependencies={},keyFile?:string):Promise<CLIConfig> {
  const origin=gatewayOrigin(config.base_url);
  const environment=options.env??process.env;
  let key=environment.AI_GATEWAY_API_KEY;
  if(keyFile){
    const fd=openSync(keyFile,constants.O_RDONLY|(process.platform==="win32"?0:constants.O_NOFOLLOW));
    try{
      const info=fstatSync(fd);
      if(!info.isFile()||info.size>8192||(process.platform!=="win32"&&(info.mode&0o077)!==0))throw new Error("Invalid API key file");
      const bytes=Buffer.alloc(8193);let count=0;
      while(count<bytes.length){const n=readSync(fd,bytes,count,bytes.length-count,null);if(!n)break;count+=n;}
      if(count>8192)throw new Error("Invalid API key file");key=bytes.subarray(0,count).toString("utf8").trim();
      bytes.fill(0);
    }finally{closeSync(fd)}
  }
  if(key){if(/[\r\n\0]/.test(key))throw new Error("Invalid API key");return {...config,base_url:origin,api_key:key,access_token:undefined,access_token_provider:undefined};}
  const session=await gatewayAccess(origin,options);
  if(config.tenant_id && config.tenant_id!==session.tenant_id)throw new Error("Configured tenant differs from authenticated session");
  let current=session;
  let refreshing:Promise<string>|undefined;
  const access_token_provider=async()=>{
    if(current.expires_at && current.expires_at>Date.now()/1000+30)return current.access_token;
    if(!refreshing)refreshing=gatewayAccess(origin,{...options,signal:undefined}).then(next=>{
      if(next.tenant_id!==session.tenant_id || next.user_id!==session.user_id)throw new Error("Login authority changed; log in again");
      current=next;return next.access_token;
    }).finally(()=>{refreshing=undefined});
    return refreshing;
  };
  return {...config,base_url:origin,api_key:"",access_token:session.access_token,access_token_provider,tenant_id:session.tenant_id,user_id:session.user_id};
}
export async function runGatewayAuth(args:string[],options:HelperDependencies={},stdout:Pick<NodeJS.WriteStream,"write">=process.stdout,stderr:Pick<NodeJS.WriteStream,"write">=process.stderr):Promise<number> {
  try {
    if(args.length===1 && ["--help","-h"].includes(args[0]!)){stdout.write("Usage: ai-gateway gateway auth login|status|logout --base-url <origin>\n");return 0;}
    const [action,flag,origin,...rest]=args;
    if(!["login","status","logout"].includes(action??"") || flag!=="--base-url" || !origin || rest.length)throw new Error("Use gateway auth login|status|logout --base-url <origin>");
    await helper(["auth",action!,"--origin",gatewayOrigin(origin),"--client-id","agentforge-cli"],undefined,options,value=>{
      const v=value as Record<string,unknown>;
      if(v?.event==="device_authorization")stdout.write(`${JSON.stringify({event:v.event,verification_uri:v.verification_uri,user_code:v.user_code,expires_in:v.expires_in})}\n`);
      else if(typeof v?.authenticated==="boolean")stdout.write(`${JSON.stringify({authenticated:v.authenticated,tenant_id:v.tenant_id,user_id:v.user_id,expires_at:v.expires_at,revoked:v.revoked})}\n`);
      else throw new Error("Invalid authentication event");
    });return 0;
  }catch(error){stderr.write(`${error instanceof Error?error.message:"Authentication failed"}\n`);return options.signal?.aborted?130:1;}
}
