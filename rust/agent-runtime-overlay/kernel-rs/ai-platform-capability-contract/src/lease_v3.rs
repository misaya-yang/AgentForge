//! Purpose-separated Ed25519 envelopes and append-only capability attenuation.
//! Root signing keys belong only to Gateway. Open proof seeds are scoped bearer
//! attenuation witnesses, never platform signing keys; dispatch receives a sealed
//! proof with no seed. This wire is not a Biscuit interoperability claim.
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
pub type Result<T> = std::result::Result<T, &'static str>;
pub const ENVELOPE_PREFIX: &str = "ed25519:v2:";
pub const CAPABILITY_PREFIX: &str = "ed25519:cap-v3:";
pub const AUTHORITY_PREFIX: &str = "ceiling-v3:";
pub const MAX_WIRE_BYTES: usize = 262_144;
fn need(ok: bool) -> Result<()> { if ok { Ok(()) } else { Err("asymmetric_lease_invalid") } }
pub fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(values) => Value::Object(values.iter().map(|(k,v)|(k.clone(),canonical(v))).collect::<BTreeMap<_,_>>().into_iter().collect()),
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}
pub fn bytes(value: &Value) -> Result<Vec<u8>> { serde_json::to_vec(&canonical(value)).map_err(|_|"asymmetric_wire_invalid") }
pub fn hex(value: &[u8]) -> String { value.iter().map(|v|format!("{v:02x}")).collect() }
pub fn unhex<const N: usize>(text: &str) -> Result<[u8;N]> {
    need(text.len()==N*2 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))?;
    let mut output=[0;N];
    for (i,slot) in output.iter_mut().enumerate() { *slot=u8::from_str_radix(&text[i*2..i*2+2],16).map_err(|_|"asymmetric_wire_invalid")?; }
    Ok(output)
}
pub fn encode<T: Serialize>(prefix:&str,value:&T)->Result<String>{
    let data=serde_json::to_vec(value).map_err(|_|"asymmetric_wire_invalid")?;
    need(data.len()<=MAX_WIRE_BYTES/2)?; Ok(format!("{prefix}{}",hex(&data)))
}
pub fn decode<T: serde::de::DeserializeOwned>(prefix:&str,text:&str)->Result<T>{
    let text=text.strip_prefix(prefix).ok_or("asymmetric_version_invalid")?;
    need(text.len()<=MAX_WIRE_BYTES && text.len()%2==0 && text.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)))?;
    let data=(0..text.len()).step_by(2).map(|i|u8::from_str_radix(&text[i..i+2],16).map_err(|_|"asymmetric_wire_invalid")).collect::<Result<Vec<_>>>()?;
    serde_json::from_slice(&data).map_err(|_|"asymmetric_wire_invalid")
}
fn signing(domain:&str,value:&Value)->Result<Vec<u8>>{let mut data=domain.as_bytes().to_vec();data.push(0);data.extend(bytes(value)?);Ok(data)}
fn verify(key:&str,domain:&str,value:&Value,signature:&str)->Result<()>{
    let key=VerifyingKey::from_bytes(&unhex::<32>(key)?).map_err(|_|"asymmetric_key_invalid")?;
    key.verify_strict(&signing(domain,value)?,&Signature::from_bytes(&unhex::<64>(signature)?)).map_err(|_|"asymmetric_signature_invalid")
}
fn sign(seed:&[u8;32],domain:&str,value:&Value)->Result<String>{Ok(hex(&SigningKey::from_bytes(seed).sign(&signing(domain,value)?).to_bytes()))}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicKey {pub key_id:String,pub public_key_hex:String,pub not_before_ms:u64,pub not_after_ms:u64,pub revoked:bool}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicKeyring {pub schema_version:String,pub keys:Vec<PublicKey>}
impl PublicKeyring {
    pub fn validate(&self)->Result<()>{
        need(self.schema_version=="gateway-verification-keys/v2" && !self.keys.is_empty() && self.keys.len()<=2)?;
        let mut seen=BTreeSet::new();
        for key in &self.keys {need(!key.key_id.is_empty()&&key.key_id.len()<=64&&key.key_id.bytes().all(|b|b.is_ascii_alphanumeric()||b"-_".contains(&b))&&seen.insert(&key.key_id)&&key.not_before_ms<key.not_after_ms)?;unhex::<32>(&key.public_key_hex)?;}
        Ok(())
    }
    fn key(&self,id:&str,now:u64)->Result<&PublicKey>{self.validate()?;self.keys.iter().find(|k|k.key_id==id&&!k.revoked&&now>=k.not_before_ms&&now<k.not_after_ms).ok_or("asymmetric_key_not_admitted")}
    pub fn verify(&self,envelope:&SignedEnvelope,purpose:&str,now:u64)->Result<()>{
        need(envelope.schema_version=="gateway-signed-envelope/v2"&&envelope.purpose==purpose&&envelope.issued_at_ms<envelope.expires_at_ms&&now.saturating_add(5000)>=envelope.issued_at_ms&&now<envelope.expires_at_ms)?;
        let key=self.key(&envelope.key_id,now)?;
        need(envelope.issued_at_ms>=key.not_before_ms&&envelope.expires_at_ms<=key.not_after_ms)?;
        verify(&key.public_key_hex,"agentforge/root-envelope/v2",&envelope.unsigned(),&envelope.signature_hex)
    }
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedEnvelope {pub schema_version:String,pub key_id:String,pub purpose:String,pub issued_at_ms:u64,pub expires_at_ms:u64,pub claims:Value,pub signature_hex:String}
impl SignedEnvelope {
    fn unsigned(&self)->Value{json!({"schema_version":self.schema_version,"key_id":self.key_id,"purpose":self.purpose,"issued_at_ms":self.issued_at_ms,"expires_at_ms":self.expires_at_ms,"claims":self.claims})}
    pub fn issue(seed:&[u8;32],key_id:&str,purpose:&str,claims:Value,issued:u64,expires:u64,keys:&PublicKeyring)->Result<Self>{
        let key=keys.key(key_id,issued)?;need(key.public_key_hex==hex(&SigningKey::from_bytes(seed).verifying_key().to_bytes()))?;
        let mut value=Self{schema_version:"gateway-signed-envelope/v2".into(),key_id:key_id.into(),purpose:purpose.into(),issued_at_ms:issued,expires_at_ms:expires,claims,signature_hex:String::new()};
        value.signature_hex=sign(seed,"agentforge/root-envelope/v2",&value.unsigned())?;keys.verify(&value,purpose,issued)?;Ok(value)
    }
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {pub capability_id:String,pub schema_hash:String,pub effect:String}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ceiling {pub schema_version:String,pub tenant_id:String,pub user_id:String,pub session_id:String,pub run_id:String,pub snapshot_id:String,pub runtime_thread_id:String,pub capability_revision:u64,pub max_call_ttl_ms:u64,pub grants:Vec<Grant>,pub proof_key_hex:String}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Restriction {pub capability_ids:Vec<String>,pub expires_at_ms:u64,pub delegated:bool,pub call:Option<Value>,pub descriptor_schema_hash:Option<String>}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {pub previous_hash:String,pub restriction:Restriction,pub next_key_hex:String,pub signature_hex:String}
impl Block {fn unsigned(&self)->Value{json!({"previous_hash":self.previous_hash,"restriction":self.restriction,"next_key_hex":self.next_key_hex})}}
#[derive(Clone,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum Proof {Open{seed_hex:String},Sealed{signature_hex:String}}
impl std::fmt::Debug for Proof {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{match self{Self::Open{..}=>f.write_str("Open([REDACTED])"),Self::Sealed{..}=>f.write_str("Sealed")}}}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityToken {pub schema_version:String,pub root:SignedEnvelope,pub blocks:Vec<Block>,pub proof:Proof}
struct Effective {ceiling:Ceiling,ids:BTreeSet<String>,expiry:u64,key:String,call:Option<Value>,schema:Option<String>}
impl CapabilityToken {
    fn transcript(&self)->Value{json!({"schema_version":self.schema_version,"root":self.root,"blocks":self.blocks})}
    fn transcript_hash(&self)->Result<String>{Ok(hex(&Sha256::digest(bytes(&self.transcript())?)))}
    fn effective(&self,keys:&PublicKeyring,now:u64)->Result<Effective>{
        need(self.schema_version=="capability-attenuation/v3"&&self.blocks.len()<=16)?;
        let sdk=self.root.purpose=="sdk-capability-ceiling";
        keys.verify(&self.root,if sdk{"sdk-capability-ceiling"}else{"capability-ceiling"},now)?;
        let ceiling:Ceiling=serde_json::from_value(self.root.claims.clone()).map_err(|_|"capability_ceiling_invalid")?;
        need(ceiling.schema_version==if sdk{"sdk-capability-ceiling/v3"}else{"turn-capability-ceiling/v3"}&&ceiling.capability_revision>0&&ceiling.max_call_ttl_ms>0&&ceiling.max_call_ttl_ms<=120_000&&ceiling.grants.len()<=256)?;
        for value in [&ceiling.tenant_id,&ceiling.user_id,&ceiling.session_id,&ceiling.run_id,&ceiling.snapshot_id]{need(!value.is_empty()&&value.len()<=255)?;}
        need(if sdk{ceiling.runtime_thread_id.is_empty()}else{!ceiling.runtime_thread_id.is_empty()&&ceiling.runtime_thread_id.len()<=255})?;
        let mut ids=BTreeSet::new();for grant in &ceiling.grants{need(!grant.capability_id.is_empty()&&ids.insert(grant.capability_id.clone())&&["read","write","unknown"].contains(&grant.effect.as_str())&&grant.schema_hash.starts_with("sha256:"))?;unhex::<32>(&grant.schema_hash[7..])?;}
        let mut key=ceiling.proof_key_hex.clone();unhex::<32>(&key)?;
        let mut expiry=self.root.expires_at_ms;let mut call=None;let mut schema=None;
        for (index,block) in self.blocks.iter().enumerate(){
            let prefix=json!({"schema_version":self.schema_version,"root":self.root,"blocks":&self.blocks[..index]});
            need(block.previous_hash==hex(&Sha256::digest(bytes(&prefix)?)))?;
            verify(&key,"agentforge/attenuation-block/v3",&block.unsigned(),&block.signature_hex)?;unhex::<32>(&block.next_key_hex)?;
            let selected=block.restriction.capability_ids.iter().cloned().collect::<BTreeSet<_>>();
            need(!selected.is_empty()&&selected.len()==block.restriction.capability_ids.len()&&selected.is_subset(&ids)&&(!block.restriction.delegated||selected!=ids)&&block.restriction.expires_at_ms<=expiry&&now<block.restriction.expires_at_ms)?;
            if let Some(next)=&block.restriction.call{if let Some(current)=&call{need(current==next)?;}call=Some(next.clone());}
            if let Some(next)=&block.restriction.descriptor_schema_hash{if let Some(current)=&schema{need(current==next)?;}schema=Some(next.clone());}
            ids=selected;expiry=block.restriction.expires_at_ms;key=block.next_key_hex.clone();
        }
        match &self.proof{
            Proof::Open{seed_hex}=>need(hex(&SigningKey::from_bytes(&unhex::<32>(seed_hex)?).verifying_key().to_bytes())==key)?,
            Proof::Sealed{signature_hex}=>verify(&key,"agentforge/attenuation-seal/v3",&json!(self.transcript_hash()?),signature_hex)?,
        }
        Ok(Effective{ceiling,ids,expiry,key,call,schema})
    }
    pub fn issue(root:SignedEnvelope,proof_seed:[u8;32])->Self{Self{schema_version:"capability-attenuation/v3".into(),root,blocks:Vec::new(),proof:Proof::Open{seed_hex:hex(&proof_seed)}}}
    pub fn ceiling(&self,keys:&PublicKeyring,now:u64)->Result<Ceiling>{Ok(self.effective(keys,now)?.ceiling)}
    pub fn attenuate(&self,restriction:Restriction,keys:&PublicKeyring,now:u64)->Result<Self>{
        self.effective(keys,now)?;let Proof::Open{seed_hex}=&self.proof else{return Err("sealed_token_cannot_attenuate")};let seed=unhex::<32>(seed_hex)?;
        // One-way child key derivation: possession of a child witness never
        // supplies the parent key needed to remove or rewrite an earlier block.
        let mut derivation=Hmac::<Sha256>::new_from_slice(&seed).map_err(|_|"attenuation_key_invalid")?;derivation.update(b"agentforge/child-proof-key/v3\0");derivation.update(&bytes(&json!(restriction))?);derivation.update(self.transcript_hash()?.as_bytes());
        let child:[u8;32]=derivation.finalize().into_bytes().into();
        let mut block=Block{previous_hash:self.transcript_hash()?,restriction,next_key_hex:hex(&SigningKey::from_bytes(&child).verifying_key().to_bytes()),signature_hex:String::new()};
        block.signature_hex=sign(&seed,"agentforge/attenuation-block/v3",&block.unsigned())?;
        let mut result=self.clone();result.blocks.push(block);result.proof=Proof::Open{seed_hex:hex(&child)};result.effective(keys,now)?;Ok(result)
    }
    pub fn seal(&self,keys:&PublicKeyring,now:u64)->Result<Self>{let effective=self.effective(keys,now)?;need(effective.call.is_some())?;let Proof::Open{seed_hex}=&self.proof else{return Err("token_already_sealed")};let mut result=self.clone();result.proof=Proof::Sealed{signature_hex:sign(&unhex::<32>(seed_hex)?,"agentforge/attenuation-seal/v3",&json!(self.transcript_hash()?))?};Ok(result)}
    pub fn verify_call(&self,keys:&PublicKeyring,call:&Value,schema_hash:&str,now:u64)->Result<()>{
        need(matches!(self.proof,Proof::Sealed{..}))?;let e=self.effective(keys,now)?;need(e.call.as_ref()==Some(call)&&e.schema.as_deref()==Some(schema_hash))?;
        let c=&e.ceiling;for(field,expected)in[("tenant_id",c.tenant_id.as_str()),("user_id",c.user_id.as_str()),("session_id",c.session_id.as_str()),("run_id",c.run_id.as_str())]{need(call[field].as_str()==Some(expected))?;}
        let id=call["capability_id"].as_str().ok_or("capability_call_invalid")?;
        need(e.ids.len()==1&&e.ids.contains(id)&&call["capability_revision"].as_u64()==Some(c.capability_revision)&&call["expires_at_epoch_ms"].as_u64().is_some_and(|expiry|expiry<=e.expiry&&now<expiry)
            &&call["issued_at_epoch_ms"].as_u64().is_some_and(|issued|issued<=now.saturating_add(5000)&&issued.saturating_add(5000)>=self.root.issued_at_ms
                &&call["expires_at_epoch_ms"].as_u64().is_some_and(|expiry|expiry>issued&&expiry-issued<=c.max_call_ttl_ms)))?;
        need(c.grants.iter().any(|g|g.capability_id==id&&call["effect"]==g.effect&&g.schema_hash==schema_hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture()->(PublicKeyring,CapabilityToken,Value){
        let seed=[1;32];let witness=[3;32];
        let keys=PublicKeyring{schema_version:"gateway-verification-keys/v2".into(),keys:vec![PublicKey{key_id:"old".into(),public_key_hex:hex(&SigningKey::from_bytes(&seed).verifying_key().to_bytes()),not_before_ms:1,not_after_ms:2000,revoked:false}]};
        let ceiling=Ceiling{schema_version:"turn-capability-ceiling/v3".into(),tenant_id:"tenant".into(),user_id:"user".into(),session_id:"session".into(),run_id:"run".into(),snapshot_id:"snapshot".into(),runtime_thread_id:"thread".into(),capability_revision:7,max_call_ttl_ms:120_000,grants:vec![Grant{capability_id:"read".into(),schema_hash:format!("sha256:{}","a".repeat(64)),effect:"read".into()},Grant{capability_id:"write".into(),schema_hash:format!("sha256:{}","b".repeat(64)),effect:"write".into()}],proof_key_hex:hex(&SigningKey::from_bytes(&witness).verifying_key().to_bytes())};
        let root=SignedEnvelope::issue(&seed,"old","capability-ceiling",serde_json::to_value(ceiling).unwrap(),100,1000,&keys).unwrap();
        let call=json!({"schema_version":"runtime-capability-lease/v1","lease_id":"lease","tenant_id":"tenant","user_id":"user","session_id":"session","run_id":"run","tool_call_id":"call","attempt_id":"call","capability_id":"read","capability_revision":7,"arguments_hash":format!("sha256:{}","c".repeat(64)),"effect":"read","issued_at_epoch_ms":200,"expires_at_epoch_ms":300,"nonce":"nonce-with-16bytes","signature":""});
        (keys,CapabilityToken::issue(root,witness),call)
    }
    fn restriction(call:Value)->Restriction{Restriction{capability_ids:vec!["read".into()],expires_at_ms:300,delegated:false,call:Some(call),descriptor_schema_hash:Some(format!("sha256:{}","a".repeat(64)))}}
    #[test]
    fn sdk_purpose_uses_genuine_run_without_codex_thread_and_cannot_cross_domains(){
        let(keys,token,call)=fixture();let mut claims=token.root.claims.clone();
        claims["schema_version"]=json!("sdk-capability-ceiling/v3");claims["runtime_thread_id"]=json!("");
        let root=SignedEnvelope::issue(&[1;32],"old","sdk-capability-ceiling",claims.clone(),100,1000,&keys).unwrap();
        let token=CapabilityToken::issue(root,[3;32]);
        let sealed=token.attenuate(restriction(call.clone()),&keys,200).unwrap().seal(&keys,200).unwrap();
        let schema=format!("sha256:{}","a".repeat(64));assert!(sealed.verify_call(&keys,&call,&schema,200).is_ok());
        assert!(!serde_json::to_string(&sealed).unwrap().contains("seed_hex"));
        for field in ["tenant_id","run_id","tool_call_id","capability_id"]{let mut changed=call.clone();changed[field]=json!("different");assert!(sealed.verify_call(&keys,&changed,&schema,200).is_err());}
        for (purpose,thread) in [("capability-ceiling",""),("sdk-capability-ceiling","fake-codex-thread")]{
            claims["runtime_thread_id"]=json!(thread);
            let root=SignedEnvelope::issue(&[1;32],"old",purpose,claims.clone(),100,1000,&keys).unwrap();
            assert!(CapabilityToken::issue(root,[3;32]).ceiling(&keys,200).is_err());
        }
        let mut forged=sealed;forged.root.purpose="capability-ceiling".into();assert!(forged.verify_call(&keys,&call,&schema,200).is_err());
    }
    #[test]
    fn sealed_call_binds_all_lease_fields_and_carries_no_attenuation_seed(){
        let(keys,token,call)=fixture();let token=token.attenuate(restriction(call.clone()),&keys,200).unwrap().seal(&keys,200).unwrap();
        let schema=format!("sha256:{}","a".repeat(64));assert!(token.verify_call(&keys,&call,&schema,200).is_ok());
        assert!(!serde_json::to_string(&token).unwrap().contains("seed_hex"));
        for field in ["tenant_id","user_id","session_id","run_id","tool_call_id","attempt_id","capability_id","arguments_hash","nonce"]{let mut changed=call.clone();changed[field]=json!("changed");assert!(token.verify_call(&keys,&changed,&schema,200).is_err());}
        assert!(token.verify_call(&keys,&call,&format!("sha256:{}","b".repeat(64)),200).is_err());
        assert!(token.verify_call(&keys,&call,&schema,301).is_err());
        assert!(token.attenuate(restriction(call),&keys,200).is_err());
    }
    #[test]
    fn verifier_cannot_mint_or_strip_or_change_a_sealed_chain(){
        let(keys,token,call)=fixture();let sealed=token.attenuate(restriction(call.clone()),&keys,200).unwrap().seal(&keys,200).unwrap();
        let schema=format!("sha256:{}","a".repeat(64));
        let mut changed=sealed.clone();changed.blocks.clear();assert!(changed.verify_call(&keys,&call,&schema,200).is_err());
        changed=sealed.clone();changed.blocks[0].restriction.capability_ids.push("write".into());assert!(changed.verify_call(&keys,&call,&schema,200).is_err());
        changed=sealed.clone();changed.root.claims["tenant_id"]=json!("other");assert!(changed.verify_call(&keys,&call,&schema,200).is_err());
        changed=sealed;let public_as_seed=unhex::<32>(&keys.keys[0].public_key_hex).unwrap();changed.root.signature_hex=sign(&public_as_seed,"agentforge/root-envelope/v2",&changed.root.unsigned()).unwrap();assert!(changed.verify_call(&keys,&call,&schema,200).is_err());
    }
    #[test]
    fn child_holder_cannot_widen_ancestor_allowlist_or_expiry(){
        let(keys,token,call)=fixture();
        let child=token.attenuate(Restriction{capability_ids:vec!["read".into()],expires_at_ms:500,delegated:true,call:None,descriptor_schema_hash:None},&keys,200).unwrap();
        for selected in [vec!["read".into(),"write".into()],vec!["write".into()]]{assert!(child.attenuate(Restriction{capability_ids:selected,expires_at_ms:400,delegated:false,call:None,descriptor_schema_hash:None},&keys,200).is_err());}
        let mut wider=restriction(call.clone());wider.expires_at_ms=501;assert!(child.attenuate(wider,&keys,200).is_err());
        let sealed=child.attenuate(restriction(call),&keys,200).unwrap().seal(&keys,200).unwrap();assert_eq!(sealed.blocks.len(),2);
        assert!(token.attenuate(Restriction{capability_ids:vec!["read".into(),"write".into()],expires_at_ms:400,delegated:true,call:None,descriptor_schema_hash:None},&keys,200).is_err());
    }
    #[test]
    fn worker_public_snapshot_owner_context_and_delivered_proof_cannot_widen_authority(){
        let(keys,open,call)=fixture();let sealed=open.attenuate(restriction(call.clone()),&keys,200).unwrap().seal(&keys,200).unwrap();
        // These are the exact shapes persisted by Gateway and Runtime: public
        // root envelope only. The open witness is held in a Worker-denied table.
        let visible=json!({"snapshot":{"capability_ceiling":open.root},
            "execution_owner":{"context":{"capabilityCeiling":open.root}},"delivered_proof":sealed});
        assert!(!serde_json::to_string(&visible).unwrap().contains("seed_hex"));
        let delivered:CapabilityToken=serde_json::from_value(visible["delivered_proof"].clone()).unwrap();
        let schema=format!("sha256:{}","a".repeat(64));
        for (field,value) in [("capability_id",json!("write")),("tool_call_id",json!("new-call")),
            ("tenant_id",json!("other-tenant")),("expires_at_epoch_ms",json!(999_999))] {
            let mut widened=call.clone();widened[field]=value;
            assert!(delivered.verify_call(&keys,&widened,&schema,200).is_err());
        }
        let mut minted=delivered.clone();minted.root.claims["max_call_ttl_ms"]=json!(u64::MAX);
        assert!(minted.verify_call(&keys,&call,&schema,200).is_err());
        minted=delivered.clone();minted.root.claims["grants"][0]["capability_id"]=json!("new-tool");
        // Treating any public snapshot bytes as an Ed25519 seed cannot recreate
        // the Gateway's signature. A copied model/launch envelope is purpose-bound.
        let fake_seed=unhex::<32>(&keys.keys[0].public_key_hex).unwrap();
        minted.root.signature_hex=sign(&fake_seed,"agentforge/root-envelope/v2",&minted.root.unsigned()).unwrap();
        assert!(minted.verify_call(&keys,&call,&schema,200).is_err());
        let mut from_snapshot=delivered;from_snapshot.root=serde_json::from_value(visible["snapshot"]["capability_ceiling"].clone()).unwrap();from_snapshot.blocks.clear();
        assert!(from_snapshot.verify_call(&keys,&call,&schema,200).is_err());
    }

    #[test]
    fn rotation_overlap_revocation_unknown_key_and_purpose_are_enforced(){
        let(mut keys,token,_)=fixture();let seed=[2;32];keys.keys.push(PublicKey{key_id:"new".into(),public_key_hex:hex(&SigningKey::from_bytes(&seed).verifying_key().to_bytes()),not_before_ms:150,not_after_ms:3000,revoked:false});
        assert!(token.ceiling(&keys,200).is_ok());
        let newer=SignedEnvelope::issue(&seed,"new","model-lease",json!({}),200,900,&keys).unwrap();assert!(keys.verify(&newer,"model-lease",250).is_ok());assert!(keys.verify(&newer,"runtime-launch",250).is_err());
        keys.keys[0].revoked=true;assert!(token.ceiling(&keys,200).is_err());keys.keys[0].revoked=false;
        let mut changed=token.clone();changed.root.key_id="missing".into();assert!(changed.ceiling(&keys,200).is_err());assert!(token.ceiling(&keys,2001).is_err());
        let mut config=serde_json::to_value(&keys).unwrap();config["signing_seed_hex"]=json!("01".repeat(32));assert!(serde_json::from_value::<PublicKeyring>(config).is_err());
    }
}
