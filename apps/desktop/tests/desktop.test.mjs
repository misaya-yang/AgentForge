import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, readFile, symlink, chmod } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { generateKeyPairSync, createPublicKey, verify } from 'node:crypto';
import { prepareAssets } from '../scripts/assets.mjs';
import { approvedURL, validatePolicy, signPolicy } from '../scripts/release.mjs';
import { requireSigningInputs } from '../scripts/publisher-gate.mjs';
const root = path.resolve(import.meta.dirname,'..');
const scratch = process.env.AGENTFORGE_DESKTOP_TEST_SCRATCH ?? tmpdir();
async function fixture() {
  const folder=await mkdtemp(path.join(scratch,'desktop-test-')), source=path.join(folder,'console');
  await mkdir(path.join(source,'assets'),{recursive:true});await writeFile(path.join(source,'index.html'),'<div id="root"></div>');await writeFile(path.join(source,'assets','console.js'),'console.log("same console build")');
  return {folder,source,output:path.join(folder,'staged')};
}
function releaseFixture(){
  const keys=generateKeyPairSync('ed25519'), now=1700000000;
  const config={enabled:true,channel:'stable',policy_url:'https://updates.example/policy',download_origin:'https://updates.example',policy_public_key:createPublicKey(keys.privateKey).export({format:'der',type:'spki'}).subarray(-32).toString('base64'),updater_public_key:'fixture-artifact-key'};
  const policy={channel:'stable',target:'darwin-aarch64',version:'1.2.0',sequence:2,issued_at:now,expires_at:now+3600,rollout_basis_points:100,manifest_url:'https://updates.example/manifest.json',artifact_size:10,manifest:{version:'1.2.0',platforms:{'darwin-aarch64':{url:'https://updates.example/app.tar.gz',signature:'fixture-artifact-signature'}}}};
  return {keys,now,config,policy};
}
test('copies the exact Console build with a separate privileged companion and byte receipt',async()=>{
  const f=await fixture(),receipt=await prepareAssets(f.source,f.output);
  assert.equal(await readFile(path.join(f.output,'assets','console.js'),'utf8'),await readFile(path.join(f.source,'assets','console.js'),'utf8'));
  assert.deepEqual(receipt.native_ipc_windows,['companion']);assert.equal(receipt.acceptance,'not_run');
  assert.match(await readFile(path.join(f.output,'native','index.html'),'utf8'),/AgentForge Companion/);
  await assert.rejects(prepareAssets(f.source,f.output));
});
test('rejects symlink input and reserved privilege paths before creating output',async()=>{
  const f=await fixture();await symlink(path.join(f.source,'index.html'),path.join(f.source,'link.html'));
  await assert.rejects(prepareAssets(f.source,f.output),/Symlink/);
  const g=await fixture();await mkdir(path.join(g.source,'native'));await writeFile(path.join(g.source,'native','inject.js'),'bad');await assert.rejects(prepareAssets(g.source,g.output),/Reserved/);
});
test('rejects overlapping staging paths',async()=>{const f=await fixture();await assert.rejects(prepareAssets(f.source,path.join(f.source,'dist')),/overlap/);});
test('only bundled companion has native ACL and all native commands have explicit guards',async()=>{
  const capability=JSON.parse(await readFile(path.join(root,'src-tauri/capabilities/companion.json'),'utf8'));
  assert.deepEqual(capability.windows,['companion']);assert.equal(capability.local,true);assert.equal(capability.remote,undefined);
  assert.deepEqual(capability.permissions,['companion-operations']);
  const native=await readFile(path.join(root,'src-tauri/src/main.rs'),'utf8');
  assert.equal((native.match(/#\[tauri::command\]/g)??[]).length,11);
  assert.equal((native.match(/guard\(&window\)\?/g)??[]).length,11);
  const permission=await readFile(path.join(root,'src-tauri/permissions/companion.toml'),'utf8');assert.doesNotMatch(permission,/http:|shell:|fs:|opener:|remote:/);
  assert.match(native,/on_navigation/);assert.match(native,/WebviewUrl::External/);
});
test('companion has no credential, HTML injection or browser storage sink',async()=>{
  const js=await readFile(path.join(root,'native/companion.js'),'utf8');
  assert.doesNotMatch(js,/innerHTML|outerHTML|insertAdjacentHTML|localStorage|sessionStorage|access_token|refresh_token|document\.cookie|eval\(/);
  const config=JSON.parse(await readFile(path.join(root,'src-tauri/tauri.conf.json'),'utf8'));
  assert.match(config.app.security.csp,/require-trusted-types-for 'script'/);assert.match(config.app.security.csp,/frame-src 'none'/);assert.equal(config.bundle.active,false);
  assert.equal(JSON.parse(await readFile(path.join(root,'src-tauri/release-policy.json'),'utf8')).enabled,false);
});
test('staged signed policy refuses scope, time, budget and artifact-signature violations',()=>{
  const {config,policy,now}=releaseFixture();assert.equal(validatePolicy(policy,config,now),policy);
  for(const patch of [{channel:'beta'},{target:'windows-arm64'},{expires_at:now},{issued_at:now+600},{rollout_basis_points:10001},{rollout_basis_points:-1},{sequence:0},{artifact_size:600*1024*1024},{version:'1.2.0-beta'},{manifest_url:'https://evil.example/manifest'}]) assert.throws(()=>validatePolicy({...policy,...patch},config,now));
  const changed=structuredClone(policy);changed.manifest.platforms['darwin-aarch64'].signature='';assert.throws(()=>validatePolicy(changed,config,now));
  assert.throws(()=>validatePolicy(policy,{...config,enabled:false},now));
  for(const url of ['http://updates.example/a','https://updates.example.evil/a','https://u@updates.example/a','https://updates.example/a?q=x','https://updates.example/a#x'])assert.throws(()=>approvedURL(url,config.download_origin));
});
test('signs exact policy bytes with a matching private key and independently verifies Ed25519',async()=>{
  const {folder}=await fixture(),{keys,config,policy,now}=releaseFixture(),keyFile=path.join(folder,'policy.pem'),output=path.join(folder,'envelope.json');
  await writeFile(keyFile,keys.privateKey.export({format:'pem',type:'pkcs8'}),{mode:0o600});
  const receipt=await signPolicy(policy,config,keyFile,output,now);assert.equal(receipt.published,false);
  const envelope=JSON.parse(await readFile(output,'utf8')),payload=Buffer.from(envelope.payload,'base64');
  assert.equal(verify(null,payload,keys.publicKey,Buffer.from(envelope.signature,'base64')),true);
  assert.deepEqual(JSON.parse(payload),policy);
  assert.equal(verify(null,Buffer.from(payload.toString().replace('1.2.0','9.9.9')),keys.publicKey,Buffer.from(envelope.signature,'base64')),false);
  await assert.rejects(signPolicy(policy,config,keyFile,output,now));
  await assert.rejects(signPolicy(policy,{...config,policy_public_key:Buffer.alloc(32).toString('base64')},keyFile,path.join(folder,'wrong.json'),now),/does not match/);
  if(process.platform!=='win32'){await chmod(keyFile,0o644);await assert.rejects(signPolicy(policy,config,keyFile,path.join(folder,'unsafe.json'),now),/private regular/);}
});
test('publisher signing fails closed without actual signing inputs',async()=>{await assert.rejects(requireSigningInputs({}),/required/);});
test('signed candidate assembly cannot turn an unsigned artifact into a release receipt',async()=>{
  const {assemble}=await import('../scripts/assemble.mjs');
  await assert.rejects(assemble('/missing/app','/missing/sig','/missing/out','/missing/scratch',{}),/required/);
});
test('companion renders hostile verification metadata as text and sends no hidden device code',async()=>{
  const {runInNewContext}=await import('node:vm');
  const elements=new Map();const get=id=>{if(!elements.has(id))elements.set(id,{textContent:'',disabled:false,value:''});return elements.get(id);};
  const commands=[];let injected=false;
  const fake={document:{getElementById:get},window:{__TAURI__:{core:{invoke:async(command,args)=>{commands.push({command,args});return command==='begin_login'?{verification_uri:'https://gateway.test/<script>injected=true</script>',user_code:'<img onerror="injected=true">',expires_in:60}:{gateway:'https://gateway.test'};}}}},JSON};
  const js=await readFile(path.join(root,'native/companion.js'),'utf8');runInNewContext(js,fake);await new Promise(setImmediate);
  await get('login').onclick();assert.match(get('challenge').textContent,/<script>/);assert.equal(injected,false);
  assert.equal(commands.at(-1).command,'begin_login');assert.deepEqual(Object.keys(commands.at(-1).args),[]);
  assert.equal(get('finish').disabled,false);
});
test('macOS archive extraction permits one app and denies traversal, links and reused output',async()=>{
  const {spawnSync}=await import('node:child_process');const {folder}=await fixture();
  const python=String.raw`import importlib.util,io,pathlib,sys,tarfile
spec=importlib.util.spec_from_file_location('extract_app',sys.argv[1]); module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
root=pathlib.Path(sys.argv[2])
def archive(name,entries):
 p=root/name
 with tarfile.open(p,'w:gz') as f:
  for name,data,kind in entries:
   item=tarfile.TarInfo(name);item.size=len(data)
   if kind=='link':item.type=tarfile.SYMTYPE;item.linkname='/tmp/escape'
   f.addfile(item,io.BytesIO(data))
 return p
good=archive('good.tar.gz',[('AgentForge.app/Contents/Info.plist',b'fixture','file')]);out=root/'app';assert module.extract_app(good,out).name=='AgentForge.app'
for name,entries in [('escape',[('../escape',b'x','file')]),('link',[('AgentForge.app/Contents/escape',b'','link')]),('many',[('One.app/Contents/Info.plist',b'x','file'),('Two.app/Contents/Info.plist',b'x','file')])]:
 try:module.extract_app(archive(name+'.tar.gz',entries),root/name);raise AssertionError('unsafe accepted')
 except ValueError:pass
try:module.extract_app(good,out);raise AssertionError('reuse accepted')
except FileExistsError:pass
`;
  const result=spawnSync('python3',['-B','-c',python,path.join(root,'scripts/extract-app.py'),folder],{encoding:'utf8'});assert.equal(result.status,0,result.stderr);
});
