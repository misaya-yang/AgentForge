#!/usr/bin/env node
// Reproducible local USTAR only. No signing key, installer, network or activation.
import {createHash} from 'node:crypto';
import {existsSync,lstatSync,readFileSync,realpathSync,writeFileSync} from 'node:fs';
import {dirname,isAbsolute,join,relative,resolve,sep} from 'node:path';
import {fileURLToPath} from 'node:url';
const here=dirname(fileURLToPath(import.meta.url));
function octal(n,width){const value=n.toString(8);if(value.length>width-1)throw Error('USTAR integer overflow');return Buffer.from(value.padStart(width-1,'0')+'\0')}
export function packageBytes(root=here){
 const manifest=JSON.parse(readFileSync(join(root,'plugin.json'),'utf8'));
 if(manifest.name!=='ai-quiz'||manifest.version!=='1.1.0'||manifest.extensions?.['com.misaya.ai-gateway']?.publisher_id!=='agentforge-maintainers'||JSON.stringify(manifest.extensions?.['com.misaya.ai-gateway']?.worker_capabilities)!=='["generate_quiz"]')throw Error('Unexpected plugin identity');
 const names=['plugin.json','agents/quiz-expert.md','skills/quiz-generation/SKILL.md'];
 for(const name of ['README.md','LICENSE'])if(existsSync(join(root,name)))names.push(name);
 const chunks=[];
 for(const name of names.sort()){
  const path=join(root,name),stat=lstatSync(path);if(!stat.isFile()||stat.isSymbolicLink()||realpathSync(path)!==resolve(path)||stat.size>1048576)throw Error('Unsafe package asset');
  const bytes=readFileSync(path);new TextDecoder('utf-8',{fatal:true}).decode(bytes);
  const header=Buffer.alloc(512);header.write(name,0,100,'ascii');octal(0o600,8).copy(header,100);octal(0,8).copy(header,108);octal(0,8).copy(header,116);octal(bytes.length,12).copy(header,124);octal(0,12).copy(header,136);header.fill(32,148,156);header[156]=48;header.write('ustar\0',257,'ascii');header.write('00',263,'ascii');
  const sum=header.reduce((n,b)=>n+b,0);header.write(sum.toString(8).padStart(6,'0')+'\0 ',148,8,'ascii');
  chunks.push(header,bytes,Buffer.alloc((512-bytes.length%512)%512));
 }
 chunks.push(Buffer.alloc(1024));const bytes=Buffer.concat(chunks);if(bytes.length>8388608)throw Error('Package exceeds limit');return bytes;
}
export function stagePackage(output){
 const parent=realpathSync(dirname(resolve(output))),target=join(parent,resolve(output).split(sep).at(-1));
 const repo=realpathSync(resolve(here,'../..')),inside=relative(repo,target);
 if(!inside.startsWith('..'+sep)&&inside!=='..'&&!isAbsolute(inside))throw Error('Output must be external to repository');
 const bytes=packageBytes();writeFileSync(target,bytes,{flag:'wx',mode:0o600});
 return {id:'ai-quiz',version:'1.1.0',artifact_sha256:'sha256:'+createHash('sha256').update(bytes).digest('hex'),bytes:bytes.length,status:'unsigned_candidate_only'};
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)){
 try{if(process.argv.length!==3)throw Error();console.log(JSON.stringify(stagePackage(process.argv[2])))}catch{console.error('Plugin source packaging failed; use a new external output file and reviewed source assets.');process.exitCode=1}
}
