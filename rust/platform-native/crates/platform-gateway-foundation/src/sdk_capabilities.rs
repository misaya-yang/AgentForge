//! Server-owned governed SDK admission. Workers receive a bounded opaque grant,
//! never a signing secret, original JWT or authority to choose a run identity.
use crate::protocol::{ServiceDefinition, UnifiedRequest};
use serde_json::Value;
use std::{future::Future, pin::Pin};
use tokio::sync::oneshot;
pub type AdmissionFuture<'a> = Pin<Box<dyn Future<Output=Result<SdkCapabilityLease,&'static str>> + Send + 'a>>;
pub trait SdkCapabilityAuthority: Send + Sync {
    fn admit<'a>(&'a self, request: &'a UnifiedRequest, service: &'a ServiceDefinition, grant: &'a str) -> AdmissionFuture<'a>;
}
pub struct SdkCapabilityLease {
    pub bridge: Value,
    pub finished: Option<oneshot::Sender<&'static str>>,
}
impl SdkCapabilityLease {
    pub fn fail(&mut self) {
        if let Some(done)=self.finished.take() {let _=done.send("failed");}
    }
    pub fn complete(&mut self) {
        if let Some(done)=self.finished.take() {let _=done.send("completed");}
    }
}
impl Drop for SdkCapabilityLease {
    fn drop(&mut self) {
        if let Some(done)=self.finished.take() {let _=done.send("cancelled");}
    }
}
/// Selection contains identifiers only. Schemas/effects/grants always come from
/// the existing current catalog, never from service metadata or request context.
pub fn selected(service:&ServiceDefinition)->Result<Vec<String>,&'static str> {
    let Some(config)=service.metadata.get("sdk_tools") else{return Ok(vec![])};
    let object=config.as_object().ok_or("sdk_tools_invalid")?;
    if object.keys().any(|k| !matches!(k.as_str(),"knowledge_dataset_ids"|"capability_ids")) {
        return Err("sdk_tools_invalid");
    }
    let Some(values)=object.get("capability_ids") else{return Ok(vec![])};
    let values=values.as_array().filter(|v|v.len()<=16).ok_or("sdk_capabilities_invalid")?;
    let mut ids=vec![];
    for value in values {
        let id=value.as_str().filter(|id|!id.is_empty() && id.len()<=160 && id.bytes().all(|b|b>32 && b<127))
            .ok_or("sdk_capability_invalid")?;
        if ids.iter().any(|v|v==id){return Err("sdk_capability_duplicate");}
        ids.push(id.to_owned());
    }
    Ok(ids)
}

/// Capability Contract V2 hashing uses sorted UTF-8 serde JSON, not the legacy
/// Python-compatible model/schema number formatter. Keep the wire hashes exact.
pub fn canonical_hash(value:&Value)->String {
    fn sorted(value:&Value)->Value {
        match value {
            Value::Object(values)=>{
                let keys=values.keys().collect::<std::collections::BTreeSet<_>>();
                Value::Object(keys.into_iter().map(|key|(key.clone(),sorted(&values[key]))).collect())
            },
            Value::Array(values)=>Value::Array(values.iter().map(sorted).collect()),
            scalar=>scalar.clone(),
        }
    }
    use sha2::{Digest,Sha256};
    format!("{:x}",Sha256::digest(serde_json::to_vec(&sorted(value)).expect("JSON value")))
}
