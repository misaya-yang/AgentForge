import { describe,it,expect } from "vitest";
// @ts-expect-error plain ESM release tooling
import { distributionManifests } from "./generate-distribution-manifests.mjs";
const asset=(name:string)=>({url:`https://releases.example/v1.5.0/${name}`,sha256:"a".repeat(64)});
const metadata=()=>({version:"1.5.0",cli_tarball:asset("main.tgz"),native_archives:Object.fromEntries(["darwin-x64","darwin-arm64","linux-x64","linux-arm64"].map(t=>[t,asset(t+".tgz")])),windows_msi:asset("agentforge-x64.msi")});
describe("distribution manifest source",()=>{
 it("generates four Homebrew targets and one signed-asset Windows installer reference",()=>{
  const result=distributionManifests(metadata());expect(result.formula).toContain('depends_on "node@24"');expect(result.formula).toContain('"--offline"');
  for(const t of ["darwin-x64","darwin-arm64","linux-x64","linux-arm64"])expect(result.formula).toContain(`resource "${t}"`);
  expect(result.winget).toContain("InstallerType: msi");expect(result.winget).toContain("InstallerSha256: "+"A".repeat(64));
 });
 it("rejects mutable or unsafe metadata before generating files",()=>{
  for(const url of ["http://example/file","https://user:secret@example/file","https://example/file?token=secret"]){const value=metadata();value.cli_tarball.url=url;expect(()=>distributionManifests(value)).toThrow()}
  const value=metadata();value.version='1.5.0\nmalicious';expect(()=>distributionManifests(value)).toThrow();
  const missing=metadata();delete (missing.native_archives as any)["darwin-arm64"];expect(()=>distributionManifests(missing)).toThrow();
 });
});
