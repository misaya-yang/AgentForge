/** Process-level tests of the real built CLI entry, using only isolated fixtures. */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, symlinkSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';

const built = resolve(process.env.CLI_DOCTOR_ENTRY ?? 'dist/cli.js');
const secret = 'DOCTOR_SECRET_CANARY_7c1e';
const identity = { schema_version: 'ai-gateway-cli/native-source/v1', upstream_sha: '1'.repeat(40), overlay_sha256: '2'.repeat(64) };
function fixture(wire = 'responses') {
  const root = mkdtempSync(join(tmpdir(), 'cli-doctor-process-'));
  const pkg = join(root, 'package');
  mkdirSync(join(pkg, 'dist'), { recursive: true });
  mkdirSync(join(pkg, 'vendor/linux-x64'), { recursive: true });
  writeFileSync(join(pkg, 'package.json'), '{"type":"module"}');
  copyFileSync(built, join(pkg, 'dist/cli.js'));
  writeFileSync(join(pkg, 'dist/native-source.json'), JSON.stringify(identity));
  writeFileSync(join(pkg, 'vendor/source.json'), JSON.stringify(identity));
  const binary = join(pkg, 'vendor/linux-x64/codex');
  writeFileSync(binary, `#!/bin/sh\necho unexpected > '${join(root, 'native-executed')}'\n`);
  chmodSync(binary, 0o755);
  const receipt = { ...identity, schema_version: 'ai-gateway-cli/native-artifact/v1', target_system: 'linux', target_arch: 'x64', binary: 'codex', sha256: createHash('sha256').update(readFileSync(binary)).digest('hex') };
  writeFileSync(join(pkg, 'vendor/linux-x64/artifact.json'), JSON.stringify(receipt));
  const guard = join(root, 'offline-guard.mjs');
  writeFileSync(guard, `import net from 'node:net'; import tls from 'node:tls'; import http from 'node:http'; import https from 'node:https'; import child from 'node:child_process'; import {syncBuiltinESMExports} from 'node:module'; const deny=()=>{throw new Error('test_forbidden_external_effect')}; net.Server.prototype.listen=deny; net.Socket.prototype.connect=deny; net.connect=net.createConnection=deny; tls.connect=deny; http.request=http.get=https.request=https.get=deny; globalThis.fetch=deny; child.spawn=child.spawnSync=child.exec=child.execSync=child.execFile=child.execFileSync=child.fork=deny; syncBuiltinESMExports();`);
  const home = join(root, 'private-cli-home');
  const config = { schema_version: 'ai-gateway-cli/providers/v1', active_provider: 'main', providers: { main: { name: secret, model: secret, base_url: `https://provider.invalid/${secret}`, wire_api: wire, auth: { type: 'bearer', api_key_env: 'TEST_PROVIDER_KEY' } } } };
  return { root, pkg, home, config, binary, receipt, guard };
}
function configure(f) {
  mkdirSync(f.home, { mode: 0o700 });
  writeFileSync(join(f.home, 'providers.json'), JSON.stringify(f.config), { mode: 0o600 });
}
function tree(root) {
  if (!existsSync(root)) return null;
  return Object.fromEntries(readdirSync(root, { recursive: true }).sort().map((relative) => {
    const path = join(root, relative); const stat = lstatSync(path);
    return [relative, stat.isFile() ? [stat.mode, createHash('sha256').update(readFileSync(path)).digest('hex')] : [stat.mode]];
  }));
}
function run(f, args = ['--json'], extra = {}) {
  const before = tree(f.root);
  const p = spawnSync(process.execPath, ['--import', pathToFileURL(f.guard).href, join(f.pkg, 'dist/cli.js'), 'doctor', ...args], {
    cwd: f.root, encoding: 'utf8', timeout: 10000,
    env: { HOME: f.root, AI_GATEWAY_CLI_HOME: f.home, PATH: '', TEST_PROVIDER_KEY: secret, ...extra },
  });
  assert.ifError(p.error);
  assert.equal(p.signal, null);
  assert.deepEqual(tree(f.root), before, 'doctor must not mutate any isolated fixture file or mode');
  assert.equal(existsSync(join(f.root, 'native-executed')), false);
  assert.equal(existsSync(join(f.home, 'runtime')), false);
  assert.equal((p.stdout + p.stderr).includes(secret), false, 'diagnostic leaked private input');
  assert.equal((p.stdout + p.stderr).includes(f.root), false, 'diagnostic leaked private absolute path');
  return p;
}
function report(p, exit) {
  assert.equal(p.status, exit, p.stderr);
  assert.equal(p.stderr, '');
  const value = JSON.parse(p.stdout);
  assert.equal(value.scope, 'local-offline');
  assert.equal(value.runtime_executed, false);
  assert.equal(value.network_checked, false);
  return Object.fromEntries(value.checks.map(c => [c.id, c]));
}

