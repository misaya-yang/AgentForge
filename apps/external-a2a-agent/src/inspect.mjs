// Read-only preflight; does not sign activation, start a listener or call a model.
import { loadConfig, sourceFingerprint, SDK } from './config.mjs';
import { agentCard } from './adapter.mjs';
import { canonical, sha256 } from './contract.mjs';
if (process.argv.length !== 4 || process.argv[2] !== '--config') { process.stderr.write('usage: node src/inspect.mjs --config /absolute/private/config.json\n'); process.exitCode = 2; }
else { try { const c = loadConfig(process.argv[3]); process.stdout.write(`${canonical({ schema: 'agentforge-external-a2a-preflight/v1', configuration_sha256: c.configSha, credentials_sha256: c.credentialSha, source_sha256: sourceFingerprint(), card_sha256: sha256(canonical(agentCard(c))), gateway_origin: c.gatewayOrigin, agent_origin: c.origin, framework: SDK, activation: 'not_checked', modelCalls: 'not_run', uGw15: 'not_run' })}\n`); } catch { process.stderr.write('EXTERNAL_A2A_PREFLIGHT_REJECTED\n'); process.exitCode = 1; } }
