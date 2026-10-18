use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
pub const PROTOCOL: &str = "ai-platform/sdk-agent/v1";
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Agent { pub model: String, pub instructions: String, pub max_turns: u8 }
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadBridge { pub grant: String, pub dataset_ids: Vec<String> }
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tool { pub name: String, pub tool: String, pub namespace: Option<String>, pub description: String, pub input_schema: Value, pub id: String, pub schema_hash: String, pub effect: String }
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityBridge { pub grant: String, pub run_id: String, pub tools: Vec<Tool>, pub timeout_seconds: u64 }
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub protocol: String, pub framework: String, pub service_id: String, pub tenant_id: String,
    pub user_id: String, pub request_id: String, pub session_id: String, pub agent: Agent,
    pub input: String, pub stream: bool,
    #[serde(default, skip_serializing_if="Option::is_none")] pub read_bridge: Option<ReadBridge>,
    #[serde(default, skip_serializing_if="Option::is_none")] pub capability_bridge: Option<CapabilityBridge>,
}
pub fn digest(bytes: impl AsRef<[u8]>) -> String { hex::encode(Sha256::digest(bytes)) }
pub fn identity(s: &str) -> bool { !s.trim().is_empty() && s.chars().count() <= 256 && !s.chars().any(|c| (c as u32) < 32) }
fn grant(s: &str) -> bool { s.len()==64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) }
// Python json.dumps defaults to ensure_ascii=True; keep scope hashes byte-compatible.
pub fn python_json(value: &Value) -> String {
    let raw = serde_json::to_string(value).expect("JSON value"); let mut out=String::new();
    for c in raw.chars() { if c <= '~' { out.push(c); } else { for unit in c.encode_utf16(&mut [0;2]) { out.push_str(&format!("\\u{unit:04x}")); } } } out
}
pub fn exact_keys(value: &Value, keys: &[&str]) -> bool {
    value.as_object().is_some_and(|o|o.len()==keys.len() && keys.iter().all(|key|o.contains_key(*key)))
}
pub fn parse_request(value: Value) -> Result<Launch> {
    let mut expected=vec!["protocol","framework","service_id","tenant_id","user_id","request_id","session_id","agent","input","stream"];
    if value.get("read_bridge").is_some(){expected.push("read_bridge");}
    if value.get("capability_bridge").is_some(){expected.push("capability_bridge");}
    ensure!(exact_keys(&value,&expected) && exact_keys(&value["agent"],&["model","instructions","max_turns"]),"sdk_request_invalid");
    if let Some(b)=value.get("read_bridge").filter(|v|!v.is_null()) {ensure!(exact_keys(b,&["grant","dataset_ids"]),"sdk_read_bridge_invalid");}
    if let Some(b)=value.get("capability_bridge").filter(|v|!v.is_null()) {
        ensure!(exact_keys(b,&["grant","run_id","tools","timeout_seconds"]),"sdk_capability_bridge_invalid");
        for t in b["tools"].as_array().ok_or_else(||anyhow::anyhow!("sdk_capability_bridge_invalid"))? {ensure!(exact_keys(t,&["name","tool","namespace","description","input_schema","id","schema_hash","effect"]),"sdk_capability_descriptor_invalid");}
    }
    Ok(serde_json::from_value(value)?)
}
impl Launch {
    pub fn validate(&mut self) -> Result<()> {
        ensure!(self.protocol==PROTOCOL && matches!(self.framework.as_str(),"openai_agents"|"claude_agent_sdk"), "sdk_request_invalid");
        ensure!([&self.service_id,&self.tenant_id,&self.user_id,&self.request_id,&self.session_id].iter().all(|s| identity(s)),"sdk_identity_invalid");
        ensure!((1..=32).contains(&self.agent.max_turns),"sdk_max_turns_invalid");
        if self.agent.instructions.trim().is_empty() { self.agent.instructions="You are a helpful assistant.".into(); }
        for (s,limit) in [(&self.agent.model,160),(&self.agent.instructions,32768),(&self.input,262144)] { ensure!(!s.trim().is_empty() && s.len()<=limit,"sdk_request_invalid"); }
        if let Some(b)=&self.read_bridge { ensure!(grant(&b.grant) && (1..=16).contains(&b.dataset_ids.len()),"sdk_read_bridge_invalid"); let mut seen=HashSet::new(); for id in &b.dataset_ids { ensure!(uuid::Uuid::parse_str(id).is_ok() && seen.insert(id),"sdk_read_bridge_invalid"); } }
        if let Some(b)=&self.capability_bridge {
            ensure!(grant(&b.grant) && uuid::Uuid::parse_str(&b.run_id).is_ok() && b.tools.len()<=16 && b.timeout_seconds==if b.tools.is_empty(){300}else{1800},"sdk_capability_bridge_invalid");
            ensure!(b.tools.is_empty() || self.stream,"sdk_capabilities_require_stream");
            let mut names=HashSet::new(); let mut ids=HashSet::new();
            for t in &b.tools { let suffix=t.name.strip_prefix("platform_capability_").unwrap_or("");
                ensure!(!suffix.is_empty() && suffix.bytes().all(|c|c.is_ascii_digit()) && names.insert(&t.name) && ids.insert((&t.namespace,&t.tool)),"sdk_capability_descriptor_invalid");
                ensure!((1..=160).contains(&t.tool.chars().count()) && t.namespace.as_ref().is_none_or(|n|(1..=64).contains(&n.chars().count())) && t.description.chars().count()<=16384 && t.input_schema.is_object() && python_json(&t.input_schema).len()<=65536 && t.schema_hash.strip_prefix("sha256:").is_some_and(grant) && matches!(t.effect.as_str(),"read"|"write"|"unknown"),"sdk_capability_descriptor_invalid");
            }
        } Ok(())
    }
    pub fn scope(&self) -> String { digest(python_json(&serde_json::json!([self.tenant_id,self.user_id,self.service_id,self.framework,self.session_id]))) }
    pub fn timeout(&self) -> u64 { self.capability_bridge.as_ref().map_or(300,|b|b.timeout_seconds) }
    pub fn profile(&self) -> Value { let mut p=serde_json::json!({"schema":"sdk-session-tools/v1","dataset_ids":self.read_bridge.as_ref().map(|b|b.dataset_ids.clone()).unwrap_or_default()}); if let Some(b)=&self.capability_bridge { if !b.tools.is_empty(){p["capability_tools"]=serde_json::to_value(&b.tools).unwrap();} } p }
}
#[cfg(test)] mod tests { use super::*;
#[test] fn python_ascii_scope_encoding(){assert_eq!(python_json(&serde_json::json!(["中","😀"])),r#"["\u4e2d","\ud83d\ude00"]"#);}
#[test] fn identities_reject_controls(){assert!(!identity("a\n"));assert!(!identity(" "));assert!(identity("租户"));}
}