test('missing home reports independent checks without creating it', () => {
  const f = fixture(); const checks = report(run(f), 1);
  assert.equal(checks.config.code, 'local_config_missing');
  assert.equal(checks.native_artifact.code, 'packaged_identity_verified');
  assert.equal(checks.product_directory.code, 'missing_parent_access_observed');
  assert.equal(existsSync(f.home), false);
});
for (const wire of ['responses', 'chat_completions']) test(`${wire} configured setup is read-only and does not spawn/listen`, () => {
  const f = fixture(wire); configure(f); const checks = report(run(f), 0);
  assert.equal(checks.credentials.code, 'required_environment_present');
  assert.equal(checks.optional_bwrap.status, 'warn');
  assert.equal(checks.native_artifact.code, 'packaged_identity_verified');
});
test('missing credentials do not hide artifact/directory checks', () => {
  const f = fixture(); configure(f); const checks = report(run(f, ['--json'], { TEST_PROVIDER_KEY: '' }), 1);
  assert.equal(checks.credentials.code, 'required_environment_missing');
  assert.equal(checks.native_artifact.status, 'pass');
  assert.ok(checks.runtime_directory);
});
test('malformed configuration does not echo JSON or secrets', () => {
  const f = fixture(); configure(f); writeFileSync(join(f.home, 'providers.json'), `{invalid ${secret}`);
  assert.equal(report(run(f), 1).config.code, 'local_config_invalid');
});
for (const [kind, expected] of [['target', 'native_target_mismatch'], ['hash', 'native_binary_hash_mismatch'], ['source', 'native_source_mismatch'], ['malformed', 'native_receipt_invalid']]) test(`${kind} receipt/source failure remains safely actionable`, () => {
  const f = fixture(); configure(f);
  if (kind === 'target') f.receipt.target_arch = 'arm64';
  if (kind === 'hash') f.receipt.sha256 = 'f'.repeat(64);
  if (kind === 'source') f.receipt.overlay_sha256 = 'f'.repeat(64);
  writeFileSync(join(f.pkg, 'vendor/linux-x64/artifact.json'), kind === 'malformed' ? secret : JSON.stringify(f.receipt));
  assert.equal(report(run(f), 1).native_artifact.code, expected);
});
test('explicit unsafe development identity is never release-verified', () => {
  const f = fixture(); configure(f);
  assert.equal(report(run(f, ['--json'], { AI_GATEWAY_AGENT_RUNTIME_BIN: f.binary, AI_GATEWAY_UNSAFE_DEV_RUNTIME: '1' }), 0).native_artifact.code, 'development_identity_unverified');
  assert.equal(report(run(f, ['--json'], { AI_GATEWAY_AGENT_RUNTIME_BIN: f.binary }), 1).native_artifact.code, 'development_override_denied');
});
test('override selects locally without writing active provider', () => {
  const f = fixture(); f.config.providers.other = { ...f.config.providers.main, auth: { type: 'none' } }; configure(f);
  assert.equal(report(run(f, ['--json', '--cli-provider', 'other'], { TEST_PROVIDER_KEY: '' }), 0).credentials.status, 'pass');
  assert.equal(report(run(f, ['--json', '--cli-provider', 'missing']), 1).provider.code, 'provider_not_selected_or_missing');
});
test('symlink product home is rejected without traversal or writes', () => {
  const f = fixture(); const target = join(f.root, 'other'); mkdirSync(target); symlinkSync(target, f.home);
  assert.equal(report(run(f), 1).product_directory.code, 'directory_type_unsafe');
  assert.deepEqual(readdirSync(target), []);
});
test('human output is safe, help is read-only, invalid syntax exits 2', () => {
  const f = fixture(); const human = run(f, []); assert.equal(human.status, 1); assert.match(human.stdout, /local, offline, read-only/);
  const help = run(f, ['--help']); assert.equal(help.status, 0); assert.match(help.stdout, /^Usage:/);
  for (const args of [['--json', '--json'], ['--cli-provider'], ['--cli-provider', '--json'], ['--unknown', secret], ['--cli-provider', 'main', '--cli-provider', 'main']]) {
    const p = run(f, args); assert.equal(p.status, 2); assert.equal(p.stdout, ''); assert.match(p.stderr, /^Usage:/);
  }
});

for (const [kind, expected] of [['binary', 'native_artifact_missing'], ['receipt', 'native_receipt_invalid']]) test(`missing ${kind} is independently reported`, () => {
  const f = fixture(); configure(f);
  unlinkSync(kind === 'binary' ? f.binary : join(f.pkg, 'vendor/linux-x64/artifact.json'));
  assert.equal(report(run(f), 1).native_artifact.code, expected);
});

test('missing unsafe system executable is a blocker, not verified readiness', () => {
  const f = fixture(); configure(f); unlinkSync(f.binary);
  const checks = report(run(f, ['--json'], { AI_GATEWAY_USE_SYSTEM_CODEX: '1', AI_GATEWAY_UNSAFE_DEV_RUNTIME: '1' }), 1);
  assert.equal(checks.native_artifact.code, 'development_identity_unverified');
  assert.equal(checks.native_executable.code, 'native_executable_missing');
});
test('explicit runtime directory is not executable setup', () => {
  const f = fixture(); configure(f);
  assert.equal(report(run(f, ['--json'], { AI_GATEWAY_AGENT_RUNTIME_BIN: f.root, AI_GATEWAY_UNSAFE_DEV_RUNTIME: '1' }), 1).native_executable.code, 'native_executable_invalid_type');
});
for (const explicit of [false, true]) test(`${explicit ? 'explicit' : 'packaged'} file without execute permission is a blocker`, () => {
  const f = fixture(); configure(f); chmodSync(f.binary, 0o600);
  const extra = explicit ? { AI_GATEWAY_AGENT_RUNTIME_BIN: f.binary, AI_GATEWAY_UNSAFE_DEV_RUNTIME: '1' } : {};
  assert.equal(report(run(f, ['--json'], extra), 1).native_executable.code, 'native_executable_access_denied');
});
