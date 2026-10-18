import { describe,it,expect } from "vitest";
// @ts-expect-error local release script is intentionally plain ESM
import { verifyOsSignature } from "./verify-os-signature.mjs";
import { createHash } from "node:crypto";
import { mkdtempSync,writeFileSync,rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
describe("release OS trust checks",()=>{
 it("requires the matching OS and a reviewed signer",()=>{
  expect(()=>verifyOsSignature({platform:"darwin",host:"linux",binary:"/binary"})).toThrow(/matching/);
  expect(()=>verifyOsSignature({platform:"darwin",host:"darwin",binary:"/binary",policy:{}})).toThrow(/policy/);
 });
 it("requires Apple chain, pinned team and Gatekeeper assessment",()=>{
  const calls:any[]=[]; const result=verifyOsSignature({platform:"darwin",host:"darwin",binary:"/path with spaces/bin",policy:{apple_team_id:"ABCDE12345"},run:(...args:any[])=>{calls.push(args);return ""}});
  expect(result.kind).toBe("apple-notarized"); expect(calls).toHaveLength(3);
  expect(calls[1][1]).toContain('anchor apple generic and certificate leaf[subject.OU] = "ABCDE12345"');
  expect(calls[2][0]).toBe("/usr/sbin/spctl");
  expect(()=>verifyOsSignature({platform:"darwin",host:"darwin",binary:"/binary",policy:{apple_team_id:"ABCDE12345"},run:()=>{throw Error("not notarized")}})).toThrow(/not notarized/);
 });
 it("keeps untrusted Windows paths out of script source",()=>{
  let call:any[]=[]; const path="C:\\a'; exit 0; 'b.exe";
  // isAbsolute follows the executing host; use a POSIX absolute fixture here.
  const binary="/"+path;
  expect(verifyOsSignature({platform:"win32",host:"win32",binary,policy:{authenticode_thumbprint:"A".repeat(40)},run:(...args:any[])=>{call=args;return "verified\r\n"}}).kind).toBe("authenticode");
  expect(call[1].join(" ")).not.toContain(path);expect(JSON.parse(call[2].input).path).toBe(binary);
  expect(()=>verifyOsSignature({platform:"win32",host:"win32",binary,policy:{authenticode_thumbprint:"A".repeat(40)},run:()=>"UnknownError"})).toThrow();
 });
 it("pins the offline Linux verifier and binds the signer and bundle",()=>{
  const dir=mkdtempSync(join(tmpdir(),"cli-cosign-"));
  try { const tool=join(dir,"cosign");writeFileSync(tool,"fixture verifier");let call:any[]=[];
   const policy={sigstore_identity:"https://github.com/example/repo/.github/workflows/release.yml@refs/tags/v1",sigstore_oidc_issuer:"https://token.actions.githubusercontent.com",cosign_path:tool,cosign_sha256:createHash("sha256").update("fixture verifier").digest("hex")};
   expect(verifyOsSignature({platform:"linux",host:"linux",binary:"/binary",bundle:"/bundle",policy,run:(...args:any[])=>{call=args;return ""}}).kind).toBe("sigstore-offline");
   expect(call[1]).toContain("--offline"); expect(call[1]).toContain(policy.sigstore_identity);expect(call[1].at(-1)).toBe("/binary");
   expect(()=>verifyOsSignature({platform:"linux",host:"linux",binary:"/binary",bundle:"/bundle",policy:{...policy,cosign_sha256:"0".repeat(64)},run:()=>""})).toThrow(/reviewed/);
  }finally{rmSync(dir,{recursive:true,force:true})}
 });
});
