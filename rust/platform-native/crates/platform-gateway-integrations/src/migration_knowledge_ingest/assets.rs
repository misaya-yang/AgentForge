//! Knowledge object keys and signed local URLs over the shared canonical native
//! storage transport. S3 and OSS signing have one owner in migration_content.
use super::{IngestError, Result, parser::MAX_SOURCE_BYTES};
use crate::migration_content::storage::{
    CloudStorage, LocalStorage, ObjectStorage, OssHttpTransport, S3HttpTransport,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
#[derive(Clone)]
pub struct AssetStorage {
    pub backend: Arc<dyn ObjectStorage>,
    pub signing_key: Option<Arc<Vec<u8>>>,
    pub url_expiry_seconds: u32,
}
pub fn quote_key_component(value: &str) -> String {
    crate::migration_content::percent_encode(value)
}
fn scope(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(IngestError::Validation(
            "invalid storage scope identity".into(),
        ));
    }
    Ok(())
}
fn valid_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.starts_with('/')
        || key.contains('\\')
        || key
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || key.chars().any(char::is_control)
    {
        return Err(IngestError::Validation(
            "invalid logical storage key".into(),
        ));
    }
    Ok(())
}
impl AssetStorage {
    pub fn from_backend(backend: Arc<dyn ObjectStorage>) -> Self {
        Self {
            backend,
            signing_key: None,
            url_expiry_seconds: 3600,
        }
    }
    pub fn with_signing_key(mut self, key: Vec<u8>) -> Self {
        self.signing_key = Some(Arc::new(key));
        self
    }
    pub fn local(root: PathBuf, prefix: String) -> Result<Self> {
        if !root.is_absolute() {
            return Err(IngestError::Validation(
                "asset storage root must be absolute".into(),
            ));
        }
        if !prefix.is_empty() {
            valid_key(prefix.trim_matches('/'))?
        }
        Ok(Self::from_backend(Arc::new(LocalStorage::new(
            root,
            prefix.trim_matches('/').into(),
        ))))
    }
    pub fn original_key(tenant: &str, document: &str, filename: &str) -> Result<String> {
        scope(tenant)?;
        scope(document)?;
        let mut name = quote_key_component(filename);
        if name.is_empty() || name == "." {
            name = "document".into()
        }
        Ok(format!(
            "knowledge/documents/{tenant}/{document}/original/{name}"
        ))
    }
    pub fn image_key(
        tenant: &str,
        document: &str,
        attachment: &str,
        filename: &str,
    ) -> Result<String> {
        scope(tenant)?;
        scope(document)?;
        scope(attachment)?;
        let safe: String = filename
            .chars()
            .filter(|c| c.is_alphanumeric() || "._-".contains(*c))
            .collect();
        Ok(format!(
            "knowledge/confluence/{tenant}/{document}/images/{attachment}_{safe}"
        ))
    }
    pub async fn upload_original(
        &self,
        tenant: &str,
        document: &str,
        filename: &str,
        bytes: &[u8],
        mime: &str,
    ) -> Result<String> {
        let key = Self::original_key(tenant, document, filename)?;
        self.put(&key, bytes, mime).await?;
        Ok(key)
    }
    pub async fn put(&self, key: &str, bytes: &[u8], mime: &str) -> Result<()> {
        valid_key(key)?;
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(IngestError::Validation(
                "document source exceeds parser byte limit".into(),
            ));
        }
        self.backend
            .upload(
                key,
                bytes.to_vec(),
                mime,
                BTreeMap::from([("content_type".into(), mime.into())]),
            )
            .await
            .map_err(|_| IngestError::Storage)?;
        Ok(())
    }
    pub async fn download_original(&self, key: &str) -> Result<Vec<u8>> {
        self.download_bounded(key, MAX_SOURCE_BYTES).await
    }
    pub async fn download_bounded(&self, key: &str, limit: usize) -> Result<Vec<u8>> {
        valid_key(key)?;
        let head = self
            .backend
            .head(key)
            .await
            .map_err(super::reextraction::storage_error)?
            .ok_or(IngestError::NotFound)?;
        if head.size_bytes > limit as u64 {
            return Err(IngestError::Validation(
                "stored document exceeds byte budget".into(),
            ));
        }
        let bytes = self
            .backend
            .download_bounded(key, limit)
            .await
            .map_err(super::reextraction::storage_error)?;
        if bytes.len() > limit {
            return Err(IngestError::Validation(
                "stored document exceeds byte budget".into(),
            ));
        }
        Ok(bytes)
    }
    pub async fn exists(&self, key: &str) -> Result<bool> {
        valid_key(key)?;
        Ok(self
            .backend
            .head(key)
            .await
            .map_err(|_| IngestError::Storage)?
            .is_some())
    }
    pub async fn delete_key(&self, key: &str) -> Result<()> {
        valid_key(key)?;
        if !self.exists(key).await? {
            return Ok(());
        }
        if !self
            .backend
            .delete(key)
            .await
            .map_err(|_| IngestError::Storage)?
        {
            return Err(IngestError::Storage);
        }
        Ok(())
    }
    pub async fn download_document_image(
        &self,
        tenant: &str,
        document: &str,
        key: &str,
    ) -> Result<Vec<u8>> {
        let tenant = tenant.trim();
        let document = document.trim();
        scope(tenant)?;
        scope(document)?;
        let prefix = format!("knowledge/confluence/{tenant}/{document}/images/");
        if key.trim() != key
            || key.strip_prefix(&prefix).is_none_or(str::is_empty)
            || key.contains("..")
            || key.contains('\\')
            || key.contains('\0')
        {
            return Err(IngestError::Validation(
                "image storage key is outside the document asset scope".into(),
            ));
        }
        self.backend
            .download(key)
            .await
            .map_err(|_| IngestError::Storage)
    }
    pub async fn delete_document_assets(&self, tenant: &str, document: &str) -> Result<u64> {
        let tenant = tenant.trim();
        let document = document.trim();
        scope(tenant)?;
        scope(document)?;
        let mut deleted = 0;
        for prefix in [
            format!("knowledge/confluence/{tenant}/{document}/images/"),
            format!("knowledge/documents/{tenant}/{document}/original/"),
        ] {
            deleted += self
                .backend
                .delete_prefix(&prefix)
                .await
                .map_err(|_| IngestError::Storage)?;
        }
        Ok(deleted)
    }
    pub async fn image_url(&self, key: &str) -> Result<String> {
        valid_key(key)?;
        let url = self
            .backend
            .download_url(key, self.url_expiry_seconds, None)
            .await
            .map_err(|_| IngestError::Storage)?
            .ok_or(IngestError::Storage)?;
        self.sign_url_if_local(&url)
    }
    pub fn sign_url_if_local(&self, url: &str) -> Result<String> {
        if !url.starts_with("file://") {
            return Ok(url.into());
        }
        let key = self
            .signing_key
            .as_ref()
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                IngestError::Validation(
                    "local image URLs require a configured HMAC signing key".into(),
                )
            })?;
        let mut parsed = reqwest::Url::parse(url).map_err(|_| IngestError::Storage)?;
        let expires = chrono::Utc::now().timestamp() + self.url_expiry_seconds as i64;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(key.as_slice()).map_err(|_| IngestError::Storage)?;
        mac.update(format!("{}:{expires}", parsed.path()).as_bytes());
        let sig = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()[..32]
            .to_owned();
        parsed
            .query_pairs_mut()
            .append_pair("expires", &expires.to_string())
            .append_pair("sig", &sig);
        Ok(parsed.to_string())
    }
}
pub struct S3Assets;
impl S3Assets {
    pub fn new(
        endpoint: reqwest::Url,
        bucket: String,
        region: String,
        prefix: String,
        access_key: String,
        secret_key: String,
        session_token: Option<String>,
    ) -> Result<AssetStorage> {
        if session_token.as_ref().is_some_and(|t| !t.is_empty()) {
            return Err(IngestError::Validation(
                "S3 session-token transport is unavailable".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| IngestError::Storage)?;
        let transport = S3HttpTransport::new(
            client,
            region,
            access_key,
            secret_key.into_bytes(),
            Some(endpoint.to_string()),
        )
        .map_err(|_| IngestError::Storage)?;
        let base = format!(
            "{}/{}",
            endpoint.as_str().trim_end_matches('/'),
            quote_key_component(&bucket)
        );
        Ok(AssetStorage::from_backend(Arc::new(CloudStorage {
            transport: Arc::new(transport),
            bucket,
            prefix,
            public_base: base,
            s3: true,
        })))
    }
}
pub struct OssAssets;
impl OssAssets {
    pub fn new(
        endpoint: String,
        bucket: String,
        prefix: String,
        access_key: String,
        secret_key: String,
    ) -> Result<AssetStorage> {
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| IngestError::Storage)?;
        let transport =
            OssHttpTransport::new(client, &endpoint, access_key, secret_key.into_bytes())
                .map_err(|_| IngestError::Storage)?;
        let parsed = reqwest::Url::parse(&if endpoint.contains("://") {
            endpoint
        } else {
            format!("http://{endpoint}")
        })
        .map_err(|_| IngestError::Storage)?;
        let public_base = format!(
            "{}://{}.{}",
            parsed.scheme(),
            bucket,
            parsed.host_str().ok_or(IngestError::Storage)?
        );
        Ok(AssetStorage::from_backend(Arc::new(CloudStorage {
            transport: Arc::new(transport),
            bucket,
            prefix,
            public_base,
            s3: false,
        })))
    }
}

/// Archived Knowledge URL verifier. This compatibility utility has no mounted
/// route callers; an absent key preserves the source's unsigned-URL behavior.
/// It must not be used as a substitute for authenticated asset authorization.
pub fn verify_signed_url(url: &str, secret_key: &str) -> (bool, String) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    verify_signed_url_at(url, secret_key, now)
}
fn verify_signed_url_at(url: &str, secret_key: &str, now: f64) -> (bool, String) {
    let fail = |message: &str| (false, message.to_owned());
    if url.is_empty() {
        return fail("Empty URL");
    }
    if secret_key.is_empty() {
        tracing::warn!("URL signature verification skipped: no secret key configured");
        return (true, String::new());
    }
    // Preserve the encoded path, rather than URL-normalizing dot segments or
    // decoding percent escapes: Python urlparse signs the literal path.
    let fragmentless = url.split_once('#').map_or(url, |(head, _)| head);
    let (head, query) = fragmentless.split_once('?').unwrap_or((fragmentless, ""));
    let mut path = head;
    let mut scheme = "";
    if let Some((candidate, rest)) = head.split_once(':') {
        if candidate.starts_with(|c: char| c.is_ascii_alphabetic())
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            scheme = candidate;
            path = rest;
        }
    }
    if let Some(authority) = path.strip_prefix("//") {
        path = authority.find('/').map_or("", |index| &authority[index..]);
    }
    if [
        "", "ftp", "http", "gopher", "nntp", "rtsp", "rtsps", "rtspu", "sip", "sips", "mms",
        "sftp", "tel", "imap", "wais", "https", "shttp",
    ]
    .contains(&scheme.to_ascii_lowercase().as_str())
    {
        let last_component = path.rfind('/').map_or(0, |index| index + 1);
        if let Some(params) = path[last_component..].find(';') {
            path = &path[..last_component + params];
        }
    }
    // parse_qs keeps the first nonempty value and ignores empty values.
    let pairs: Vec<_> = reqwest::Url::parse(&format!("http://localhost/?{query}"))
        .map(|u| {
            u.query_pairs()
                .filter(|(_, v)| !v.is_empty())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let signature = pairs.iter().find(|(k, _)| k == "sig").map(|(_, v)| v);
    let expires = pairs.iter().find(|(k, _)| k == "expires").map(|(_, v)| v);
    let (Some(signature), Some(expires)) = (signature, expires) else {
        return fail("Missing signature or expiry");
    };
    let Ok(expires) = expires.trim().parse::<i64>() else {
        return fail("Invalid expiry format");
    };
    if now > expires as f64 {
        return fail("URL has expired");
    };
    // Source compares lowercase hex strings, so uppercase encodings must not
    // acquire acceptance merely because a hex decoder would accept them.
    if signature.len() != 32
        || !signature
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return fail("Invalid signature");
    }
    let mut supplied = [0u8; 16];
    for (index, pair) in signature.as_bytes().chunks_exact(2).enumerate() {
        let digit = |c: u8| {
            if c.is_ascii_digit() {
                c - b'0'
            } else {
                c - b'a' + 10
            }
        };
        supplied[index] = digit(pair[0]) * 16 + digit(pair[1]);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret_key.as_bytes())
        .expect("HMAC accepts every key length");
    mac.update(format!("{path}:{expires}").as_bytes());
    if mac.verify_truncated_left(&supplied).is_err() {
        return fail("Invalid signature");
    };
    (true, String::new())
}

