//! Native extension for the already configured Google Drive OAuth provider.
use super::*;

impl CanonicalConnectorSearchProviders {
    pub(super) async fn google_drive(
        &self,
        token: &str,
        query: &str,
        limit: &ConnectorSearchLimit,
    ) -> Result<Value, ConnectorError> {
        let limit = limit
            .as_i64()
            .filter(|n| (1..=100).contains(n))
            .ok_or_else(|| {
                ConnectorError::detail(422, "Google Drive search limit must be between 1 and 100")
            })? as usize;
        if query.trim().is_empty() || query.chars().count() > 4096 {
            return Err(ConnectorError::detail(
                422,
                "Google Drive search query must contain 1 to 4096 characters",
            ));
        }
        // Escape Drive's query language before the HTTP adapter URL-encodes q.
        // Callers supply plain text, never an executable Drive query fragment.
        let escaped = query.replace('\\', "\\\\").replace('\'', "\\'");
        let q = format!("trashed = false and fullText contains '{escaped}'");
        let mut page: Option<String> = None;
        let mut seen_pages = std::collections::HashSet::new();
        let mut seen_files = std::collections::HashSet::new();
        let mut results = Vec::new();
        // Drive may return partial or empty pages before the final page. Follow
        // its opaque cursor, with bounded work and no false complete result.
        for _ in 0..10 {
            let mut params = vec![
                ("q".into(), q.clone()),
                ("pageSize".into(), (limit - results.len()).to_string()),
                ("spaces".into(), "drive".into()),
                ("corpora".into(), "user".into()),
                ("includeItemsFromAllDrives".into(), "true".into()),
                ("supportsAllDrives".into(), "true".into()),
                ("orderBy".into(), "modifiedTime desc".into()),
                ("fields".into(), "nextPageToken,incompleteSearch,files(id,name,mimeType,webViewLink,description,modifiedTime,size)".into()),
            ];
            if let Some(page) = &page {
                params.push(("pageToken".into(), page.clone()));
            }
            let data = self
                .get_json(
                    "https://www.googleapis.com/drive/v3/files".into(),
                    params,
                    token,
                    Some("application/json"),
                    "Google Drive search failed",
                )
                .await?;
            if !data.is_object() || data.get("error").is_some() {
                return Err(ConnectorError::detail(
                    502,
                    "Google Drive search response invalid",
                ));
            }
            if data
                .get("incompleteSearch")
                .is_some_and(|v| v != &Value::Bool(false))
            {
                return Err(ConnectorError::detail(
                    502,
                    "Google Drive search incomplete; narrow the query",
                ));
            }
            let files = data.get("files").and_then(Value::as_array).ok_or_else(|| {
                ConnectorError::detail(502, "Google Drive search response invalid")
            })?;
            for file in files {
                let id = file
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        ConnectorError::detail(502, "Google Drive file identity missing")
                    })?;
                let name = file
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ConnectorError::detail(502, "Google Drive file name missing"))?;
                if !seen_files.insert(id.to_owned()) {
                    continue;
                }
                let text = |key: &str| file.get(key).and_then(Value::as_str).unwrap_or("");
                results.push(json!({
                    "id":id,"title":name,"type":"google_drive_file",
                    "url":text("webViewLink"),"mime_type":text("mimeType"),
                    "date":text("modifiedTime"),"size":text("size"),
                    "excerpt":prefix_codepoints(text("description"), 500),
                }));
                if results.len() == limit {
                    return Ok(Value::Array(results));
                }
            }
            match data.get("nextPageToken") {
                None | Some(Value::Null) => return Ok(Value::Array(results)),
                Some(Value::String(next)) if next.is_empty() => return Ok(Value::Array(results)),
                Some(Value::String(next))
                    if next.len() <= 8192 && seen_pages.insert(next.clone()) =>
                {
                    page = Some(next.clone());
                }
                _ => {
                    return Err(ConnectorError::detail(
                        502,
                        "Google Drive search cursor invalid",
                    ));
                }
            }
        }
        Err(ConnectorError::detail(
            502,
            "Google Drive search pagination limit reached; narrow the query",
        ))
    }
}
