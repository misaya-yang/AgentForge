//! Gateway-only root authority. Runtime/Worker receive only public configuration
//! and scoped attenuation witnesses; platform private keys never leave this host.
#[path = "../../../../../agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/lease_v3.rs"]
pub(super) mod wire;
use serde::Deserialize;
use serde_json::Value;
use super::AgentError;
use wire::{CapabilityToken, Ceiling, Grant, PublicKeyring, SignedEnvelope};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivateConfiguration {active_key_id:String,signing_seed_hex:String,verification:PublicKeyring}
fn config()->Result<Option<PrivateConfiguration>,AgentError>{
    let Some(path)=std::env::var_os("AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE") else{
        if std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref()==Ok("verify_only"){return Err(error())}
        return Ok(None)
    };
    let bytes=std::fs::read(path).map_err(|_|error())?;
    if bytes.len()>16_384{return Err(error())}
    let config:PrivateConfiguration=serde_json::from_slice(&bytes).map_err(|_|error())?;
    config.verification.validate().map_err(|_|error())?;wire::unhex::<32>(&config.signing_seed_hex).map_err(|_|error())?;Ok(Some(config))
}
pub(super) fn signing_enabled()->Result<bool,AgentError>{Ok(config()?.is_some())}
fn error()->AgentError{AgentError::code(503,"GATEWAY_LEASE_KEYRING_NOT_ADMITTED")}
pub(super) fn now_ms()->u64{chrono::Utc::now().timestamp_millis().max(0)as u64}
pub(super) fn sign_claims(purpose:&str,claims:&Value)->Result<Option<String>,AgentError>{
    let Some(c)=config()? else{return Ok(None)};
    let issued=claims["issued_at_ms"].as_u64().ok_or_else(error)?;
    let expires=claims["expires_at_ms"].as_u64().ok_or_else(error)?;
    let envelope=SignedEnvelope::issue(&wire::unhex::<32>(&c.signing_seed_hex).map_err(|_|error())?,&c.active_key_id,purpose,claims.clone(),issued,expires,&c.verification).map_err(|_|error())?;
    wire::encode(wire::ENVELOPE_PREFIX,&envelope).map(Some).map_err(|_|error())
}
pub(super) fn verify_claims(purpose:&str,signature:&str,claims:&Value,now:u64)->Result<(),AgentError>{
    let c=config()?.ok_or_else(error)?;
    let envelope:SignedEnvelope=wire::decode(wire::ENVELOPE_PREFIX,signature).map_err(|_|invalid())?;
    c.verification.verify(&envelope,purpose,now).map_err(|_|invalid())?;
    if envelope.claims!=*claims||claims["issued_at_ms"].as_u64()!=Some(envelope.issued_at_ms)||claims["expires_at_ms"].as_u64()!=Some(envelope.expires_at_ms){return Err(invalid())}Ok(())
}
fn invalid()->AgentError{AgentError::code(401,"RUNTIME_MODEL_LEASE_SIGNATURE_INVALID")}
pub(super) fn legacy_admitted(now:u64)->bool{
    if std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref()==Ok("verify_only"){return false}
    if std::env::var_os("AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE").is_none(){return matches!(std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref(),Err(_)|Ok("legacy_hmac"))}
    std::env::var("AI_PLATFORM_LEASE_HMAC_ACCEPT_UNTIL_MS").ok().and_then(|v|v.parse::<u64>().ok())
        .is_some_and(|deadline|now<deadline&&deadline<=now.saturating_add(86_400_000))
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn issue_ceiling(scope:&super::Scope,run:&str,snapshot:&str,thread:&str,revision:u64,readonly:&Value,expires:u64)->Result<Option<CapabilityToken>,AgentError>{
    let Some(c)=config()? else{return Ok(None)};
    let mut grants=Vec::new();let mut seen=std::collections::BTreeSet::new();
    for key in ["tools","mcp","deferred","attachment_tools"]{
        for descriptor in readonly[key].as_array().into_iter().flatten(){
            let id=descriptor["id"].as_str().ok_or_else(error)?.to_owned();
            if !seen.insert(id.clone()){return Err(error())}
            let effect=descriptor.get("effect").or_else(||descriptor.get("operation_kind")).and_then(Value::as_str)
                .unwrap_or(if descriptor["read_only"]==true{"read"}else{"write"});
            grants.push(Grant{capability_id:id,schema_hash:descriptor["schema_hash"].as_str().ok_or_else(error)?.into(),effect:effect.into()});
        }
    }
    let mut proof_seed=[0;32];getrandom::fill(&mut proof_seed).map_err(|_|error())?;
    let ceiling=Ceiling{schema_version:if thread.is_empty(){"sdk-capability-ceiling/v3"}else{"turn-capability-ceiling/v3"}.into(),tenant_id:scope.tenant_id.clone(),user_id:scope.user_id.clone(),session_id:scope.session_id.clone(),run_id:run.into(),snapshot_id:snapshot.into(),runtime_thread_id:thread.into(),capability_revision:revision,max_call_ttl_ms:120_000,grants,proof_key_hex:wire::hex(&ed25519_dalek::SigningKey::from_bytes(&proof_seed).verifying_key().to_bytes())};
    let root=SignedEnvelope::issue(&wire::unhex::<32>(&c.signing_seed_hex).map_err(|_|error())?,&c.active_key_id,if thread.is_empty(){"sdk-capability-ceiling"}else{"capability-ceiling"},serde_json::to_value(ceiling).map_err(|_|error())?,now_ms(),expires,&c.verification).map_err(|_|error())?;
    let token=CapabilityToken::issue(root,proof_seed);token.ceiling(&c.verification,now_ms()).map_err(|_|error())?;Ok(Some(token))
}
