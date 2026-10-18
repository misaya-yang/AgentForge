/** ACP v1 over stdio, translated into the CLI-owned local App Server. The
 * adapter never creates an Agent loop or connects to the hosted Runtime. */
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { realpathSync } from "node:fs";
import { isAbsolute } from "node:path";
import type { Readable, Writable } from "node:stream";
type Id=string|number;
type Message={jsonrpc?:string;id?:Id;method?:string;params?:any;result?:any;error?:{code:number;message:string}};
const MAX_LINE=1024*1024;
class Rpc {
  private counter=0; private pending=new Map<Id,{resolve:(v:any)=>void;reject:(e:Error)=>void;cleanup:()=>void}>();
  private buffer=Buffer.alloc(0); private closed=false;
  handler?:(message:Message)=>Promise<void>|void;
  constructor(input:Readable,private output:Writable,private failure:()=>void){
    input.on("data",(chunk:Buffer|string)=>{
      if(this.closed)return;
      this.buffer=Buffer.concat([this.buffer,Buffer.from(chunk)]);
      if(this.buffer.length>2*MAX_LINE){this.close();failure();return;}
      let end;
      while((end=this.buffer.indexOf(10))>=0){
        const line=this.buffer.subarray(0,end);this.buffer=this.buffer.subarray(end+1);
        if(line.length>MAX_LINE){this.close();failure();return;}
        if(!line.length)continue;
        try {
          const message=JSON.parse(line.toString("utf8")) as Message;
          if(!message || typeof message!=="object" || Array.isArray(message))throw new Error();
          if(message.id!==undefined && !["number","string"].includes(typeof message.id))throw new Error();
          if(!message.method && message.id!==undefined){const pending=this.pending.get(message.id);if(pending){this.pending.delete(message.id);pending.cleanup();message.error?pending.reject(new Error("Local runtime request rejected")):pending.resolve(message.result)}}
          else Promise.resolve(this.handler?.(message)).catch(()=>{try{this.error(message.id,-32603,"Request failed")}catch{this.close();failure()}});
        }catch{this.close();failure();return;}
      }
      if(this.buffer.length>MAX_LINE){this.close();failure();}
    });
    input.on("error",()=>{this.close();failure()});
    input.on("end",()=>{this.close();failure()});
    output.on("error",()=>{this.close();failure()});
  }
  send(value:Message){if(this.closed)throw new Error("RPC peer closed");const line=JSON.stringify(value);if(Buffer.byteLength(line)>MAX_LINE || this.output.writableLength>4*MAX_LINE)throw new Error("RPC output limit");this.output.write(`${line}\n`)}
  notify(method:string,params:any){this.send({jsonrpc:"2.0",method,params})}
  result(id:Id|undefined,result:any){if(id!==undefined)this.send({jsonrpc:"2.0",id,result})}
  error(id:Id|undefined,code:number,message:string){if(id!==undefined && !this.closed)this.send({jsonrpc:"2.0",id,error:{code,message}})}
  request(method:string,params:any,signal?:AbortSignal):Promise<any>{
    if(this.pending.size>=32)return Promise.reject(new Error("Too many pending requests"));
    const id=`bridge-${++this.counter}`;
    return new Promise((resolve,reject)=>{
      const abort=()=>{this.pending.delete(id);cleanup();reject(new Error("Request cancelled"))};
      const cleanup=()=>signal?.removeEventListener("abort",abort);
      this.pending.set(id,{resolve,reject,cleanup});signal?.addEventListener("abort",abort,{once:true});
      if(signal?.aborted){abort();return;}
      try{this.send({jsonrpc:"2.0",id,method,params})}catch(e){this.pending.delete(id);cleanup();reject(e)}
    });
  }
  close(){if(this.closed)return;this.closed=true;for(const p of this.pending.values()){p.cleanup();p.reject(new Error("RPC peer closed"))};this.pending.clear()}
}
interface Session { cwd:string; active?:{id:Id;turn?:string;cancelled:boolean;permissions:AbortController;ready:Promise<void>;finish:(reason:string)=>void}; deltas:Set<string> }
export interface AcpOptions { input?:Readable; output?:Writable; spawnProcess?:typeof spawn; signal?:AbortSignal }
export async function runAcpStdio(binary:string,env:NodeJS.ProcessEnv,options:AcpOptions={}):Promise<number>{
  const child=(options.spawnProcess??spawn)(binary,["app-server","--listen","stdio://"],{stdio:["pipe","pipe","pipe"],env,windowsHide:true}) as ChildProcessWithoutNullStreams;
  // Runtime diagnostics are suppressed. ACP stdout is exclusively JSON-RPC.
  child.stderr.on("data",()=>undefined);
  return attachAcp(child,options.input??process.stdin,options.output??process.stdout,options.signal);
}
export function attachAcp(child:ChildProcessWithoutNullStreams,input:Readable,output:Writable,signal?:AbortSignal):Promise<number>{
  return new Promise(resolve=>{
    let stopped=false,initialized=false;let killTimer:NodeJS.Timeout|undefined;
    const sessions=new Map<string,Session>();
    let editor:Rpc;let upstream:Rpc;
    const close=(code=0)=>{if(stopped)return;stopped=true;for(const s of sessions.values()){try{s.active?.finish("cancelled")}catch{}}editor?.close();upstream?.close();signal?.removeEventListener("abort",abort);child.kill("SIGTERM");killTimer=setTimeout(()=>child.kill("SIGKILL"),1000);killTimer.unref();resolve(code)};
    const abort=()=>close(130);
    editor=new Rpc(input,output,()=>close());
    upstream=new Rpc(child.stdout,child.stdin,()=>close(1));
    const update=(id:string,update:any)=>editor.notify("session/update",{sessionId:id,update});
    const session=(id:unknown):[string,Session]=>{if(typeof id!=="string" || !sessions.has(id))throw new Error("Unknown session");return [id,sessions.get(id)!]};
    const directory=(cwd:unknown)=>{if(typeof cwd!=="string" || !isAbsolute(cwd))throw new Error("Absolute cwd required");return realpathSync(cwd)};
    const cancelled=async(id:string,s:Session)=>{if(s.active?.turn)await upstream.request("turn/interrupt",{threadId:id,turnId:s.active.turn})};
    editor.handler=async message=>{
      const p=message.params??{};
      try {
        switch(message.method){
          case "initialize":{
            if(initialized || p.protocolVersion!==1){editor.error(message.id,-32602,"ACP protocolVersion 1 required");return;}
            await upstream.request("initialize",{clientInfo:{name:"agentforge-acp",version:"1.0.0",title:"AgentForge ACP"},capabilities:{experimentalApi:false}});
            upstream.notify("initialized",{});initialized=true;
            editor.result(message.id,{protocolVersion:1,agentCapabilities:{loadSession:true,promptCapabilities:{image:false,audio:false,embeddedContext:false},mcpCapabilities:{http:false,sse:false}},agentInfo:{name:"agentforge",title:"AgentForge",version:"1.0.0"},authMethods:[]});return;
          }
          case "session/new":case "session/load":{
            if(!initialized)throw new Error("Initialize first");
            if(p.mcpServers && (!Array.isArray(p.mcpServers)||p.mcpServers.length))throw new Error("MCP configuration is CLI-owned");
            const cwd=directory(p.cwd);
            const load=message.method==="session/load";
            if(load && (typeof p.sessionId!=="string" || !p.sessionId || p.sessionId.length>255))throw new Error("Invalid session");
            const result=await upstream.request(load?"thread/resume":"thread/start",{...(load?{threadId:p.sessionId}:{}),cwd});
            const id=result?.thread?.id;
            if(typeof id!=="string" || !id || (load && id!==p.sessionId) || (result.cwd && directory(result.cwd)!==cwd))throw new Error("Runtime session binding mismatch");
            if(sessions.has(id))throw new Error("Session already attached");
            if(sessions.size>=128)throw new Error("Session limit");
            sessions.set(id,{cwd,deltas:new Set()});editor.result(message.id,{...(load?{}:{sessionId:id})});return;
          }
          case "session/prompt":{
            const [id,s]=session(p.sessionId);if(s.active || message.id===undefined)throw new Error("A turn is already active");
            if(!Array.isArray(p.prompt)||!p.prompt.length||p.prompt.length>128||p.prompt.some((c:any)=>c?.type!=="text"||typeof c.text!=="string"))throw new Error("Only text prompts are supported");
            const text=p.prompt.map((c:any)=>c.text).join("\n");if(Buffer.byteLength(text)>256*1024)throw new Error("Prompt too large");
            let ready!:()=>void;
            const startedReady=new Promise<void>(resolve=>{ready=resolve});
            const active:NonNullable<Session["active"]>={id:message.id,cancelled:false,permissions:new AbortController(),ready:startedReady,finish:reason=>{if(s.active!==active)return;active.permissions.abort();s.active=undefined;editor.result(active.id,{stopReason:reason})}};
            s.deltas.clear();s.active=active;
            try {
              const started=await upstream.request("turn/start",{threadId:id,input:[{type:"text",text,text_elements:[]}]});
              if(typeof started?.turn?.id!=="string")throw new Error("Missing turn identity");
              active.turn=started.turn.id;ready();
              if(active.cancelled && s.active===active)await cancelled(id,s);
            }catch{ready();if(s.active===active){active.permissions.abort();s.active=undefined;editor.error(active.id,-32603,"Local turn could not be started; inspect session before retrying")}}
            return;
          }
          case "session/cancel":{
            const [id,s]=session(p.sessionId);if(s.active){s.active.cancelled=true;s.active.permissions.abort();await cancelled(id,s)}editor.result(message.id,{});return;
          }
          default:editor.error(message.id,-32601,"ACP method not supported");
        }
      }catch{editor.error(message.id,-32602,"Invalid request or unavailable local session")}
    };
    upstream.handler=async message=>{
      const p=message.params??{};
      const id=p.threadId;
      const s=typeof id==="string"?sessions.get(id):undefined;
      if(s?.active && !s.active.turn)await s.active.ready;
      if(message.id!==undefined){
        if(!s || !s.active || !s.active.turn || typeof p.itemId!=="string" || !p.itemId || (s.active.turn && p.turnId!==s.active.turn)){upstream.error(message.id,-32602,"Approval scope mismatch");return;}
        if(message.method==="item/commandExecution/requestApproval" || message.method==="item/fileChange/requestApproval"){
          const active=s.active;
          const title=message.method.includes("commandExecution")?"Run local command":"Apply local file changes";
          try {
            const decision=await editor.request("session/request_permission",{sessionId:id,toolCall:{toolCallId:p.itemId,title,status:"pending",kind:message.method.includes("commandExecution")?"execute":"edit",rawInput:p},options:[{optionId:"allow_once",name:"Allow once",kind:"allow_once"},{optionId:"reject_once",name:"Reject",kind:"reject_once"}]},active.permissions.signal);
            const allow=decision?.outcome?.outcome==="selected" && decision.outcome.optionId==="allow_once" && s.active===active && p.turnId===active.turn && !active.cancelled;
            const choices=p.availableDecisions;
            const accepts=!Array.isArray(choices)||choices.some((v:any)=>v==="accept");
            upstream.result(message.id,{decision:allow&&accepts?"accept":"decline"});
          }catch{upstream.result(message.id,{decision:"decline"})}
        }else upstream.error(message.id,-32601,"Unsupported runtime request; denied");
        return;
      }
      if(!s || !s.active)return;
      if(s.active.turn && p.turnId && p.turnId!==s.active.turn)return;
      if(message.method==="item/agentMessage/delta" && typeof p.delta==="string"){
        s.deltas.add(String(p.itemId));update(id,{sessionUpdate:"agent_message_chunk",content:{type:"text",text:p.delta}});
      }else if(message.method==="turn/completed"){
        if(p.turn?.id && s.active.turn && p.turn.id!==s.active.turn)return;
        const status=p.turn?.status;
        if(!["completed","interrupted","failed"].includes(status)){s.active.permissions.abort();editor.error(s.active.id,-32603,"Unknown local turn outcome; inspect session before retrying");s.active=undefined;return;}
        if(status==="failed"){s.active.permissions.abort();editor.error(s.active.id,-32603,"Local turn failed; inspect session before retrying");s.active=undefined}
        else s.active.finish(s.active.cancelled||status==="interrupted"?"cancelled":"end_turn");
      }else if(message.method==="item/started" || message.method==="item/completed"){
        const item=p.item; if(!item||typeof item.id!=="string")return;
        if(item.type==="agentMessage" && message.method==="item/completed" && !s.deltas.has(item.id) && typeof item.text==="string")update(id,{sessionUpdate:"agent_message_chunk",content:{type:"text",text:item.text}});
        else if(["commandExecution","fileChange","mcpToolCall"].includes(item.type))update(id,{sessionUpdate:message.method==="item/started"?"tool_call":"tool_call_update",toolCallId:item.id,title:item.type,status:message.method==="item/started"?"in_progress":item.status==="failed"?"failed":"completed",kind:item.type==="commandExecution"?"execute":item.type==="fileChange"?"edit":"other"});
      }
    };
    child.once("error",()=>close(1));child.once("close",code=>{if(killTimer)clearTimeout(killTimer);close(code??1)});
    signal?.addEventListener("abort",abort,{once:true});if(signal?.aborted)abort();
  });
}
