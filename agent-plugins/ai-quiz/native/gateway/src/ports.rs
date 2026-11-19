//! Host authority ports. Plugin code never resolves identity or Knowledge access itself.
use axum::http::HeaderMap;
use serde_json::{Value, json};
use std::{collections::HashSet, future::Future, pin::Pin};
pub type QuizFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, QuizError>> + Send + 'a>>;
#[derive(Clone, Debug, PartialEq)]
pub enum QuizError {
    Http(u16, Value),
    Internal,
    Cancelled,
}
impl QuizError {
    pub fn detail(status: u16, detail: impl Into<Value>) -> Self {
        Self::Http(status, json!({"detail":detail.into()}))
    }
    pub fn validation(field: &str, typ: &str, message: &str, input: Value) -> Self {
        Self::detail(
            422,
            json!([{"type":typ,"loc":["body",field],"msg":message,"input":input}]),
        )
    }
}
#[derive(Clone, Default)]
pub struct QuizIdentity {
    pub user_id: String,
    pub tenant_id: Option<String>,
    pub is_authenticated: bool,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub tier: String,
}
pub trait QuizIdentityResolver: Send + Sync {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap) -> QuizFuture<'a, QuizIdentity>;
}
#[derive(Clone, Debug, Default)]
pub struct SourceReferences {
    pub datasets: HashSet<String>,
    pub documents: HashSet<(String, String)>,
    pub versions: HashSet<(String, String, i64, String)>,
}
pub trait QuizSourceAuthority: Send + Sync {
    fn quiz_sources_with_origin<'a>(
        &'a self,
        user: &'a QuizIdentity,
        id: &'a str,
        public: bool,
    ) -> QuizFuture<'a, SourceReferences>;
    fn require_active<'a>(&'a self, user: &'a QuizIdentity) -> QuizFuture<'a, ()>;
    fn allowed<'a>(
        &'a self,
        user: &'a QuizIdentity,
        sources: &'a SourceReferences,
    ) -> QuizFuture<'a, bool>;
}
pub fn parse_uuid(text: &str, location: &str, name: &str) -> Result<String, QuizError> {
    if let Ok(id) = uuid::Uuid::parse_str(text) {
        return Ok(id.to_string());
    }
    let raw = text.strip_prefix("urn:uuid:").unwrap_or(text);
    let raw = raw
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(raw);
    let error = if raw.len() != 32 && !raw.contains('-') {
        format!(
            "invalid length: expected length 32 for simple format, found {}",
            raw.len()
        )
    } else if let Some((i, c)) = raw
        .chars()
        .enumerate()
        .find(|(_, c)| !c.is_ascii_hexdigit() && *c != '-')
    {
        format!("invalid character: found `{c}` at {}", i + 1)
    } else {
        let groups = raw.split('-').collect::<Vec<_>>();
        if groups.len() != 5 {
            format!("invalid group count: expected 5, found {}", groups.len())
        } else {
            let expected = [8, 4, 4, 4, 12];
            let (i, g) = groups
                .iter()
                .enumerate()
                .find(|(i, g)| g.len() != expected[*i])
                .unwrap_or((0, &""));
            format!(
                "invalid group length in group {}: expected {}, found {}",
                i,
                expected[i],
                g.len()
            )
        }
    };
    Err(QuizError::detail(
        422,
        json!([{"type":"uuid_parsing","loc":[location,name],"msg":format!("Input should be a valid UUID, {error}"),"input":text,"ctx":{"error":error}}]),
    ))
}
pub fn freeze_source_scope(s: &SourceReferences) -> Result<Value, QuizError> {
    let documents = s
        .versions
        .iter()
        .map(|(d, id, _, _)| (d.clone(), id.clone()))
        .collect::<HashSet<_>>();
    let datasets = s
        .versions
        .iter()
        .map(|(d, _, _, _)| d.clone())
        .collect::<HashSet<_>>();
    if s.datasets.iter().any(String::is_empty)
        || s.documents
            .iter()
            .any(|(d, id)| d.is_empty() || id.is_empty())
        || s.versions.iter().any(|(d, id, v, h)| {
            d.is_empty()
                || id.is_empty()
                || *v <= 0
                || h.len() != 64
                || !h
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        || s.documents != documents
        || !s.documents.iter().all(|(d, _)| s.datasets.contains(d))
        || (!s.datasets.is_empty() && s.datasets != datasets)
    {
        return Err(QuizError::detail(
            409,
            "Share sources lack a verifiable immutable version",
        ));
    }
    let mut datasets = s.datasets.iter().collect::<Vec<_>>();
    datasets.sort();
    let mut documents = s.documents.iter().collect::<Vec<_>>();
    documents.sort();
    let mut versions = s.versions.iter().collect::<Vec<_>>();
    versions.sort();
    Ok(
        json!({"version":1,"dataset_ids":datasets,"document_ids":documents,"source_versions":versions}),
    )
}
