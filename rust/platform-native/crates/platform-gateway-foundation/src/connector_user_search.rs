//! Source-faithful REST searches from `src/api/v1/connectors.py`.
//!
//! This layer hands literal Python-style path interpolation to the HTTP adapter.
//! URL parsing/percent encoding, DNS, TLS, timeout and redirect policy belong to
//! that adapter; request capture here is deliberately not network-wire evidence.
//! Provider data errors map to the source's unhandled 500 boundary without copying
//! response bodies, bearer tokens or provider diagnostics into an error.
mod google_drive;

use crate::connector_user_types::{
    ConnectorError, ConnectorFuture, ConnectorGetRequest, ConnectorProviderHttp,
    ConnectorSearchLimit, ConnectorSearchProviders,
};
use crate::{
    service_registry::python_truthy as truthy,
    usage_parser::python_value_to_string as python_string,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

pub struct CanonicalConnectorSearchProviders {
    http: Arc<dyn ConnectorProviderHttp>,
}

impl CanonicalConnectorSearchProviders {
    /// Construction is inert. Each request future owns no detached provider work.
    pub fn new(http: Arc<dyn ConnectorProviderHttp>) -> Self {
        Self { http }
    }

    async fn get_json(
        &self,
        url: String,
        query: Vec<(String, String)>,
        token: &str,
        accept: Option<&str>,
        failure: &'static str,
    ) -> Result<Value, ConnectorError> {
        let mut headers = vec![("Authorization".into(), format!("Bearer {token}"))];
        if let Some(accept) = accept {
            headers.push(("Accept".into(), accept.into()));
        }
        let response = self
            .http
            .get(ConnectorGetRequest {
                url,
                query,
                headers,
            })
            .await
            .map_err(|_| ConnectorError::source())?;
        if response.status != 200 {
            return Err(ConnectorError::detail(response.status, failure));
        }
        response.json()
    }

    async fn confluence(
        &self,
        token: &str,
        metadata: &Value,
        query: &str,
        limit: &ConnectorSearchLimit,
    ) -> Result<Value, ConnectorError> {
        let cloud_id = optional(metadata, "cloud_id", Value::String(String::new()))?;
        if !truthy(&cloud_id) {
            return Err(ConnectorError::detail(
                400,
                "Confluence cloud_id not found. Reconnect.",
            ));
        }
        let escaped = query.replace('\\', "\\\\").replace('"', "\\\"");
        let data = self
            .get_json(
                format!(
                    "https://api.atlassian.com/ex/confluence/{}/wiki/rest/api/content/search",
                    python_string(&cloud_id)
                ),
                vec![
                    ("cql".into(), format!("type=page AND text~\"{escaped}\"")),
                    ("limit".into(), limit.as_str().to_owned()),
                    ("expand".into(), "body.view,space".into()),
                ],
                token,
                None,
                "Confluence search failed",
            )
            .await?;
        let pages = optional(&data, "results", json!([]))?;
        let mut results = Vec::new();
        for page in iterable_records(&pages)? {
            let body = optional(page, "body", json!({}))?;
            let view = optional(&body, "view", json!({}))?;
            let html = optional(&view, "value", json!(""))?;
            let html = html.as_str().ok_or_else(ConnectorError::source)?;
            let excerpt = prefix_codepoints(&strip_html_tags(html), 500);
            let space = optional(page, "space", json!({}))?;
            let links = optional(page, "_links", json!({}))?;
            let base = optional(&links, "base", json!(""))?;
            let webui = optional(&links, "webui", json!(""))?;
            results.push(json!({
                "id": required(page, "id")?,
                "title": required(page, "title")?,
                "space": optional(&space, "name", json!(""))?,
                "url": format!("{}{}", python_string(&base), python_string(&webui)),
                "excerpt": excerpt,
                "type": "confluence_page",
            }));
        }
        Ok(Value::Array(results))
    }

    async fn outlook(
        &self,
        token: &str,
        query: &str,
        limit: &ConnectorSearchLimit,
    ) -> Result<Value, ConnectorError> {
        let data = self
            .get_json(
                "https://graph.microsoft.com/v1.0/me/messages".into(),
                vec![
                    ("$search".into(), format!("\"{query}\"")),
                    ("$top".into(), limit.as_str().to_owned()),
                    (
                        "$select".into(),
                        "subject,from,receivedDateTime,bodyPreview".into(),
                    ),
                ],
                token,
                None,
                "Outlook search failed",
            )
            .await?;
        let messages = optional(&data, "value", json!([]))?;
        let mut results = Vec::new();
        // The source does not slice Outlook results by the requested limit.
        for message in iterable_records(&messages)? {
            let from = optional(message, "from", json!({}))?;
            let address = optional(&from, "emailAddress", json!({}))?;
            let excerpt = optional(message, "bodyPreview", json!(""))?;
            results.push(json!({
                "id": required(message, "id")?,
                "title": optional(message, "subject", json!(""))?,
                "from": optional(&address, "address", json!(""))?,
                "date": optional(message, "receivedDateTime", json!(""))?,
                "excerpt": python_slice(&excerpt, &ConnectorSearchLimit::from(300))?,
                "type": "email",
            }));
        }
        Ok(Value::Array(results))
    }

    async fn github(
        &self,
        token: &str,
        query: &str,
        limit: &ConnectorSearchLimit,
    ) -> Result<Value, ConnectorError> {
        let data = self
            .get_json(
                "https://api.github.com/search/code".into(),
                vec![
                    ("q".into(), query.into()),
                    ("per_page".into(), limit.as_str().to_owned()),
                ],
                token,
                Some("application/vnd.github+json"),
                "GitHub search failed",
            )
            .await?;
        let items = python_slice(&optional(&data, "items", json!([]))?, limit)?;
        let mut results = Vec::new();
        for item in iterable_records(&items)? {
            let repository = optional(item, "repository", json!({}))?;
            results.push(json!({
                "id": optional(item, "sha", json!(""))?,
                "title": optional(item, "name", json!(""))?,
                "path": optional(item, "path", json!(""))?,
                "repo": optional(&repository, "full_name", json!(""))?,
                "url": optional(item, "html_url", json!(""))?,
                "type": "code",
            }));
        }
        Ok(Value::Array(results))
    }

    async fn gmail(
        &self,
        token: &str,
        query: &str,
        limit: &ConnectorSearchLimit,
    ) -> Result<Value, ConnectorError> {
        let data = self
            .get_json(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages".into(),
                vec![
                    ("q".into(), query.into()),
                    ("maxResults".into(), limit.as_str().to_owned()),
                ],
                token,
                None,
                "Gmail search failed",
            )
            .await?;
        let messages = python_slice(&optional(&data, "messages", json!([]))?, limit)?;
        let mut results = Vec::new();
        // Deliberately sequential. A detail failure is skipped only for HTTP
        // non-200; malformed successful JSON and transport errors abort search.
        for message in iterable_records(&messages)? {
            let id = required(message, "id")?;
            let response = self
                .http
                .get(ConnectorGetRequest {
                    url: format!(
                        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}",
                        python_string(&id)
                    ),
                    query: vec![
                        ("format".into(), "metadata".into()),
                        ("metadataHeaders".into(), "Subject,From,Date".into()),
                    ],
                    headers: vec![("Authorization".into(), format!("Bearer {token}"))],
                })
                .await
                .map_err(|_| ConnectorError::source())?;
            if response.status != 200 {
                continue;
            }
            let detail = response.json()?;
            let payload = optional(&detail, "payload", json!({}))?;
            let raw_headers = optional(&payload, "headers", json!([]))?;
            let mut headers = Map::new();
            for header in iterable_records(&raw_headers)? {
                let name = required(header, "name")?;
                let value = required(header, "value")?;
                match name {
                    Value::String(name) => {
                        headers.insert(name, value);
                    }
                    // Python permits scalar dictionary keys, but these cannot
                    // match the three exact, case-sensitive string lookups.
                    Value::Null | Value::Bool(_) | Value::Number(_) => {}
                    _ => return Err(ConnectorError::source()),
                }
            }
            results.push(json!({
                "id": id,
                "title": headers.get("Subject").cloned().unwrap_or(json!("")),
                "from": headers.get("From").cloned().unwrap_or(json!("")),
                "date": headers.get("Date").cloned().unwrap_or(json!("")),
                "excerpt": optional(&detail, "snippet", json!(""))?,
                "type": "email",
            }));
        }
        Ok(Value::Array(results))
    }
}

