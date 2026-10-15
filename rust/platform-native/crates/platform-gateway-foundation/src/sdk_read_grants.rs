//! Ephemeral least-privilege SDK reads. Grants contain no provider credentials and
//! are minted only from the authenticated invocation route, never request context.
use crate::{protocol::{ServiceDefinition,UnifiedRequest},sdk_agent_config::is_sdk};
use axum::http::{HeaderMap,HeaderValue};
use ai_platform_auth::service::Principal;
use serde_json::{Value,json};
use std::{collections::HashMap,sync::{Arc,Mutex},time::{Duration,Instant}};

pub fn datasets(service:&ServiceDefinition)->Result<Vec<String>,&'static str>{
    let Some(config)=service.metadata.get("sdk_tools") else{return Ok(vec![])};
    let object=config.as_object().ok_or("sdk_tools_invalid")?;
    crate::sdk_capabilities::selected(service)?;
    let Some(values)=object.get("knowledge_dataset_ids") else{return Ok(vec![])};
    let values=values.as_array().filter(|v|v.len()<=16).ok_or("sdk_tools_invalid")?;
    let mut ids=vec![];
    for value in values {
        let id=value.as_str().ok_or("sdk_dataset_invalid")?;
        uuid::Uuid::parse_str(id).map_err(|_|"sdk_dataset_invalid")?;
        if ids.iter().any(|v|v==id){return Err("sdk_dataset_duplicate");}
        ids.push(id.to_owned());
    }
    Ok(ids)
}
#[derive(Clone)]
pub struct BoundRead {
    pub tenant_id:String,pub user_id:String,pub service_id:String,pub request_id:String,
    pub session_id:String,pub dataset_ids:Vec<String>,pub capability_ids:Vec<String>,
    /// Original JWT remains inside Gateway; never serialize or Debug this value.
    pub authorization:HeaderValue,
}
struct Grant {bound:BoundRead,expires:Instant,activated:bool,calls:u16}
#[derive(Default)]
pub struct SdkReadGrants {grants:Mutex<HashMap<String,Grant>>}
pub struct SdkReadLease {store:Arc<SdkReadGrants>,token:String}
impl Drop for SdkReadLease {
    fn drop(&mut self){self.store.grants.lock().unwrap_or_else(|e|e.into_inner()).remove(&self.token);}
}
impl SdkReadGrants {
    pub fn issue(self:&Arc<Self>,request:&UnifiedRequest,service:&ServiceDefinition,principal:&Principal,headers:&HeaderMap)->Result<Option<SdkReadLease>,&'static str>{
        if !is_sdk(service){return Ok(None)}
        let dataset_ids=datasets(service)?;
        let capability_ids=crate::sdk_capabilities::selected(service)?;
        let ttl=if capability_ids.is_empty(){330}else{1830};
        if principal.account.user_id!=request.user_id || principal.account.tenant_id!=request.tenant_id {
            return Err("sdk_read_identity_invalid");
        }
        let mut authorization=headers.get("authorization").cloned().ok_or("sdk_jwt_required")?;
        if !authorization.to_str().is_ok_and(|s|s.starts_with("Bearer ")) {return Err("sdk_jwt_required");}
        authorization.set_sensitive(true);
        let mut grants=self.grants.lock().unwrap_or_else(|e|e.into_inner());
        grants.retain(|_,v|v.expires>Instant::now());
        if grants.len()>=1024{return Err("sdk_read_capacity");}
        if grants.values().any(|g|g.bound.tenant_id==request.tenant_id && g.bound.user_id==request.user_id && g.bound.service_id==request.service_id && g.bound.request_id==request.request_id){return Err("sdk_request_already_active");}
        let token=uuid::Uuid::new_v4().simple().to_string()+&uuid::Uuid::new_v4().simple().to_string();
        grants.insert(token.clone(),Grant {bound:BoundRead{tenant_id:request.tenant_id.clone(),user_id:request.user_id.clone(),service_id:request.service_id.clone(),request_id:request.request_id.clone(),session_id:String::new(),dataset_ids,capability_ids,authorization},expires:Instant::now()+Duration::from_secs(ttl),activated:false,calls:0});
        Ok(Some(SdkReadLease{store:self.clone(),token}))
    }
    /// Only the post-admission adapter activates a grant and pins the resolved session.
    pub fn activate(&self,request:&UnifiedRequest,service:&ServiceDefinition)->Result<Option<Value>,&'static str>{
        if !is_sdk(service){return Ok(None)}
        datasets(service)?;
        crate::sdk_capabilities::selected(service)?;
        let mut grants=self.grants.lock().unwrap_or_else(|e|e.into_inner());
        let (token,grant)=grants.iter_mut().find(|(_,g)|g.expires>Instant::now() && g.bound.tenant_id==request.tenant_id && g.bound.user_id==request.user_id && g.bound.service_id==request.service_id && g.bound.request_id==request.request_id).ok_or("sdk_read_grant_unavailable")?;
        if grant.activated{return Err("sdk_read_grant_already_active");}
        grant.bound.session_id=request.session_id.clone().filter(|v|!v.is_empty()).ok_or("sdk_session_required")?;
        grant.activated=true;
        Ok(Some(json!({"grant":token,"dataset_ids":grant.bound.dataset_ids})))
    }
    pub fn admit(&self,token:&str,dataset:&str)->Result<BoundRead,&'static str>{
        let mut grants=self.grants.lock().unwrap_or_else(|e|e.into_inner());
        let grant=grants.get_mut(token).ok_or("sdk_read_grant_invalid")?;
        if !grant.activated || grant.expires<=Instant::now() || grant.calls>=32 || !grant.bound.dataset_ids.iter().any(|v|v==dataset){return Err("sdk_read_not_allowed");}
        grant.calls+=1;Ok(grant.bound.clone())
    }
    /// Provenance for a governed run; the durable SDK run, not this bearer,
    /// supplies dispatch authority. Current service/JWT checks are mandatory.
    pub fn bound(&self,token:&str)->Result<BoundRead,&'static str>{
        self.grants.lock().unwrap_or_else(|e|e.into_inner()).get(token)
            .filter(|g|g.activated && g.expires>Instant::now())
            .map(|g|g.bound.clone()).ok_or("sdk_grant_invalid")
    }
    pub fn first_dataset(&self,token:&str)->Option<String>{
        self.grants.lock().unwrap_or_else(|e|e.into_inner()).get(token)
            .filter(|g|g.activated && g.expires>Instant::now()).and_then(|g|g.bound.dataset_ids.first().cloned())
    }
    pub fn still_active(&self,token:&str)->bool{
        self.grants.lock().unwrap_or_else(|e|e.into_inner()).get(token).is_some_and(|g|g.activated && g.expires>Instant::now())
    }
}

