import {test} from 'node:test';import assert from 'node:assert/strict';
import {mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';import {tmpdir} from 'node:os';import {join} from 'node:path';
import {generateKeyPairSync,sign,verify} from 'node:crypto';import {packageBytes,stagePackage} from './package.mjs';
test('reproducible bounded package contains declarative assets and no native source/WASM',()=>{
 const bytes=packageBytes();assert.deepEqual(bytes,packageBytes());assert.equal(bytes.length%512,0);let at=0;const names=[];
 while(bytes[at]){const h=bytes.subarray(at,at+512);const name=h.subarray(0,100).toString().replace(/\0.*$/s,'');names.push(name);const n=parseInt(h.subarray(124,136).toString().replace(/\0.*$/s,''),8);assert.equal(h.subarray(257,263).toString(),'ustar\0');assert.equal(h[156],48);at+=512+Math.ceil(n/512)*512;}
 assert(names.includes('plugin.json'));assert(names.includes('skills/quiz-generation/SKILL.md'));assert(!names.some(n=>/\.(rs|wasm|exe)$/.test(n)));assert(bytes.subarray(at).every(b=>b===0));
});
test('staging cannot overwrite an existing release candidate',()=>{
 const dir=mkdtempSync(join(tmpdir(),'quiz-pack-'));try{const path=join(dir,'quiz.tar');const first=stagePackage(path);assert.equal(first.status,'unsigned_candidate_only');assert.throws(()=>stagePackage(path));assert.deepEqual(readFileSync(path),packageBytes())}finally{rmSync(dir,{recursive:true,force:true})}
});
test('minisign raw Ed fixture cryptography binds actual bytes, key ID and trusted comment',()=>{
 const {privateKey,publicKey}=generateKeyPairSync('ed25519'),bytes=packageBytes(),signature=sign(null,bytes,privateKey),id=Buffer.alloc(8,7);const raw=Buffer.concat([Buffer.from('Ed'),id,signature]);
 const comment='timestamp:1700000000 file:ai-quiz.tar',global=sign(null,Buffer.concat([signature,Buffer.from(comment)]),privateKey);
 assert.equal(raw.length,74);assert(verify(null,bytes,publicKey,raw.subarray(10)));assert(verify(null,Buffer.concat([signature,Buffer.from(comment)]),publicKey,global));
 assert(!verify(null,Buffer.concat([bytes,Buffer.from('tampered')]),publicKey,raw.subarray(10)));assert(!verify(null,Buffer.concat([signature,Buffer.from(comment+' changed')]),publicKey,global));
 const wrong=generateKeyPairSync('ed25519').publicKey;assert(!verify(null,bytes,wrong,raw.subarray(10)));
});
test('plugin-owned generation descriptor retains the reviewed public schema hash',()=>{
 const d=JSON.parse(readFileSync(new URL('./native/capability.json',import.meta.url),'utf8'));
 const canonical=v=>v&&typeof v==='object'?(Array.isArray(v)?`[${v.map(canonical).join(',')}]`:`{${Object.keys(v).sort().map(k=>JSON.stringify(k)+':'+canonical(v[k])).join(',')}}`):JSON.stringify(v);
 return import('node:crypto').then(({createHash})=>{assert.equal(d.id,'generate_quiz');assert.equal('sha256:'+createHash('sha256').update(canonical(d.input_schema)).digest('hex'),d.schema_hash);assert.equal(d.schema_hash,'sha256:9b4ece973284e980c86d013ca5a3e62f3c65765f57f96846374a95a3015a3666')});
});