#[cfg(test)]
mod signed_url_contract {
    use super::*;
    fn signed(path: &str, expires: i64) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test-key").unwrap();
        mac.update(format!("{path}:{expires}").as_bytes());
        let signature = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        format!("{path}?expires={expires}&sig={}", &signature[..32])
    }
    #[test]
    fn expiry_boundary_and_literal_relative_paths_match_source() {
        let url = signed("/owned/../image%20one.png", 100);
        assert_eq!(
            verify_signed_url_at(&url, "test-key", 100.0),
            (true, String::new())
        );
        assert_eq!(
            verify_signed_url_at(&url, "test-key", 100.001),
            (false, "URL has expired".into())
        );
        assert_eq!(
            verify_signed_url_at(
                &url.replace("expires=100", "expires=+100"),
                "test-key",
                99.0
            ),
            (true, String::new())
        );
        assert_eq!(
            verify_signed_url_at(
                &url.replace("expires=100", "expires=&expires=100"),
                "test-key",
                99.0
            ),
            (true, String::new())
        );
        let mut parsed = reqwest::Url::parse(&format!("http://localhost{url}")).unwrap();
        let sig = parsed
            .query_pairs()
            .find(|(k, _)| k == "sig")
            .unwrap()
            .1
            .to_uppercase();
        parsed
            .query_pairs_mut()
            .clear()
            .append_pair("expires", "100")
            .append_pair("sig", &sig);
        assert_eq!(
            verify_signed_url_at(parsed.as_str(), "test-key", 99.0),
            (false, "Invalid signature".into())
        );
    }
}
