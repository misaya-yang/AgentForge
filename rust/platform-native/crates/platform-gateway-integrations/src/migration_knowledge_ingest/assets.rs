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
    pub async fn delete_document_assets(&self, tenant: &str, document: &str) -> Result<()> {
        scope(tenant)?;
        scope(document)?;
        for prefix in [
            format!("knowledge/documents/{tenant}/{document}/"),
            format!("knowledge/confluence/{tenant}/{document}/images/"),
        ] {
            self.backend
                .delete_prefix(&prefix)
                .await
                .map_err(|_| IngestError::Storage)?;
        }
        Ok(())
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