#[cfg(test)]
mod lineage_tests {
use super::*;
use ai_platform_auth::accounts::{Account,Authority};
fn principal()->Principal {Principal{account:Account{user_id:"user".into(),tenant_id:"tenant".into(),email:None,display_name:None,department:None,tier:"user".into(),roles:vec!["user".into()],permissions:vec![],status:"active".into(),password_hash:None,force_password_change:false,login_attempts:0,locked_until:None,password_changed_at:None},authority:Authority{roles:vec!["user".into()],permissions:vec![],tier:"user".into()},jti:"synthetic-session".into(),expires_at:0}}
fn setup()->(ServiceDefinition,UnifiedRequest,HeaderMap){
    let s=ServiceDefinition{service_id:"svc".into(),metadata:json!({"adapter_type":"openai_agents"}),..Default::default()};
    let mut r=UnifiedRequest::new("request","svc",vec![]);r.tenant_id="tenant".into();r.user_id="user".into();r.session_id=Some("session".into());
    let mut h=HeaderMap::new();h.insert("authorization","Bearer synthetic-jwt-only".parse().unwrap());(s,r,h)
}

#[test]
fn pure_runs_require_scoped_provenance_without_knowledge_permission(){
    let (s,r,h)=setup();let store=Arc::new(SdkReadGrants::default());
    assert!(store.activate(&r,&s).is_err());
    assert!(store.issue(&r,&s,&principal(),&HeaderMap::new()).is_err());
    let lease=store.issue(&r,&s,&principal(),&h).unwrap().unwrap();
    let body=store.activate(&r,&s).unwrap().unwrap();let token=body["grant"].as_str().unwrap();
    let bound=store.bound(token).unwrap();
    assert_eq!(bound.session_id,"session");assert!(bound.dataset_ids.is_empty());assert!(bound.capability_ids.is_empty());
    assert!(store.admit(token,"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").is_err());
    assert!(store.issue(&r,&s,&principal(),&h).is_err());
    drop(lease);assert!(store.bound(token).is_err());
}
#[test]
fn pure_run_provenance_rejects_identity_substitution(){
    let (s,mut r,h)=setup();r.user_id="another-user".into();
    assert!(Arc::new(SdkReadGrants::default()).issue(&r,&s,&principal(),&h).is_err());
}
#[test]
fn exhausted_knowledge_quota_does_not_block_lifecycle_authority(){
    let (mut s,r,h)=setup();
    let dataset="aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    s.metadata["sdk_tools"]=json!({"knowledge_dataset_ids":[dataset]});
    let store=Arc::new(SdkReadGrants::default());
    let _lease=store.issue(&r,&s,&principal(),&h).unwrap().unwrap();
    let body=store.activate(&r,&s).unwrap().unwrap();let token=body["grant"].as_str().unwrap();
    // Server + child admission, then two checks for each of 15 retrievals.
    for _ in 0..32 {assert!(store.admit(token,dataset).is_ok());}
    assert!(store.admit(token,dataset).is_err());
    assert!(store.bound(token).is_ok());
    assert!(store.admit(token,dataset).is_err());
}

}
