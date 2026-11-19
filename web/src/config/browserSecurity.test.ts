import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../..", import.meta.url));
function generate(script: string, env: Record<string, string> = {}): string {
  const dir = mkdtempSync(join(tmpdir(), "agentforge-browser-security-"));
  const path = join(dir, "output");
  try {
    execFileSync("sh", [join(root, "docker-entrypoint.d", script)], {
      env: { PATH: process.env.PATH, RUNTIME_CONFIG_OUTPUT_PATH: path, FRONTEND_SECURITY_HEADERS_OUTPUT_PATH: path, ...env },
      stdio: "pipe",
    });
    return readFileSync(path, "utf8");
  } finally { rmSync(dir, { recursive: true, force: true }); }
}

test("public config is JSON, preserves arbitrary Unicode/control characters and cannot inject script", () => {
  const value = '支持@example.test\n\r\t\b\f\u0001"\\</script><script>alert(1)</script>\u2028';
  const config = JSON.parse(generate("40-runtime-config.sh", { VITE_SUPPORT_EMAIL: value, PRIVATE_KEY: "must-not-export" }));
  assert.equal(config.supportEmail, value);
  assert.equal(config.authEmailDomain, "example.com");
  assert.equal(config.agentStudioEnabled, "true");
  assert.equal(JSON.stringify(config).includes("must-not-export"), false);
  assert.deepEqual(JSON.parse(readFileSync(join(root, "public/runtime-config.json"), "utf8")), {});
  assert.doesNotMatch(readFileSync(join(root, "index.html"), "utf8"), /runtime-config\.js/);
});

test("strict CSP is report-only by default; explicit enforcement has no broad network allowlist", () => {
  const initial = generate("45-security-headers.sh");
  assert.match(initial, /Content-Security-Policy-Report-Only .*require-trusted-types-for 'script'; trusted-types dompurify/);
  assert.doesNotMatch(initial, /X-XSS-Protection|trusted-types default/);
  for (const header of ["Permissions-Policy", "Cross-Origin-Opener-Policy", "Cross-Origin-Resource-Policy"]) assert.match(initial, new RegExp(header));
  const enforced = generate("45-security-headers.sh", {
    FRONTEND_CSP_MODE: "enforce",
    FRONTEND_CONNECT_ORIGINS: "https://api.example.test:8443 wss://api.example.test http://[::1]:8080",
    FRONTEND_IMAGE_ORIGINS: "https://images.example.test",
  });
  assert.doesNotMatch(enforced, /Report-Only|https:;|wss:;|unsafe-eval|script-src[^;]*unsafe-inline/);
  assert.match(enforced, /connect-src 'self' https:\/\/api.example.test:8443 wss:\/\/api.example.test http:\/\/\[::1\]:8080;/);
  assert.match(enforced, /img-src 'self' data: blob: https:\/\/images.example.test;/);
});

test("invalid policy mode/origin and header/directive injection fail startup", () => {
  for (const env of [
    { FRONTEND_CSP_MODE: "off" },
    ...["https:", "*", "https://*.example.test", "https://host/path", "https://host; script-src *", 'https://host";\nadd_header X-Evil yes;', "https://host:65536", "https://host:0"].map(FRONTEND_CONNECT_ORIGINS => ({ FRONTEND_CONNECT_ORIGINS })),
    { FRONTEND_IMAGE_ORIGINS: "wss://host" },
  ]) assert.throws(() => generate("45-security-headers.sh", env));
});

test("both edge configurations share security headers while embeds retain Gateway framing authority", () => {
  const nginx = readFileSync(join(root, "nginx.conf"), "utf8");
  assert.equal(nginx, readFileSync(join(root, "../deploy/helm/ai-gateway/files/nginx.conf"), "utf8"));
  assert.doesNotMatch(nginx, /X-XSS-Protection|connect-src.*https:/);
  const embed = nginx.slice(nginx.indexOf("location /embed/agents/"), nginx.indexOf("# API proxy"));
  assert.doesNotMatch(embed, /frontend-security-headers|SAMEORIGIN|frame-ancestors/);
  assert.match(embed, /Referrer-Policy "no-referrer"/);
  assert.equal(spawnSync("sh", ["-n", join(root, "docker-entrypoint.d/45-security-headers.sh")]).status, 0);
});
