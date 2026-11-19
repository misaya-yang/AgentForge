/* Offline release-policy tooling. Never uploads, installs, starts the app, or
 * fabricates publisher credentials. Private key content is never returned. */
import { readFile, lstat, writeFile } from 'node:fs/promises';
import { createPrivateKey, createPublicKey, sign, verify } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
export function approvedURL(raw, origin) {
  const value = new URL(raw), expected = new URL(origin);
  if (value.protocol !== 'https:' || expected.protocol !== 'https:' || value.origin !== expected.origin || value.username || value.password || value.hash || value.search || expected.pathname !== '/' || expected.search || expected.hash || expected.username || expected.password) throw new Error('Update URL outside pinned HTTPS origin');
  return value;
}
export function validatePolicy(policy, config, now = Math.floor(Date.now()/1000)) {
  if (!config.enabled || !config.updater_public_key?.trim() || Buffer.from(config.policy_public_key ?? '', 'base64').length !== 32) throw new Error('Release keys are not configured');
  approvedURL(config.policy_url, config.download_origin);
  const allowed = ['channel','target','version','sequence','issued_at','expires_at','rollout_basis_points','manifest_url','artifact_size','manifest'];
  if (Object.keys(policy).some(key => !allowed.includes(key)) || Object.keys(policy).length !== allowed.length) throw new Error('Unknown or absent policy field');
  if (!['stable','beta'].includes(config.channel) || policy.channel !== config.channel || !['darwin-aarch64','darwin-x86_64','linux-x86_64','linux-aarch64','windows-x86_64'].includes(policy.target)) throw new Error('Channel or target refused');
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(policy.version) || (config.channel === 'stable' && policy.version.includes('-'))) throw new Error('Version refused');
  for(const key of ['sequence','issued_at','expires_at','rollout_basis_points','artifact_size']) if(!Number.isSafeInteger(policy[key])) throw new Error('Policy integer required');
  if (policy.sequence <= 0 || policy.issued_at < 0 || policy.issued_at > now+300 || policy.expires_at <= now || policy.expires_at <= policy.issued_at || policy.expires_at-policy.issued_at > 7*86400 || policy.rollout_basis_points < 0 || policy.rollout_basis_points > 10000 || policy.artifact_size <= 0 || policy.artifact_size > 512*1024*1024) throw new Error('Policy bounds refused');
  approvedURL(policy.manifest_url,config.download_origin);
  const platform = policy.manifest?.platforms?.[policy.target];
  if (policy.manifest?.version !== policy.version || typeof platform?.signature !== 'string' || !platform.signature.trim()) throw new Error('Signed artifact manifest required');
  approvedURL(platform.url,config.download_origin);
  return policy;
}
export async function signPolicy(policy, config, keyFile, output, now) {
  validatePolicy(policy,config,now);
  const metadata = await lstat(keyFile);
  if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size > 16384 || (process.platform !== 'win32' && (metadata.mode & 0o077))) throw new Error('Signing key must be a private regular file');
  const key = createPrivateKey(await readFile(keyFile));
  if(key.asymmetricKeyType !== 'ed25519') throw new Error('Ed25519 policy key required');
  const publicDER = createPublicKey(key).export({format:'der',type:'spki'});
  if (!publicDER.subarray(-32).equals(Buffer.from(config.policy_public_key,'base64'))) throw new Error('Signing key does not match pinned public key');
  const payload = Buffer.from(JSON.stringify(policy));
  if(payload.length > 40000) throw new Error('Policy too large');
  const envelope = {payload:payload.toString('base64'),signature:sign(null,payload,key).toString('base64')};
  if (!verify(null,payload,createPublicKey(key),Buffer.from(envelope.signature,'base64'))) throw new Error('Signature self-verification failed');
  await writeFile(output,JSON.stringify(envelope)+'\n',{flag:'wx',mode:0o644});
  return {version:policy.version,sequence:policy.sequence,target:policy.target,published:false};
}
if(process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if(process.argv.length !== 6) throw new Error('Usage: node release.mjs POLICY_JSON PUBLIC_CONFIG_JSON PRIVATE_ED25519_PEM NEW_ENVELOPE_JSON');
    const policy=JSON.parse(await readFile(process.argv[2],'utf8')), config=JSON.parse(await readFile(process.argv[3],'utf8'));
    console.log(JSON.stringify(await signPolicy(policy,config,process.argv[4],process.argv[5])));
  } catch {console.error('Release policy signing refused; verify configuration, policy and private key. No artifact was published.');process.exitCode=1;}
}
