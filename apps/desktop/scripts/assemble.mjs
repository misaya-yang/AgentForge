/* Package already signed bytes only. This command neither builds nor signs nor
 * publishes; missing publisher/updater verification closes assembly. */
import { lstat, readFile, mkdir, copyFile, writeFile, mkdtemp } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { verifyPublisher, requireSigningInputs } from './publisher-gate.mjs';
export async function assemble(artifact, artifactSignatureFile, newOutput, scratch, environment=process.env) {
  await requireSigningInputs(environment);
  const metadata=await lstat(artifact), signatureMeta=await lstat(artifactSignatureFile);
  if(!metadata.isFile() || metadata.isSymbolicLink() || metadata.size<=0 || metadata.size>512*1024*1024 || !signatureMeta.isFile() || signatureMeta.isSymbolicLink() || signatureMeta.size>16384) throw new Error('Signed artifact or signature file refused');
  const verifyDirectory=await mkdtemp(path.join(scratch,'desktop-signature-'));
  const staged=path.join(verifyDirectory,path.basename(artifact));
  const bytes=await readFile(artifact),sha256=createHash('sha256').update(bytes).digest('hex');
  await writeFile(staged,bytes,{flag:'wx',mode:0o700});
  let publisherArtifact=staged;
  if(process.platform==='darwin') {
    const extraction=spawnSync('python3',[path.join(path.dirname(fileURLToPath(import.meta.url)),'extract-app.py'),staged,path.join(verifyDirectory,'app')],{encoding:'utf8',stdio:['ignore','pipe','pipe']});
    if(extraction.status!==0)throw new Error('Safe macOS updater application extraction failed');
    publisherArtifact=JSON.parse(extraction.stdout).app;
  }
  // No caller-supplied receipt can stand in for independent OS verification.
  const publisher=await verifyPublisher(publisherArtifact,environment);
  const config=JSON.parse(await readFile(environment.AGENTFORGE_DESKTOP_RELEASE_POLICY,'utf8'));
  const key=path.join(verifyDirectory,'updater.pub'),signature=path.join(verifyDirectory,'artifact.minisig');
  await writeFile(key,Buffer.from(config.updater_public_key,'base64'),{flag:'wx'});
  await writeFile(signature,Buffer.from((await readFile(artifactSignatureFile,'utf8')).trim(),'base64'),{flag:'wx'});
  const verifier=spawnSync('minisign',['-Vm',staged,'-p',key,'-x',signature],{stdio:'pipe'});
  if(verifier.status!==0)throw new Error('Independent official-updater signature verification failed');
  if(createHash('sha256').update(await readFile(staged)).digest('hex')!==sha256)throw new Error('Verified artifact changed');
  await mkdir(newOutput,{recursive:false});
  const destination=path.join(newOutput,path.basename(artifact));await copyFile(staged,destination,1);
  if(createHash('sha256').update(await readFile(destination)).digest('hex')!==sha256)throw new Error('Artifact changed during assembly');
  const receipt={schema:1,artifact:path.basename(artifact),sha256,bytes:bytes.length,...publisher,updater_signature:'verified',ga_eligible:false,parity_acceptance:'not_run',published:false};
  await writeFile(path.join(newOutput,'candidate-receipt.json'),JSON.stringify(receipt,null,2)+'\n',{flag:'wx'});return receipt;
}
if(process.argv[1] && path.resolve(process.argv[1])===fileURLToPath(import.meta.url)){
  try{if(process.argv.length!==6)throw new Error();console.log(JSON.stringify(await assemble(...process.argv.slice(2))));}
  catch{console.error('Signed candidate assembly refused. No build, signing or publication was performed.');process.exitCode=1;}
}
