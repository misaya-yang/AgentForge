use super::credentials::Credentials;
use super::*;
use std::{collections::BTreeSet, time::Duration};
pub fn cloud_domain(domain: &str) -> bool {
    let Some(label) = domain.strip_suffix(".atlassian.net") else {
        return false;
    };
    !label.is_empty()
        && label.len() <= 63
        && label.as_bytes()[0].is_ascii_alphanumeric()
        && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
pub fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}
impl Sources {
    pub async fn verify(&self, domain: &str, email: &str, token: &str) -> Result<()> {
        if !cloud_domain(domain) {
            return Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid Atlassian Cloud hostname",
            ));
        }
        let failure = || {
            Error::new(
                StatusCode::BAD_GATEWAY,
                "Confluence Cloud verification failed; check hostname, account email and API token",
            )
        };
        let response = self
            .client
            .get(format!("https://{domain}/wiki/rest/api/user/current"))
            .basic_auth(email, Some(token))
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|_| failure())?;
        if response.status() != StatusCode::OK {
            return Err(failure());
        }
        let identity: Value = response.json().await.map_err(|_| failure())?;
        if identity["type"] != "known" || text(&identity, "accountId").is_empty() {
            return Err(failure());
        }
        Ok(())
    }
    pub async fn provider(
        &self,
        credentials: &Credentials,
        path: &[&str],
        params: &[(&str, &str)],
    ) -> Result<Value> {
        if !cloud_domain(&credentials.domain) {
            return Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Unsupported Confluence hostname",
            ));
        }
        let mut url = reqwest::Url::parse(&format!("https://{}/wiki/api/v2/", credentials.domain))
            .map_err(|_| Error::unavailable("Confluence origin unavailable"))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| Error::unavailable("Confluence origin unavailable"))?;
            segments.pop_if_empty();
            for part in path {
                segments.push(part);
            }
        }
        let failure = || {
            Error::new(
                StatusCode::BAD_GATEWAY,
                "Confluence provider request failed",
            )
        };
        let mut accepted = None;
        for attempt in 0..self.max_retries.max(1) {
            self.throttle(&credentials.domain).await?;
            let response = self
                .client
                .get(url.clone())
                .query(params)
                .basic_auth(&credentials.email, Some(&credentials.token))
                .send()
                .await;
            match response {
                Ok(response) if response.status() == StatusCode::TOO_MANY_REQUESTS => {
                    let delay = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(5);
                    tokio::time::sleep(Duration::from_secs(delay.min(30))).await;
                }
                Ok(response)
                    if response.status().is_server_error() && attempt + 1 < self.max_retries =>
                {
                    tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                }
                Ok(response) if response.status() == StatusCode::NOT_FOUND => {
                    return Err(Error::new(
                        StatusCode::BAD_GATEWAY,
                        "Confluence provider resource not found",
                    ));
                }
                Ok(response) if response.status().is_success() => {
                    accepted = Some(response);
                    break;
                }
                Ok(_) => return Err(failure()),
                Err(_) if attempt + 1 < self.max_retries => {
                    tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                }
                Err(_) => return Err(failure()),
            }
        }
        let mut response = accepted.ok_or_else(failure)?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| failure())? {
            if bytes.len() + chunk.len() > 32 * 1024 * 1024 {
                return Err(failure());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| failure())
    }
    pub async fn page(&self, credentials: &Credentials, id: &str) -> Result<Value> {
        self.provider(credentials, &["pages", id], &[("body-format", "storage")])
            .await
    }
    pub async fn children(&self, credentials: &Credentials, id: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut cursor = String::new();
        let mut seen = BTreeSet::new();
        loop {
            let mut params = vec![("limit", "100")];
            if !cursor.is_empty() {
                params.push(("cursor", &cursor));
            }
            let data = self
                .provider(credentials, &["pages", id, "children"], &params)
                .await?;
            let rows = data["results"].as_array().ok_or_else(|| {
                Error::new(
                    StatusCode::BAD_GATEWAY,
                    "Invalid Confluence child discovery response",
                )
            })?;
            for row in rows {
                let id = scalar(&row["id"]);
                if id.trim().is_empty() {
                    return Err(Error::new(
                        StatusCode::BAD_GATEWAY,
                        "Invalid Confluence child discovery response",
                    ));
                }
                out.push(id);
            }
            let next = &data["_links"]["next"];
            if next.is_null() || next == "" {
                break;
            }
            let next = next.as_str().ok_or_else(|| {
                Error::new(
                    StatusCode::BAD_GATEWAY,
                    "Invalid Confluence child pagination response",
                )
            })?;
            let next = reqwest::Url::parse("https://pagination.invalid/")
                .unwrap()
                .join(next)
                .map_err(|_| {
                    Error::new(
                        StatusCode::BAD_GATEWAY,
                        "Invalid Confluence child pagination response",
                    )
                })?;
            cursor = next
                .query_pairs()
                .find(|(k, _)| k == "cursor")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            if cursor.is_empty() || !seen.insert(cursor.clone()) {
                return Err(Error::new(
                    StatusCode::BAD_GATEWAY,
                    "Incomplete Confluence child pagination",
                ));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn onboarding_accepts_only_exact_cloud_hostnames() {
        for valid in ["a.atlassian.net", "company-42.atlassian.net"] {
            assert!(cloud_domain(valid));
        }
        for invalid in [
            "https://a.atlassian.net",
            "a.atlassian.net.evil.invalid",
            "a.atlassian.net:443",
            "a.b.atlassian.net",
            "-a.atlassian.net",
            "a-.atlassian.net",
            "A.atlassian.net",
            "localhost",
            "a.atlassian.net/",
            "a@b.atlassian.net",
        ] {
            assert!(!cloud_domain(invalid), "{invalid}");
        }
    }
    #[test]
    fn remote_numeric_identifiers_preserve_identity() {
        assert_eq!(scalar(&json!(12345)), "12345");
        assert_eq!(scalar(&json!("00123")), "00123");
        assert_eq!(scalar(&Value::Null), "");
    }
}
