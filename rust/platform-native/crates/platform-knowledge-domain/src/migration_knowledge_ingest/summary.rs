//! Source-compatible summary generation. Transport is explicitly configured;
//! provider failures use the same extractive fallbacks as the Python service.
use super::{IngestError, Result};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::sync::Semaphore;
#[derive(Clone)]
pub struct SummaryGenerator {
    pub provider: String,
    pub endpoint: String,
    pub model: String,
    pub authorization: Option<String>,
    pub timeout: Duration,
}
pub fn prefix(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}
impl SummaryGenerator {
    pub fn new(_endpoint:String,model:String,_authorization:Option<String>)->Result<Self> { Self::for_model("dashscope".into(),model) }
    pub fn for_model(provider:String,model:String)->Result<Self> {
        if provider.trim().is_empty() || model.trim().is_empty() {return Err(IngestError::Validation("summary model identity is required".into()));}
        Ok(Self{provider,model,endpoint:String::new(),authorization:None,timeout:Duration::from_secs(60)})
    }
    async fn call(&self,prompt:String)->Result<String> {
        use crate::migration_knowledge_query::model_plane::{self,ModelPurpose,ModelWire};
        let (status,result)=model_plane::execute(&self.provider,&self.model,"",ModelPurpose::Summary,ModelWire::ChatCompletions,json!({"model":self.model,"messages":[{"role":"user","content":prompt}],"max_tokens":500,"temperature":0.3}),self.timeout).await?;
        if !(200..300).contains(&status) {return Err(IngestError::Index("summary provider rejected request".into()));}
        Ok(result["choices"][0]["message"]["content"].as_str().unwrap_or("").into())
    }
    pub async fn summarize_document(&self, text: &str) -> Value {
        let text = if text.chars().count() > 15000 {
            format!(
                "{}\n...\n{}",
                prefix(text, 7500),
                text.chars()
                    .skip(text.chars().count() - 7500)
                    .collect::<String>()
            )
        } else {
            text.into()
        };
        let prompt = format!(
            "Analyze the following document and provide:\n1. A concise summary (2-3 sentences) capturing the main content and purpose\n2. 5-10 relevant keywords\n3. 2-3 main topics/themes\n\nDocument:\n{text}\n\nRespond in JSON format:\n{{\n    \"summary\": \"...\",\n    \"keywords\": [\"keyword1\", \"keyword2\", ...],\n    \"topics\": [\"topic1\", \"topic2\", ...]\n}}"
        );
        match self.call(prompt).await {
            Ok(s) => {
                let r = parse_json_response(&s);
                json!({"summary":r.get("summary").cloned().unwrap_or(json!("")),"keywords":r.get("keywords").cloned().unwrap_or(json!([])),"topics":r.get("topics").cloned().unwrap_or(json!([]))})
            }
            Err(_) => fallback_summary(&text),
        }
    }
    pub async fn summarize_section(&self, text: &str) -> String {
        let text = prefix(text, 4000);
        self.call(format!(
            "Summarize the following section in 1-2 sentences:\n\n{text}\n\nSummary:"
        ))
        .await
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| first_sentence(&text))
    }
    pub async fn extract_keywords(&self, text: &str, count: usize) -> Vec<String> {
        self.call(format!("Extract {count} important keywords from the following text.\nReturn only the keywords as a comma-separated list.\n\nText:\n{}\n\nKeywords:",prefix(text,15000))).await.map(|s|s.split(',').take(count).map(|s|s.trim().to_owned()).collect()).unwrap_or_default()
    }
}
pub fn parse_json_response(response: &str) -> Value {
    let mut text = response.trim();
    for fence in ["```json", "```"] {
        if let Some(start) = text.find(fence) {
            let s = &text[start + fence.len()..];
            text = s.split_once("```").map(|x| x.0).unwrap_or(s).trim();
            break;
        }
    }
    if let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) {
        if b >= a {
            text = &text[a..=b]
        }
    }
    serde_json::from_str(text)
        .ok()
        .filter(Value::is_object)
        .unwrap_or(json!({}))
}
pub fn fallback_summary(text: &str) -> Value {
    let mut summary = prefix(text, 500).trim().to_owned();
    if text.chars().count() > 500 {
        for sep in [". ", "。", "! ", "? "] {
            if let Some(i) = summary
                .rfind(sep)
                .filter(|i| summary[..*i].chars().count() > 100)
            {
                summary.truncate(i + sep.chars().next().unwrap().len_utf8());
                break;
            }
        }
    }
    let words = regex::Regex::new(r"\b[A-Z][a-z]+\b").unwrap();
    let sample = prefix(text, 5000);
    let unique: BTreeSet<_> = words
        .find_iter(&sample)
        .map(|m| m.as_str().to_owned())
        .collect();
    json!({"summary":summary,"keywords":unique.into_iter().take(10).collect::<Vec<_>>(),"topics":[]})
}
pub fn first_sentence(text: &str) -> String {
    let text = text.trim();
    for sep in [". ", "。", "! ", "? ", "\n"] {
        if let Some(i) = text.find(sep).filter(|i| {
            let n = text[..*i].chars().count();
            n > 10 && n < 200
        }) {
            return text[..i + sep.chars().next().unwrap().len_utf8()]
                .trim()
                .into();
        }
    }
    prefix(text, 200).trim().into()
}
#[derive(Clone)]
pub struct BatchSummaryGenerator {
    pub generator: SummaryGenerator,
    semaphore: Arc<Semaphore>,
    pub rate_limit_delay: Duration,
}
impl BatchSummaryGenerator {
    pub fn new(
        generator: SummaryGenerator,
        max_concurrent: usize,
        rate_limit_delay: Duration,
    ) -> Self {
        Self {
            generator,
            semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
            rate_limit_delay,
        }
    }
    pub async fn summarize_documents(&self, documents: &[Value]) -> Vec<Value> {
        let mut tasks = tokio::task::JoinSet::new();
        for (index, doc) in documents.iter().cloned().enumerate() {
            let this = self.clone();
            tasks.spawn(crate::migration_knowledge_query::model_plane::inherit(async move {
                let Ok(_permit) = this.semaphore.acquire().await else {
                    return (index, json!({"error":"summary batch closed"}));
                };
                let mut result = this
                    .generator
                    .summarize_document(doc["text"].as_str().unwrap_or(""))
                    .await;
                tokio::time::sleep(this.rate_limit_delay).await;
                result["id"] = doc["id"].clone();
                (index, result)
            }));
        }
        let mut results = vec![Value::Null; documents.len()];
        while let Some(Ok((i, v))) = tasks.join_next().await {
            results[i] = v
        }
        results
    }
}
