//! Typed request/response equivalents for the Python Assistant SDK.
use serde::{Serialize,Deserialize};use serde_json::{Value,json};use crate::Error;
#[derive(Clone,Serialize,Deserialize)]pub struct ChatRequest{
 pub message:String,
 #[serde(skip_serializing_if="Option::is_none")]pub session_id:Option<String>,
 #[serde(skip_serializing_if="Option::is_none")]pub history:Option<Vec<Value>>,
 #[serde(skip_serializing_if="Option::is_none")]pub model_id:Option<String>,
 pub temperature:f64,
 #[serde(skip_serializing_if="Option::is_none")]pub max_tokens:Option<u64>,
 #[serde(skip_serializing_if="Option::is_none")]pub system_prompt:Option<String>,
 #[serde(skip_serializing_if="Option::is_none")]pub kb_dataset_ids:Option<Vec<String>>,
 #[serde(skip_serializing_if="Option::is_none")]pub kb_mode:Option<String>,
 #[serde(skip_serializing_if="Option::is_none")]pub kb_top_k:Option<u64>,
 #[serde(skip_serializing_if="Option::is_none")]pub kb_score_threshold:Option<f64>,
 #[serde(skip_serializing_if="Option::is_none")]pub kb_include_images:Option<bool>,
 pub web_search_enabled:bool,
 #[serde(skip_serializing_if="Option::is_none")]pub web_search_max_results:Option<u64>,
 #[serde(skip_serializing_if="Option::is_none")]pub file_paths:Option<Vec<String>>,
 #[serde(skip_serializing_if="Option::is_none")]pub execution_profile:Option<String>,
 #[serde(skip_serializing_if="Option::is_none")]pub memory_mode:Option<String>,
 #[serde(skip_serializing_if="Option::is_none")]pub os_agent_enabled:Option<bool>,pub stream:bool,
}
impl ChatRequest{pub fn new(message:impl Into<String>)->Self{Self{message:message.into(),session_id:None,history:None,model_id:None,temperature:0.7,max_tokens:None,system_prompt:None,kb_dataset_ids:None,kb_mode:None,kb_top_k:None,kb_score_threshold:None,kb_include_images:None,web_search_enabled:false,web_search_max_results:None,file_paths:None,execution_profile:None,memory_mode:None,os_agent_enabled:None,stream:true}}pub fn to_value(&self)->Result<Value,Error>{if !self.temperature.is_finite()||self.kb_score_threshold.is_some_and(|x|!x.is_finite()){return Err(Error::Request)}serde_json::to_value(self).map_err(|_|Error::Request)}}
#[derive(Clone,Serialize,Deserialize)]pub struct ImageRequest{pub prompt:String,#[serde(skip_serializing_if="Option::is_none")]pub model_id:Option<String>,#[serde(skip_serializing_if="Option::is_none")]pub style:Option<String>,#[serde(skip_serializing_if="Option::is_none")]pub size:Option<String>,pub n:u32}
impl ImageRequest{pub fn new(prompt:impl Into<String>)->Self{Self{prompt:prompt.into(),model_id:None,style:None,size:None,n:1}}}
#[derive(Default,Clone,Serialize,Deserialize)]pub struct SessionCreateRequest{pub title:Option<String>,pub metadata:Option<serde_json::Map<String,Value>>}
impl SessionCreateRequest{pub fn to_value(&self)->Value{let mut metadata=self.metadata.clone().unwrap_or_default();if let Some(title)=self.title.as_ref().filter(|s|!s.is_empty()){metadata.insert("title".into(),json!(title));}if metadata.is_empty(){json!({})}else{json!({"metadata":metadata})}}}
#[derive(Default,Clone,Serialize,Deserialize)]#[serde(default)]pub struct ChatResponse{pub content:String,pub session_id:String,pub model_id:Option<String>,pub run_id:Option<String>,pub usage:Option<Value>,pub duration_ms:Option<f64>,pub contexts:Option<Vec<Value>>}
#[derive(Default,Clone,Serialize,Deserialize)]#[serde(default)]pub struct Session{pub session_id:String,pub user_id:String,pub tenant_id:String,pub service_id:Option<String>,pub created_at:Option<String>,pub updated_at:Option<String>,pub metadata:Option<Value>,pub message_count:u64}
impl Session{pub fn title(&self)->Option<String>{self.metadata.as_ref()?.get("title").map(|v|v.as_str().map(str::to_owned).unwrap_or_else(||v.to_string()))}}
#[derive(Default,Clone,Serialize,Deserialize)]#[serde(default)]pub struct Message{pub role:String,pub content:String,pub timestamp:Option<String>,pub metadata:Option<Value>}
#[derive(Default,Clone,Serialize,Deserialize)]#[serde(default)]pub struct Artifact{pub artifact_id:String,pub session_id:String,#[serde(rename="type")]pub kind:String,pub format:String,pub title:String,pub filename:String,pub size_bytes:u64,pub mime_type:Option<String>,pub source:Option<String>,pub storage_key:Option<String>,pub message_id:Option<String>,pub tenant_id:Option<String>,pub user_id:Option<String>,pub metadata:Option<Value>,pub download_url:Option<String>,pub created_at:Option<String>,pub updated_at:Option<String>}
#[derive(Default,Clone,Serialize,Deserialize)]#[serde(default)]pub struct GeneratedImage{pub url:String,pub width:Option<u32>,pub height:Option<u32>}
#[derive(Default,Clone,Serialize,Deserialize)]#[serde(default)]pub struct ImageResult{pub success:bool,pub images:Vec<GeneratedImage>,pub provider:String,pub duration_ms:f64,pub error:Option<String>}
#[derive(Default,Clone,Serialize,Deserialize)]#[serde(default)]pub struct Dataset{pub dataset_id:String,pub name:String,pub description:Option<String>,pub document_count:u64,pub chunk_count:u64,pub embedding_model:Option<String>,pub is_multimodal:bool}
#[derive(Clone,Serialize,Deserialize)]#[serde(default)]pub struct ToolInfo{pub name:String,pub description:String,pub enabled:bool,pub category:Option<String>,pub parameters:Option<Value>}
impl Default for ToolInfo{fn default()->Self{Self{name:String::new(),description:String::new(),enabled:true,category:None,parameters:None}}}
#[derive(Default,Clone,Serialize,Deserialize)]#[serde(default)]pub struct ModelInfo{pub id:String,pub name:String,pub provider:String,pub context_window:u64,pub max_output_tokens:u64,pub supports_vision:bool,pub supports_tools:bool,pub access_level:Option<String>,pub input_price_per_1k:Option<f64>,pub output_price_per_1k:Option<f64>}
#[cfg(test)]mod tests{use super::*;#[test]fn request_defaults_and_session_metadata(){assert_eq!(ChatRequest::new("hello").to_value().unwrap(),json!({"message":"hello","temperature":0.7,"web_search_enabled":false,"stream":true}));let request=SessionCreateRequest{title:Some("title".into()),metadata:Some(serde_json::from_value(json!({"title":"old","x":1})).unwrap())};assert_eq!(request.to_value(),json!({"metadata":{"title":"title","x":1}}));let tool:ToolInfo=serde_json::from_value(json!({"name":"t","unknown":1})).unwrap();assert!(tool.enabled);}}
#[derive(Clone,Serialize,Deserialize)]pub struct CreateArtifactRequest{pub session_id:String,#[serde(rename="type")]pub kind:String,pub format:String,pub title:String,pub filename:String,pub content_base64:String,#[serde(skip_serializing_if="Option::is_none")]pub source:Option<String>,#[serde(skip_serializing_if="Option::is_none")]pub message_id:Option<String>,#[serde(skip_serializing_if="Option::is_none")]pub metadata:Option<Value>}