impl ConnectorSearchProviders for CanonicalConnectorSearchProviders {
    fn search<'a>(
        &'a self,
        provider: &'a str,
        token: &'a str,
        metadata: &'a Value,
        query: &'a str,
        limit: i64,
    ) -> ConnectorFuture<'a, Value> {
        Box::pin(async move {
            let limit = ConnectorSearchLimit::from(limit);
            self.search_exact(provider, token, metadata, query, &limit)
                .await
        })
    }

    fn search_exact<'a>(
        &'a self,
        provider: &'a str,
        token: &'a str,
        metadata: &'a Value,
        query: &'a str,
        limit: &'a ConnectorSearchLimit,
    ) -> ConnectorFuture<'a, Value> {
        Box::pin(async move {
            match provider {
                "confluence" => self.confluence(token, metadata, query, limit).await,
                "outlook" => self.outlook(token, query, limit).await,
                "github" => self.github(token, query, limit).await,
                "gmail" => self.gmail(token, query, limit).await,
                "google_drive" => self.google_drive(token, query, limit).await,
                _ => Err(ConnectorError::detail(
                    400,
                    format!("Search not supported for {provider}"),
                )),
            }
        })
    }
}

fn optional(value: &Value, key: &str, default: Value) -> Result<Value, ConnectorError> {
    let object = value.as_object().ok_or_else(ConnectorError::source)?;
    Ok(object.get(key).cloned().unwrap_or(default))
}

