//! Runtime consumes Gateway public keys and scoped turn witnesses, never root keys.
use ai_platform_capability_contract::lease_v3::{self as wire, CapabilityToken, PublicKeyring, Restriction, SignedEnvelope};
use ai_platform_capability_contract::RuntimeCapabilityLeaseV1;
use serde_json::Value;
use crate::{PlatformThreadIdentity, PostgresThreadStore};

pub(crate) fn public_keys() -> Result<Option<PublicKeyring>,String> {
    if std::env::var_os("AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE").is_some(){return Err("consumer_signing_key_forbidden".into())}
    let file=std::env::var_os("AI_PLATFORM_LEASE_VERIFY_KEYRING_FILE");
    let json=std::env::var("AI_PLATFORM_LEASE_VERIFY_KEYRING_JSON").ok();
    if file.is_some()&&json.is_some(){return Err("lease_verification_config_invalid".into())}
    let bytes=match(file,json){(Some(file),None)=>Some(std::fs::read(file).map_err(|_|"lease_verification_config_invalid")?),(None,Some(json))=>Some(json.into_bytes()),_=>None};
    let Some(bytes)=bytes else{
        if !matches!(std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref(),Err(_)|Ok("legacy_hmac")){return Err("lease_verification_key_missing".into())}
        return Ok(None)
    };
    if bytes.len()>16_384||std::env::var_os("AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE").is_some(){return Err("consumer_signing_key_forbidden".into())}
    let keys:PublicKeyring=serde_json::from_slice(&bytes).map_err(|_|"lease_verification_config_invalid")?;
    keys.validate().map_err(str::to_owned)?;
    if !matches!(std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref(),Ok("verify_only")|Ok("transition")|Err(_)){return Err("lease_trust_mode_invalid".into())}

    if std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref()!=Ok("transition")&&std::env::var_os("AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET").is_some(){return Err("consumer_signing_key_forbidden".into())}
    if std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref()==Ok("transition")&&!legacy_window(){return Err("legacy_lease_window_expired".into())}
    Ok(Some(keys))
}
fn now()->u64{std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128)as u64}
fn legacy_window()->bool{let now=now();std::env::var("AI_PLATFORM_LEASE_HMAC_ACCEPT_UNTIL_MS").ok().and_then(|v|v.parse::<u64>().ok()).is_some_and(|deadline|now<deadline&&deadline<=now.saturating_add(86_400_000))}
pub(crate) fn verify_turn(signature:&str,ceiling:Option<&SignedEnvelope>,scope:&PlatformThreadIdentity,run:&str,snapshot:&str,lease:&str,revision:u64,model:&str)->Result<(),String>{
    let keys=public_keys()?;
    if signature.starts_with(wire::ENVELOPE_PREFIX){
        let keys=keys.ok_or("lease_verification_key_missing")?;
        let envelope:SignedEnvelope=wire::decode(wire::ENVELOPE_PREFIX,signature).map_err(str::to_owned)?;
        keys.verify(&envelope,"model-lease",now()).map_err(str::to_owned)?;
        let thread_id=scope.runtime_thread_id.to_string();
        for(field,value)in[("tenant_id",scope.tenant_id.as_str()),("user_id",scope.user_id.as_str()),("session_id",scope.session_id.as_str()),("runtime_thread_id",thread_id.as_str()),("run_id",run),("snapshot_id",snapshot),("lease_id",lease),("model_id",model)]{if envelope.claims[field].as_str()!=Some(value){return Err("model_lease_binding_invalid".into())}}
        if envelope.claims["capability_revision"].as_u64()!=Some(revision)||envelope.claims["issued_at_ms"].as_u64()!=Some(envelope.issued_at_ms)||envelope.claims["expires_at_ms"].as_u64()!=Some(envelope.expires_at_ms){return Err("model_lease_binding_invalid".into())}
        let ceiling=ceiling.ok_or("turn_capability_ceiling_missing")?;
        keys.verify(ceiling,"capability-ceiling",now()).map_err(str::to_owned)?;
        let c:wire::Ceiling=serde_json::from_value(ceiling.claims.clone()).map_err(|_|"turn_capability_ceiling_invalid")?;
        if c.tenant_id!=scope.tenant_id||c.user_id!=scope.user_id||c.session_id!=scope.session_id||c.run_id!=run||c.snapshot_id!=snapshot||c.runtime_thread_id!=scope.runtime_thread_id.to_string()||c.capability_revision!=revision{return Err("turn_capability_ceiling_binding_invalid".into())}
        Ok(())
    }else if keys.is_none()||(std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref()==Ok("transition")&&legacy_window()){Ok(())}else{Err("legacy_model_lease_not_admitted".into())}
}
pub(crate) async fn capability_authority(store:&PostgresThreadStore,scope:&PlatformThreadIdentity,run:&str)->Result<Vec<u8>,String>{
    let keys=public_keys()?;
    if let Some(keys)=keys{
        let row:Option<Option<Value>>=sqlx::query_scalar("SELECT witness.token FROM assistant_runtime_snapshots s JOIN assistant_runs r ON r.run_id=s.run_id AND r.runtime_snapshot_id=s.snapshot_id JOIN assistant_runtime_model_leases l ON l.snapshot_id=s.snapshot_id AND l.run_id=r.run_id JOIN assistant_runtime_capability_witnesses witness ON witness.run_id=r.run_id AND witness.snapshot_id=s.snapshot_id AND witness.tenant_id=s.tenant_id AND witness.user_id=s.user_id AND witness.session_id=s.session_id AND witness.token->'root'=s.snapshot->'capability_ceiling' WHERE s.run_id::text=$1 AND s.runtime_thread_id=$2 AND s.tenant_id=$3 AND s.user_id=$4 AND s.session_id=$5 AND r.status='running' AND r.engine='agent_runtime' AND l.status='active' AND l.expires_at>clock_timestamp() AND (s.expires_at IS NULL OR s.expires_at>clock_timestamp()) AND NOT EXISTS(SELECT 1 FROM assistant_runtime_snapshot_revocations x WHERE x.snapshot_id=s.snapshot_id)")
            .bind(run).bind(uuid::Uuid::parse_str(&scope.runtime_thread_id.to_string()).map_err(|_|"capability_authority_invalid")?).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).fetch_optional(&store.pool).await.map_err(|_|"capability_authority_unavailable")?;
        if let Some(value)=row.flatten().filter(|v|!v.is_null()){
            let token:CapabilityToken=serde_json::from_value(value).map_err(|_|"capability_authority_invalid")?;
            let c=token.ceiling(&keys,now()).map_err(str::to_owned)?;
            if c.run_id!=run||c.tenant_id!=scope.tenant_id||c.user_id!=scope.user_id||c.session_id!=scope.session_id||c.runtime_thread_id!=scope.runtime_thread_id.to_string(){return Err("capability_authority_scope_invalid".into())}
            return Ok(format!("{}{}",wire::AUTHORITY_PREFIX,serde_json::to_string(&token).map_err(|_|"capability_authority_invalid")?).into_bytes())
        }
        if std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref()!=Ok("transition")||!legacy_window(){return Err("turn_capability_ceiling_missing".into())}
    }
    std::env::var("AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET").ok().filter(|v|v.len()>=32).map(String::into_bytes).ok_or_else(||"legacy_lease_secret_missing".into())
}
/// SDK authority uses its real SDK snapshot and owner/fence, never a Codex thread.
pub(crate) async fn sdk_capability_authority(tx:&mut sqlx::Transaction<'_,sqlx::Postgres>,scope:&ai_platform_capability_contract::CapabilityScopeV2,run:uuid::Uuid,snapshot:uuid::Uuid,owner:uuid::Uuid,fence:i64)->Result<Vec<u8>,String>{
    if let Some(keys)=public_keys()? {
        let value:Option<Value>=sqlx::query_scalar("SELECT w.token FROM assistant.assistant_runtime_capability_witnesses w JOIN assistant.assistant_sdk_runs s ON s.run_id=w.run_id AND s.snapshot_id=w.snapshot_id AND s.tenant_id=w.tenant_id AND s.user_id=w.user_id AND s.session_id=w.session_id JOIN assistant.assistant_runs r ON r.run_id=s.run_id AND r.tenant_id=s.tenant_id AND r.user_id=s.user_id AND r.session_id=s.session_id AND r.engine=s.engine WHERE s.run_id=$1 AND s.snapshot_id=$2 AND s.tenant_id=$3 AND s.user_id=$4 AND s.session_id=$5 AND s.owner_id=$6 AND s.fence=$7 AND s.engine IN ('openai_agents','claude_agent_sdk') AND r.status='running' AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp() AND s.lease_until>clock_timestamp() AND w.token->'root'=s.snapshot->'capability_ceiling'")
            .bind(run).bind(snapshot).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).bind(owner).bind(fence).fetch_optional(&mut **tx).await.map_err(|_|"sdk_capability_authority_unavailable")?;
        if let Some(value)=value {
            let token:CapabilityToken=serde_json::from_value(value).map_err(|_|"sdk_capability_authority_invalid")?;
            let ceiling=token.ceiling(&keys,now()).map_err(str::to_owned)?;
            if token.root.purpose!="sdk-capability-ceiling"||!ceiling.runtime_thread_id.is_empty()||ceiling.run_id!=run.to_string()||ceiling.snapshot_id!=snapshot.to_string()||ceiling.tenant_id!=scope.tenant_id||ceiling.user_id!=scope.user_id||ceiling.session_id!=scope.session_id{return Err("sdk_capability_authority_scope_invalid".into())}
            return Ok(format!("{}{}",wire::AUTHORITY_PREFIX,serde_json::to_string(&token).map_err(|_|"sdk_capability_authority_invalid")?).into_bytes())
        }
        if std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref()!=Ok("transition")||!legacy_window(){return Err("sdk_capability_ceiling_missing".into())}
    }
    std::env::var("AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET").ok().filter(|v|v.len()>=32).map(String::into_bytes).ok_or_else(||"legacy_lease_secret_missing".into())
}
pub(crate) fn authorize_call(authority:&[u8],lease:&mut RuntimeCapabilityLeaseV1,schema:&str,now:u64)->Result<bool,String>{
    let text=std::str::from_utf8(authority).map_err(|_|"capability_authority_invalid")?;
    let Some(text)=text.strip_prefix(wire::AUTHORITY_PREFIX) else{
        if public_keys()?.is_some()&&(std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref()!=Ok("transition")||!legacy_window()){return Err("legacy_capability_signing_forbidden".into())}
        return Ok(false)
    };
    let keys=public_keys()?.ok_or("lease_verification_key_missing")?;
    let token:CapabilityToken=serde_json::from_str(text).map_err(|_|"capability_authority_invalid")?;
    token.ceiling(&keys,now).map_err(str::to_owned)?;
    lease.expires_at_epoch_ms=lease.expires_at_epoch_ms.min(token.root.expires_at_ms);
    let call=serde_json::to_value(&*lease).map_err(|_|"capability_call_invalid")?;
    let restriction=Restriction{capability_ids:vec![lease.capability_id.clone()],expires_at_ms:lease.expires_at_epoch_ms,delegated:false,call:Some(call.clone()),descriptor_schema_hash:Some(schema.into())};
    let sealed=token.attenuate(restriction,&keys,now).and_then(|t|t.seal(&keys,now)).map_err(str::to_owned)?;
    sealed.verify_call(&keys,&call,schema,now).map_err(str::to_owned)?;
    lease.signature=wire::encode(wire::CAPABILITY_PREFIX,&sealed).map_err(str::to_owned)?;Ok(true)
}
