use crate::{contract::Launch, emit};
use anyhow::{Result, bail, ensure};
use serde_json::{Value,json};
use std::time::{Duration,Instant};

pub fn descriptors(l: &Launch) -> Vec<Value> {
    let mut tools=vec![json!({"name":"calculate","description":"Evaluate bounded arithmetic without code execution","input_schema":{"type":"object","properties":{"expression":{"type":"string","maxLength":256}},"required":["expression"],"additionalProperties":false}}),json!({"name":"utc_time","description":"Read current UTC time","input_schema":{"type":"object","properties":{},"additionalProperties":false}})];
    if let Some(b)=&l.read_bridge {tools.push(json!({"name":"knowledge_retrieve","description":"Retrieve relevant passages from explicitly selected platform knowledge datasets; cite returned sources.","input_schema":{"type":"object","properties":{"dataset_id":{"type":"string","enum":b.dataset_ids},"query":{"type":"string","maxLength":4096},"top_k":{"type":"integer","minimum":1,"maximum":5}},"required":["dataset_id","query","top_k"],"additionalProperties":false}}));}
    if let Some(b)=&l.capability_bridge {for t in &b.tools {tools.push(json!({"name":t.name,"description":t.description,"input_schema":t.input_schema}));}} tools
}
pub struct Bridge { client: reqwest::Client, origin: Option<String> }
impl Bridge {
    pub fn new() -> Result<Self> {
        let origin=std::env::var("SDK_AGENT_GATEWAY_URL").ok();
        if let Some(base)=&origin {let u=reqwest::Url::parse(base)?;ensure!(u.scheme()=="https" || (u.scheme()=="http" && matches!(u.host_str(),Some("localhost"|"127.0.0.1"|"[::1]"|"::1"))),"sdk_gateway_origin_invalid");ensure!(u.host_str().is_some() && u.username().is_empty() && u.password().is_none() && u.query().is_none() && u.fragment().is_none() && matches!(u.path(),""|"/"),"sdk_gateway_origin_invalid");}
        Ok(Self{client:reqwest::Client::builder().retry(reqwest::retry::never()).no_proxy().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(50)).build()?,origin})
    }
    async fn request(&self, group:&str,operation:&str,grant:&str,body:Value) -> Result<Value> {
        let base=self.origin.as_ref().ok_or_else(||anyhow::anyhow!("sdk_gateway_origin_invalid"))?;
        let mut response=self.client.post(format!("{}/internal/v1/sdk-tools/{group}/{operation}",base.trim_end_matches('/'))).bearer_auth(grant).json(&body).send().await?;
        ensure!(response.status().is_success(),"sdk_bridge_request_failed");
        let mut data=Vec::new();while let Some(chunk)=response.chunk().await? {ensure!(data.len()+chunk.len()<=1048576,"sdk_bridge_output_limit");data.extend_from_slice(&chunk);} Ok(serde_json::from_slice(&data)?)
    }
    pub async fn admit_execution(&self,l:&Launch)->Result<()> {
        if let Some(b)=&l.capability_bridge {ensure!(self.request("capabilities","admit",&b.grant,json!({"run_id":b.run_id})).await?["authorized"]==true,"sdk_capability_not_authorized");} Ok(())
    }
    pub async fn admit(&self,l:&Launch)->Result<()> {
        self.admit_execution(l).await?;
        if let Some(b)=&l.read_bridge {ensure!(self.request("knowledge","admit",&b.grant,json!({})).await?["authorized"]==true,"sdk_knowledge_not_authorized");} Ok(())
    }
    pub async fn invoke(&self,l:&Launch,name:&str,args:Value,call_id:&str)->Result<Value> {
        match name {
            "utc_time"=> {ensure!(args==json!({}),"tool_arguments_invalid");return Ok(json!({"utc":chrono::Utc::now().to_rfc3339()}));},
            "calculate"=> {ensure!(args.as_object().is_some_and(|o|o.len()==1),"tool_arguments_invalid");let exp=args["expression"].as_str().ok_or_else(||anyhow::anyhow!("tool_arguments_invalid"))?;return Ok(json!({"value":calculate(exp)?}));},
            "knowledge_retrieve"=> {let b=l.read_bridge.as_ref().ok_or_else(||anyhow::anyhow!("sdk_knowledge_not_enabled"))?;ensure!(args.as_object().is_some_and(|o|o.len()==3) && args["dataset_id"].as_str().is_some_and(|id|b.dataset_ids.iter().any(|d|d==id)) && args["query"].as_str().is_some_and(|q|(1..=4096).contains(&q.len())) && args["top_k"].as_u64().is_some_and(|n|(1..=5).contains(&n)),"sdk_knowledge_arguments_invalid");return self.request("knowledge","retrieve",&b.grant,args).await;},
            _=>{}
        }
        let b=l.capability_bridge.as_ref().ok_or_else(||anyhow::anyhow!("sdk_capability_not_enabled"))?;
        let t=b.tools.iter().find(|t|t.name==name).ok_or_else(||anyhow::anyhow!("sdk_tool_not_allowed"))?;
        ensure!(!call_id.is_empty() && call_id.len()<=160 && call_id.bytes().all(|c|c.is_ascii_alphanumeric() || b"_.:/-".contains(&c)) && args.is_object(),"sdk_tool_call_identity_required");
        ensure!(serde_json::to_vec(&args)?.len()<=65536,"sdk_capability_arguments_invalid");
        let body=json!({"run_id":b.run_id,"tool_call_id":call_id,"tool":t.tool,"namespace":t.namespace,"arguments":args});
        let deadline=Instant::now()+Duration::from_secs(660);let mut approval:Option<String>=None;
        loop {
            // Sequential loop, no retries on transport errors or uncertain outcomes.
            let r=self.request("capabilities","execute",&b.grant,body.clone()).await?;
            ensure!(r["run_id"]==b.run_id && r["tool_call_id"]==call_id && r["tool"]==t.tool,"sdk_capability_receipt_mismatch");
            let status=r["status"].as_str().unwrap_or("");
            if status=="approval_required" {
                let id=r["approval_id"].as_str().ok_or_else(||anyhow::anyhow!("sdk_approval_identity_changed"))?;
                ensure!(approval.as_ref().is_none_or(|a|a==id),"sdk_approval_identity_changed");
                if approval.is_none(){approval=Some(id.into());emit(&json!({"event":"approval_required","run_id":b.run_id,"tool_call_id":call_id,"tool":t.tool,"approval_id":id,"expires_at":r["expires_at"],"preview":r["preview"]}))?;}
                ensure!(Instant::now()<deadline,"sdk_approval_timeout");tokio::time::sleep(Duration::from_secs(1)).await;continue;
            }
            ensure!(matches!(status,"succeeded"|"failed"|"cancelled"|"timeout"|"side_effect_unknown"),"sdk_capability_nonterminal");
            if let Some(id)=approval {emit(&json!({"event":"approval_result","run_id":b.run_id,"tool_call_id":call_id,"tool":t.tool,"approval_id":id,"status":status,"approval_status":r["approval_status"]}))?;}
            emit(&json!({"event":"tool_result","run_id":b.run_id,"tool_call_id":call_id,"tool":t.tool,"status":status,"execution_id":r["execution_id"]}))?;
            ensure!(status!="side_effect_unknown","sdk_capability_outcome_unknown");
            return Ok(json!({"status":status,"execution_id":r["execution_id"],"receipt":r["receipt"],"response":r["response"]}));
        }
    }
}
fn number(literal:&str)->Result<f64> {
    let (digits,radix)=if literal.starts_with("0x") || literal.starts_with("0X"){(&literal[2..],16)}else if literal.starts_with("0o") || literal.starts_with("0O"){(&literal[2..],8)}else if literal.starts_with("0b") || literal.starts_with("0B"){(&literal[2..],2)}else{(literal,10)};
    if radix!=10 {
        let digits=digits.strip_prefix('_').unwrap_or(digits);
        ensure!(!digits.is_empty() && !digits.starts_with('_') && !digits.ends_with('_') && !digits.contains("__"),"tool_expression_unsupported");
        let clean=digits.replace('_',"");return Ok(u64::from_str_radix(&clean,radix)? as f64);
    }
    let bytes=literal.as_bytes();
    for (i,b) in bytes.iter().enumerate(){if *b==b'_'{ensure!(i>0 && i+1<bytes.len() && bytes[i-1].is_ascii_digit() && bytes[i+1].is_ascii_digit(),"tool_expression_unsupported");}}
    let clean=literal.replace('_',"");
    if !clean.contains(['.','e','E']) && clean.starts_with('0'){ensure!(clean.bytes().all(|b|b==b'0'),"tool_expression_unsupported");}
    Ok(clean.parse::<f64>()?)
}
// A bounded expression parser, never shell/eval. Python-compatible precedence and modulo.
pub fn calculate(s:&str)->Result<f64> {
    ensure!(s.chars().count()<=256,"tool_arguments_invalid");
    struct Parser<'a>{s:&'a [u8],i:usize,n:usize}
    impl Parser<'_>{
        fn ws(&mut self){while self.i<self.s.len() && self.s[self.i].is_ascii_whitespace(){self.i+=1;}}
        fn expr(&mut self,min:u8)->Result<f64>{self.ws();let mut left=match self.s.get(self.i).copied(){
            Some(b'+'|b'-')=>{self.n+=2;ensure!(self.n<=80,"tool_expression_too_complex");let sign=self.s[self.i];self.i+=1;let n=self.expr(3)?;if sign==b'-'{-n}else{n}},
            Some(b'(')=>{self.i+=1;let n=self.expr(0)?;self.ws();ensure!(self.s.get(self.i)==Some(&b')'),"tool_expression_unsupported");self.i+=1;n},
            _=>{self.n+=1;ensure!(self.n<=80,"tool_expression_too_complex");let start=self.i;while self.i<self.s.len(){let c=self.s[self.i];if c.is_ascii_alphanumeric() || c==b'.' || c==b'_' || ((c==b'+' || c==b'-') && self.i>start && matches!(self.s[self.i-1],b'e'|b'E')){self.i+=1;}else{break;}}ensure!(self.i>start,"tool_expression_unsupported");number(std::str::from_utf8(&self.s[start..self.i])?)?}
        };ensure!(left.is_finite() && left.abs()<=1e15,"tool_result_out_of_range");loop{self.ws();let op=self.s.get(self.i).copied().unwrap_or(0);let p=match op {b'+'|b'-'=>1,b'*'|b'/'|b'%'=>2,_=>0};if p==0 || p<min{break;}self.n+=2;ensure!(self.n<=80,"tool_expression_too_complex");self.i+=1;let right=self.expr(p+1)?;left=match op{b'+'=>left+right,b'-'=>left-right,b'*'=>left*right,b'/'=>left/right,b'%'=>left-right*(left/right).floor(),_=>unreachable!()};ensure!(left.is_finite() && left.abs()<=1e15,"tool_result_out_of_range");}Ok(left)}
    }
    let mut p=Parser{s:s.as_bytes(),i:0,n:1};let n=p.expr(0)?;p.ws();if p.i!=s.len(){bail!("tool_expression_unsupported");}Ok(n)
}
#[cfg(test)]mod tests{use super::*;#[test]fn arithmetic(){assert_eq!(calculate("2+3*4").unwrap(),14.);assert_eq!(calculate("0x_FF+0b10+1_000").unwrap(),1257.);assert!(calculate("01").is_err());assert!(calculate("1__2").is_err());assert_eq!(calculate("-5 % 3").unwrap(),1.);assert_eq!(calculate("-(2+3)").unwrap(),-5.);assert!(calculate("1/0").is_err());assert!(calculate("__import__('os')").is_err());assert!(calculate("2**10").is_err());}}

#[cfg(test)]
#[path="sdk_migration_parity_lineage.rs"]
mod sdk_migration_parity_lineage;
#[cfg(test)]
#[path="sdk_migration_parity_arguments.rs"]
mod sdk_migration_parity_arguments;
#[cfg(test)]
#[path="sdk_migration_parity_response.rs"]
mod sdk_migration_parity_response;
