import { readFile, lstat } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
export async function requireSigningInputs(environment = process.env) {
  const file=environment.TAURI_SIGNING_PRIVATE_KEY_PATH;
  if (!file || !environment.AGENTFORGE_DESKTOP_RELEASE_POLICY || !environment.AGENTFORGE_DESKTOP_PUBLISHER_ID) throw new Error('Publisher and updater signing inputs required');
  const meta=await lstat(file);
  if (!meta.isFile() || meta.isSymbolicLink() || meta.size < 32 || meta.size > 16384 || (process.platform!=='win32' && (meta.mode & 0o077))) throw new Error('Updater signing key file refused');
  const config=JSON.parse(await readFile(environment.AGENTFORGE_DESKTOP_RELEASE_POLICY,'utf8'));
  if(!config.enabled || !config.policy_public_key || !config.updater_public_key) throw new Error('Signed update policy must be enabled');
  return {publisher:environment.AGENTFORGE_DESKTOP_PUBLISHER_ID};
}
function run(command,args){const result=spawnSync(command,args,{encoding:'utf8',stdio:['ignore','pipe','pipe']});if(result.status!==0)throw new Error('Independent publisher verification failed');return result.stdout;}
export async function verifyPublisher(artifact,environment=process.env){
  const {publisher}=await requireSigningInputs(environment);
  const meta=await lstat(artifact);if(meta.isSymbolicLink())throw new Error('Artifact symlink refused');
  if(process.platform==='darwin'){
    run('codesign',['--verify','--deep','--strict','--verbose=2',artifact]);
    // codesign identity output is diagnostic text, not emitted into receipts.
    const details=spawnSync('codesign',['--display','--verbose=4',artifact],{encoding:'utf8'});
    if(details.status!==0 || !details.stderr.split('\n').includes(`TeamIdentifier=${publisher}`))throw new Error('Publisher team mismatch');
    run('spctl',['--assess','--type','execute','--verbose=2',artifact]);run('xcrun',['stapler','validate',artifact]);
  } else if(process.platform==='win32') {
    if(!/^[0-9A-Fa-f]{40}$/.test(publisher))throw new Error('Expected Authenticode certificate thumbprint required');
    // File paths travel as environment data, never interpolated PowerShell code.
    const result=spawnSync('powershell.exe',['-NoProfile','-NonInteractive','-Command',"$s=Get-AuthenticodeSignature -LiteralPath $env:AF_DESKTOP_ARTIFACT; if($s.Status -ne 'Valid' -or $s.SignerCertificate.Thumbprint -ne $env:AF_DESKTOP_SIGNER){exit 1}"],{env:{...environment,AF_DESKTOP_ARTIFACT:path.resolve(artifact),AF_DESKTOP_SIGNER:publisher},stdio:'pipe'});
    if(result.status!==0)throw new Error('Authenticode verification failed');
  } else if(process.platform==='linux') {
    const keyFile=environment.AGENTFORGE_DESKTOP_LINUX_PUBLISHER_PUBLIC_KEY_FILE, signatureFile=environment.AGENTFORGE_DESKTOP_LINUX_PUBLISHER_SIGNATURE_FILE;
    if(!keyFile || !signatureFile || !/^[a-f0-9]{64}$/.test(publisher))throw new Error('Pinned Linux publisher signature inputs required');
    const keyMeta=await lstat(keyFile),signatureMeta=await lstat(signatureFile);
    if(!keyMeta.isFile() || keyMeta.isSymbolicLink() || keyMeta.size>16384 || !signatureMeta.isFile() || signatureMeta.isSymbolicLink() || signatureMeta.size>16384)throw new Error('Linux publisher material refused');
    if(createHash('sha256').update(await readFile(keyFile)).digest('hex')!==publisher)throw new Error('Linux publisher key fingerprint mismatch');
    run('minisign',['-Vm',artifact,'-p',keyFile,'-x',signatureFile]);
  } else {throw new Error('Unsupported publisher platform');}
  return {publisher_signature:'verified',notarization:process.platform==='darwin'?'verified':'not_applicable',published:false};
}
if(process.argv[1] && path.resolve(process.argv[1])===fileURLToPath(import.meta.url)){
  try{if(process.argv.length!==3)throw new Error();console.log(JSON.stringify(await verifyPublisher(process.argv[2])));}
  catch{console.error('Publisher gate refused promotion. No signing, notarization or publication was performed.');process.exitCode=1;}
}
