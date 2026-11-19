#!/usr/bin/env node
// Offline candidate staging only. Never downloads, publishes, or changes vendor.
import { createHash, createPrivateKey, createPublicKey, sign } from "node:crypto";
import { copyFileSync, existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { RELEASE_TARGETS, canonical, verifyReleaseSignature } from "../src/native/release_signature.mjs";
import { verifyBinaryHeader } from "./verify-native-package.mjs";
import { verifyOsSignature } from "./verify-os-signature.mjs";
const root=resolve(dirname(fileURLToPath(import.meta.url)),"../../..");
const hash=bytes=>createHash("sha256").update(bytes).digest("hex");
function json(path){return JSON.parse(readFileSync(path,"utf8"))}
function regular(path){const m=lstatSync(path);if(!m.isFile()||m.isSymbolicLink())throw new Error("Expected regular input file");return readFileSync(path)}
export function stageRelease(options){
  const {target,binary,platformctl,materials,output,signingKeyFile,keyId,trustedKeysFile,osSigningPolicyFile,binaryBundle,platformctlBundle}=options;
  if(!RELEASE_TARGETS.includes(target)||!/^[A-Za-z0-9_.-]{1,64}$/.test(keyId??""))throw new Error("Invalid target/key ID");
  const parent=realpathSync(dirname(resolve(output)));
  const repo=realpathSync(root),destination=join(parent,resolve(output).split(sep).at(-1));
  const within=relative(repo,destination);
  if(!within.startsWith(`..${sep}`)&&within!==".."&&!isAbsolute(within))throw new Error("Output must be external to the repository");
  if(existsSync(destination))throw new Error("Output already exists; staging never deletes files");
  const pins=json(trustedKeysFile??join(root,"sdk/cli/release/trusted-keys.json"));
  const keyStat=lstatSync(signingKeyFile);
  if(keyStat.size>16384 || (process.platform!=="win32" && (keyStat.mode&0o077)!==0))throw new Error("Signing key file must be small and private");
  const key=createPrivateKey(regular(signingKeyFile));
  if(key.asymmetricKeyType!=="ed25519" || createPublicKey(key).export({type:"spki",format:"pem"}).toString()!==pins[keyId])throw new Error("Signing key is not pinned");
  const lock=json(join(root,"deploy/agent-runtime-source/lock.json"));
  const sourceReceipt=json(join(root,"deploy/agent-runtime-source/source-receipt.json"));
  const overlay=json(join(root,"rust/agent-runtime-overlay/manifest.json"));
  if(sourceReceipt.source?.upstream_sha!==lock.source?.upstream_sha || overlay.upstream_sha!==lock.source?.upstream_sha || overlay.sha256!==lock.build?.overlay_sha256 || sourceReceipt.overlay?.sha256!==overlay.sha256)throw new Error("Native source identity mismatch");
  const version=json(join(root,"sdk/cli/package.json")).version;
  const identity={schema_version:"ai-gateway-cli/native-source/v1",upstream_sha:lock.source.upstream_sha,overlay_sha256:overlay.sha256,release_version:version};
  const files={LICENSE:lock.license.upstream_license_sha256,NOTICE:lock.license.upstream_notice_sha256,"NOTICE.ai-platform.md":lock.license.notice_sha256,"sbom.cdx.json":lock.build.sbom_sha256,"source-receipt.json":lock.build.source_receipt_sha256};
  const [platform,arch]=target.split("-");
  const bytes=[regular(binary),regular(platformctl)];bytes.forEach(b=>verifyBinaryHeader(b,platform,arch));
  const osPolicy=json(osSigningPolicyFile);
  const osSignatures=[binary,platformctl].map((path,index)=>verifyOsSignature({platform,binary:resolve(path),policy:osPolicy,bundle:index===0?binaryBundle:platformctlBundle}));
  // Verify the exact bytes that were checked, detecting input replacement during verification.
  if([binary,platformctl].some((path,index)=>hash(regular(path))!==hash(bytes[index])))throw new Error("Binary changed during OS signature verification");
  for(const [name,digest] of Object.entries(files))if(hash(regular(join(materials,name)))!==digest)throw new Error(`Locked material mismatch: ${name}`);
  // Finish every validation before creating the new, non-overwriting directory.
  mkdirSync(destination,{mode:0o755});
  for(const name of Object.keys(files))copyFileSync(join(materials,name),join(destination,name));
  writeFileSync(join(destination,"source.json"),JSON.stringify(identity,null,2)+"\n");
  for(const [index,stem] of ["codex","platformctl"].entries()){
    const name=stem+(platform==="win32"?".exe":"");
    writeFileSync(join(destination,name),bytes[index],{mode:0o755,flag:"wx"});
    const receipt={release_version:version,schema_version:"ai-gateway-cli/native-artifact/v1",upstream_sha:identity.upstream_sha,overlay_sha256:identity.overlay_sha256,target_system:platform==="win32"?"windows":platform,target_arch:arch,binary:name,sha256:hash(bytes[index]),os_signature:osSignatures[index],materials:files};
    const signed={...receipt,signature:{algorithm:"Ed25519",key_id:keyId,value:sign(null,Buffer.from(canonical(receipt)),key).toString("base64url")}};
    verifyReleaseSignature(signed,pins);
    writeFileSync(join(destination,stem==="codex"?"artifact.json":"platformctl-artifact.json"),JSON.stringify(signed,null,2)+"\n",{flag:"wx"});
  }
  writeFileSync(join(destination,"package.json"),JSON.stringify({name:`@agentforge/cli-${target}`,version,license:"Apache-2.0",os:[platform],cpu:[arch],files:["codex*","platformctl*","artifact.json","source.json",...Object.keys(files)]},null,2)+"\n");
  return {target,package:`@agentforge/cli-${target}`,source:identity};
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)){
  try{
    const args=process.argv.slice(2),options={};
    const names={"--target":"target","--binary":"binary","--platformctl":"platformctl","--materials":"materials","--output":"output","--signing-key-file":"signingKeyFile","--key-id":"keyId","--trusted-keys-file":"trustedKeysFile","--os-signing-policy-file":"osSigningPolicyFile","--binary-bundle":"binaryBundle","--platformctl-bundle":"platformctlBundle"};
    for(let i=0;i<args.length;i+=2){const key=names[args[i]];if(!key||!args[i+1]||options[key])throw new Error("Invalid staging arguments");options[key]=args[i+1]}
    for(const key of ["target","binary","platformctl","materials","output","signingKeyFile","keyId","osSigningPolicyFile"])if(!options[key])throw new Error(`Missing staging input: ${key}`);
    console.log(JSON.stringify(stageRelease(options)));
  }catch{console.error("Native candidate staging failed; verify external inputs, pinned signing key and locked source/material identities.");process.exitCode=1;}
}
