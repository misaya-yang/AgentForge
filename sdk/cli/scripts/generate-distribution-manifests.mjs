// Generate reviewable Homebrew / winget metadata from already signed release assets.
// Does not fetch assets, create installers, sign, publish, or overwrite existing files.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
function asset(value){
  const url=new URL(value?.url);
  if(url.protocol!=="https:"||url.username||url.password||url.search||url.hash||!/^[a-f0-9]{64}$/.test(value.sha256))throw new Error("Expected immutable HTTPS release asset and SHA-256");
  return {url:url.href,sha256:value.sha256};
}
export function distributionManifests(input){
  const version=input.version;
  if(!/^\d+\.\d+\.\d+$/.test(version??""))throw new Error("Stable semantic version required");
  const cli=asset(input.cli_tarball),targets={};
  for(const target of ["darwin-x64","darwin-arm64","linux-x64","linux-arm64"])targets[target]=asset(input.native_archives?.[target]);
  const windows=asset(input.windows_msi);
  const ruby=value=>JSON.stringify(value); // JSON strings are safe Ruby literals for these URL/hash/version values.
  const formula=`# Generated candidate; review and install-test before publishing.\nclass AgentforgeCli < Formula\n  desc "AgentForge local agent CLI"\n  homepage "https://github.com/misaya-yang/AgentForge"\n  url ${ruby(cli.url)}\n  sha256 ${ruby(cli.sha256)}\n  version ${ruby(version)}\n  license "Apache-2.0"\n  depends_on "node@24"\n\n${Object.entries(targets).map(([target,a])=>`  resource ${ruby(target)} do\n    url ${ruby(a.url)}\n    sha256 ${ruby(a.sha256)}\n  end`).join("\n")}\n\n  def install\n    system Formula["node@24"].opt_bin/"npm", "install", "--global", "--prefix", libexec, "--omit=optional", "--ignore-scripts", "--offline", buildpath\n    target = "#{OS.mac? ? 'darwin' : 'linux'}-#{Hardware::CPU.arm? ? 'arm64' : 'x64'}"\n    resource(target).stage do\n      (libexec/"lib/node_modules/ai-gateway-cli/vendor"/target).install Dir["*"]\n    end\n    (bin/"ai-gateway").write_env_script libexec/"bin/ai-gateway", PATH: "#{Formula['node@24'].opt_bin}:#{ENV.fetch('PATH')}"\n  end\n\n  test do\n    assert_match "Usage: ai-gateway", shell_output("#{bin}/ai-gateway --help")\n    report = JSON.parse(IO.popen([bin/"ai-gateway", "doctor", "--json"], &:read))\n    assert_equal "pass", report.fetch("checks").find { |check| check["id"] == "native_artifact" }.fetch("status")\n  end\nend\n`;
  // MSI is an external signed asset; no guessed product code or forged installer is generated.
  const winget=`# Generated candidate; validate the real signed MSI before submitting.\nPackageIdentifier: AgentForge.CLI\nPackageVersion: ${version}\nInstallerType: msi\nScope: user\nInstallers:\n  - Architecture: x64\n    InstallerUrl: ${JSON.stringify(windows.url)}\n    InstallerSha256: ${windows.sha256.toUpperCase()}\nDependencies:\n  PackageDependencies:\n    - PackageIdentifier: OpenJS.NodeJS.LTS\nManifestType: installer\nManifestVersion: 1.9.0\n`;
  return {formula,winget};
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)){
 try{
  if(process.argv.length!==4)throw Error("Expected metadata JSON and new output directory");
  const result=distributionManifests(JSON.parse(readFileSync(process.argv[2],"utf8")));
  const output=resolve(process.argv[3]);mkdirSync(output,{recursive:false});
  writeFileSync(resolve(output,"agentforge-cli.rb"),result.formula,{flag:"wx"});
  writeFileSync(resolve(output,"AgentForge.CLI.installer.yaml"),result.winget,{flag:"wx"});
  console.log(JSON.stringify({generated:["agentforge-cli.rb","AgentForge.CLI.installer.yaml"]}));
 }catch{console.error("Distribution metadata generation failed; review immutable signed asset inputs and use a new output directory.");process.exitCode=1;}
}
