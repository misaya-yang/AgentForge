// Explicit local-only operator signing. Cannot issue a production activation.
import { writeFileSync } from 'node:fs';
import { createPrivateKey, createPublicKey, sign } from 'node:crypto';
import { loadConfig, sourceFingerprint, validateActivationClaims } from './config.mjs';
import { canonical, sha256, readFile, reject } from './contract.mjs';
import { agentCard } from './adapter.mjs';
export function issueRehearsal(config, keyFile, kid, tenant, user, now = Math.floor(Date.now() / 1000)) {
 const key=createPrivateKey(readFile(keyFile));const publicKey=createPublicKey(key).export({format:'jwk'});if(key.asymmetricKeyType!=='ed25519'||config.activationKeys.get(kid)?.jwk.x!==publicKey.x)reject(503,'OPERATOR_SIGNING_PIN_MISMATCH');
 if(!config.credentials.has(`${tenant}\0${user}`))reject(503,'REHEARSAL_CREDENTIAL_OWNER_REQUIRED');
 const claims={schema:'agentforge-external-a2a-activation/v1',admission:'rehearsal',rehearsal:{tenant_id:tenant,user_id:user,profile_sha256:sha256(canonical(config.model)),address:config.model.address},u_gw15:{status:'not_run'},configuration_sha256:config.configSha,credentials_sha256:config.credentialSha,source_sha256:sourceFingerprint(),card_sha256:sha256(canonical(agentCard(config))),gateway_origin:config.gatewayOrigin,agent_origin:config.origin,allow_model_calls:true,iat:now,exp:now+600};
 validateActivationClaims(claims,config,claims.card_sha256,now);const protected64=Buffer.from(canonical({alg:'EdDSA',typ:'agentforge-external-a2a-activation+jwt',kid})).toString('base64url');const input=`${protected64}.${Buffer.from(canonical(claims)).toString('base64url')}`;return `${input}.${sign(null,Buffer.from(input),key).toString('base64url')}`;
}
if(import.meta.url===`file://${process.argv[1]}`){if(process.argv.length!==14||process.argv[2]!=='--config'||process.argv[4]!=='--signing-key'||process.argv[6]!=='--kid'||process.argv[8]!=='--tenant'||process.argv[10]!=='--user'||process.argv[12]!=='--output'){process.stderr.write('usage: rehearsal.mjs --config FILE --signing-key FILE --kid KID --tenant TENANT --user USER --output NEWFILE\n');process.exitCode=2;}else{try{const c=loadConfig(process.argv[3]);const compact=issueRehearsal(c,process.argv[5],process.argv[7],process.argv[9],process.argv[11]);writeFileSync(process.argv[13],`${compact}\n`,{mode:0o600,flag:'wx'});process.stdout.write('REHEARSAL_ONLY_600_SECONDS\n');}catch{process.stderr.write('REHEARSAL_ISSUANCE_REJECTED\n');process.exitCode=1;}}}