fn required(value: &Value, key: &str) -> Result<Value, ConnectorError> {
    value
        .as_object()
        .and_then(|object| object.get(key))
        .cloned()
        .ok_or_else(ConnectorError::source)
}

fn python_slice(value: &Value, limit: &ConnectorSearchLimit) -> Result<Value, ConnectorError> {
    match value {
        Value::Array(items) => Ok(Value::Array(items[..limit.slice_end(items.len())].to_vec())),
        Value::String(value) => Ok(Value::String(prefix_codepoints(
            value,
            limit.slice_end(value.chars().count()),
        ))),
        _ => Err(ConnectorError::source()),
    }
}

fn iterable_records(value: &Value) -> Result<&[Value], ConnectorError> {
    match value {
        Value::Array(items) => Ok(items),
        // Empty dictionaries and strings execute zero loop iterations in Python.
        Value::Object(items) if items.is_empty() => Ok(&[]),
        Value::String(value) if value.is_empty() => Ok(&[]),
        _ => Err(ConnectorError::source()),
    }
}

fn prefix_codepoints(value: &str, count: usize) -> String {
    value.chars().take(count).collect()
}

/// Equivalent to the source's `re.sub(r"<[^>]+>", "", html)`; no HTML entity
/// decoding, DOM normalization, whitespace folding or script removal is added.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut remaining = html;
    while let Some(start) = remaining.find('<') {
        result.push_str(&remaining[..start]);
        let after_open = &remaining[start + 1..];
        match after_open.find('>') {
            Some(end) if end > 0 => remaining = &after_open[end + 1..],
            _ => {
                result.push('<');
                remaining = after_open;
            }
        }
    }
    result.push_str(remaining);
    result
}
