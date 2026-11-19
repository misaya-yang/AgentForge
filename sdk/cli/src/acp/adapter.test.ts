import { PassThrough } from "node:stream";
import { EventEmitter } from "node:events";
import { describe,it,expect } from "vitest";
import { attachAcp } from "./adapter.js";
function fixture(){
  const input=new PassThrough(),output=new PassThrough(),stdin=new PassThrough(),stdout=new PassThrough();
  const child=Object.assign(new EventEmitter(),{stdin,stdout,stderr:new PassThrough(),exitCode:null as number|null,kill(this:any){this.exitCode=0;this.emit("close",0);return true}});
  const messages:any[]=[];let buffer="";output.on("data",c=>{buffer+=c;let n;while((n=buffer.indexOf("\n"))>=0){messages.push(JSON.parse(buffer.slice(0,n)));buffer=buffer.slice(n+1)}});
  const requests:any[]=[];let native="";
  stdin.on("data",c=>{native+=c;let n;while((n=native.indexOf("\n"))>=0){requests.push(JSON.parse(native.slice(0,n)));native=native.slice(n+1)}});
  const completion=attachAcp(child as any,input,output);
  const wait=async(predicate:(m:any)=>boolean,list=messages)=>{for(let i=0;i<100;i++){const found=list.find(predicate);if(found)return found;await new Promise(r=>setTimeout(r,1))}throw new Error("missing fixture RPC")};
  const send=(v:any)=>input.write(JSON.stringify({jsonrpc:"2.0",...v})+"\n");
  const upstream=(v:any)=>stdout.write(JSON.stringify(v)+"\n");
  const init=async()=>{send({id:1,method:"initialize",params:{protocolVersion:1}});const req=await wait(m=>m.method==="initialize",requests);upstream({id:req.id,result:{}});await wait(m=>m.id===1)};
  const session=async()=>{send({id:2,method:"session/new",params:{cwd:process.cwd(),mcpServers:[]}});const req=await wait(m=>m.method==="thread/start",requests);upstream({id:req.id,result:{thread:{id:"thread-one"},cwd:process.cwd()}});await wait(m=>m.id===2)};
  const prompt=async()=>{send({id:3,method:"session/prompt",params:{sessionId:"thread-one",prompt:[{type:"text",text:"synthetic prompt"}]}});const req=await wait(m=>m.method==="turn/start",requests);upstream({id:req.id,result:{turn:{id:"turn-one"}}});await new Promise(r=>setTimeout(r,1))};
  return {input,child,completion,messages,requests,wait,send,upstream,init,session,prompt,async close(){input.end();await completion}};
}
describe("local ACP App Server bridge",()=>{
  it("initializes, creates a local session, streams text and completes exactly one turn",async()=>{
    const f=fixture();try{await f.init();await f.session();await f.prompt();
      f.upstream({method:"item/agentMessage/delta",params:{threadId:"thread-one",turnId:"turn-one",itemId:"message",delta:"hello"}});
      f.upstream({method:"turn/completed",params:{threadId:"thread-one",turn:{id:"turn-one",status:"completed"}}});
      expect((await f.wait(m=>m.id===3)).result.stopReason).toBe("end_turn");
      expect(f.messages.find(m=>m.method==="session/update").params.update.content.text).toBe("hello");
      expect(f.requests.filter(m=>m.method==="turn/start")).toHaveLength(1);
    }finally{await f.close()}
  });
  it("binds permission to the exact owned session/turn and never auto-approves",async()=>{
    const f=fixture();try{await f.init();await f.session();await f.prompt();
      f.upstream({id:90,method:"item/commandExecution/requestApproval",params:{threadId:"foreign",turnId:"turn-one",itemId:"tool",command:"echo test"}});
      expect((await f.wait(m=>m.id===90,f.requests)).error).toBeDefined();
      f.upstream({id:91,method:"item/commandExecution/requestApproval",params:{threadId:"thread-one",turnId:"turn-one",itemId:"tool",command:"echo synthetic",availableDecisions:["accept","decline"]}});
      const permission=await f.wait(m=>m.method==="session/request_permission");
      expect(f.requests.some(m=>m.id===91)).toBe(false);
      expect(permission.params.toolCall.rawInput.command).toBe("echo synthetic");
      f.send({id:permission.id,result:{outcome:{outcome:"selected",optionId:"allow_once"}}});
      expect((await f.wait(m=>m.id===91,f.requests)).result.decision).toBe("accept");
    }finally{await f.close()}
  });
  it("cancels the active turn and refuses a late approval",async()=>{
    const f=fixture();try{await f.init();await f.session();await f.prompt();
      f.upstream({id:92,method:"item/fileChange/requestApproval",params:{threadId:"thread-one",turnId:"turn-one",itemId:"file"}});
      const permission=await f.wait(m=>m.method==="session/request_permission");
      f.send({method:"session/cancel",params:{sessionId:"thread-one"}});
      const cancel=await f.wait(m=>m.method==="turn/interrupt",f.requests);expect(cancel.params).toEqual({threadId:"thread-one",turnId:"turn-one"});
      f.upstream({id:cancel.id,result:{}});
      // Cancellation denies a pending request before the editor responds.
      expect((await f.wait(m=>m.id===92,f.requests)).result.decision).toBe("decline");
      f.send({id:permission.id,result:{outcome:{outcome:"selected",optionId:"allow_once"}}});
      expect((await f.wait(m=>m.id===92,f.requests)).result.decision).toBe("decline");
      f.upstream({method:"turn/completed",params:{threadId:"thread-one",turn:{id:"turn-one",status:"interrupted"}}});
      expect((await f.wait(m=>m.id===3)).result.stopReason).toBe("cancelled");
    }finally{await f.close()}
  });
  it("expires pending approvals when a turn ends, before another turn starts",async()=>{
    const f=fixture();try{await f.init();await f.session();await f.prompt();
      f.upstream({id:93,method:"item/fileChange/requestApproval",params:{threadId:"thread-one",turnId:"turn-one",itemId:"old-file"}});
      const permission=await f.wait(m=>m.method==="session/request_permission");
      f.upstream({method:"turn/completed",params:{threadId:"thread-one",turn:{id:"turn-one",status:"completed"}}});
      expect((await f.wait(m=>m.id===93,f.requests)).result.decision).toBe("decline");
      f.send({id:4,method:"session/prompt",params:{sessionId:"thread-one",prompt:[{type:"text",text:"next"}]}});
      const second=await f.wait(m=>m.method==="turn/start"&&m.params.input[0].text==="next",f.requests);
      f.upstream({id:second.id,result:{turn:{id:"turn-two"}}});
      f.send({id:permission.id,result:{outcome:{outcome:"selected",optionId:"allow_once"}}});
      await new Promise(r=>setTimeout(r,2));
      expect(f.requests.filter(m=>m.id===93)).toHaveLength(1);
    }finally{await f.close()}
  });
  it("rejects foreign sessions, unknown content and client-supplied MCP configuration",async()=>{
    const f=fixture();try{await f.init();
      f.send({id:10,method:"session/new",params:{cwd:process.cwd(),mcpServers:[{name:"untrusted"}]}});expect((await f.wait(m=>m.id===10)).error).toBeDefined();
      f.send({id:11,method:"session/prompt",params:{sessionId:"other",prompt:[{type:"text",text:"x"}]}});expect((await f.wait(m=>m.id===11)).error).toBeDefined();
      expect(f.requests.some(m=>m.method==="turn/start")).toBe(false);
      f.input.write(Buffer.alloc(1024*1024+1,120));expect(await f.completion).toBe(0);expect(f.child.exitCode).toBe(0);
    }finally{await f.close()}
  });
});
