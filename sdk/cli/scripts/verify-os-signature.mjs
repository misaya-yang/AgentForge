// Release-host verification only. No downloads, signing, publication or mutable trust.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { lstatSync, readFileSync } from "node:fs";
import { isAbsolute } from "node:path";
function bounded(value, pattern) { if(typeof value!=="string" || !pattern.test(value)) throw new Error("Missing or invalid reviewed OS signing policy"); return value; }
export function verifyOsSignature({platform,binary,policy,bundle,host=process.platform,run=execFileSync}) {
  if(platform!==host || !isAbsolute(binary)) throw new Error("Verify OS signature on its matching release host with an absolute binary path");
  const call=(tool,args,extra={})=>String(run(tool,args,{encoding:"utf8",timeout:30000,maxBuffer:262144,windowsHide:true,...extra}));
  if(platform==="darwin") {
    const team=bounded(policy?.apple_team_id,/^[A-Z0-9]{10}$/);
    call("/usr/bin/codesign",["--verify","--strict","--verbose=2",binary]);
    // Explicit designated requirement checks the pinned team as well as Apple's chain.
    call("/usr/bin/codesign",["--verify","--strict","-R",`anchor apple generic and certificate leaf[subject.OU] = "${team}"`,binary]);
    call("/usr/sbin/spctl",["--assess","--type","execute","--verbose=2",binary]);
    return {kind:"apple-notarized",team_id:team};
  }
  if(platform==="win32") {
    const thumbprint=bounded(policy?.authenticode_thumbprint,/^[A-F0-9]{40,64}$/);
    // File paths arrive on stdin, never interpolated into PowerShell source.
    const script='$ErrorActionPreference="Stop"; $i=([Console]::In.ReadToEnd()|ConvertFrom-Json); $s=Get-AuthenticodeSignature -LiteralPath $i.path; if($s.Status -ne "Valid" -or $s.SignerCertificate.Thumbprint -ne $i.thumbprint){exit 1}; Write-Output "verified"';
    const result=call("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",["-NoLogo","-NoProfile","-NonInteractive","-Command",script],{input:JSON.stringify({path:binary,thumbprint})});
    if(result.trim()!=="verified") throw new Error("Authenticode verification did not confirm the pinned signer");
    return {kind:"authenticode",certificate_thumbprint:thumbprint};
  }
  if(platform==="linux") {
    const identity=bounded(policy?.sigstore_identity,/^[^\r\n]{1,2048}$/);
    const issuer=bounded(policy?.sigstore_oidc_issuer,/^https:\/\/[^\s]{1,2048}$/);
    const tool=bounded(policy?.cosign_path,/^[^\r\n]{1,4096}$/);
    const expected=bounded(policy?.cosign_sha256,/^[a-f0-9]{64}$/);
    if(!isAbsolute(tool))throw new Error("Cosign tool path must be absolute");
    const stat=lstatSync(tool);
    if(!stat.isFile()||stat.isSymbolicLink()||createHash("sha256").update(readFileSync(tool)).digest("hex")!==expected)throw new Error("Cosign tool does not match the reviewed release policy");
    if(!isAbsolute(bundle??""))throw new Error("An offline Sigstore bundle is required for each Linux binary");
    call(tool,["verify-blob","--offline","--bundle",bundle,"--certificate-identity",identity,"--certificate-oidc-issuer",issuer,binary]);
    return {kind:"sigstore-offline",identity,issuer};
  }
  throw new Error("Unsupported OS signing target");
}
