//! Explicit configured object storage. Credentials stay server-side; constructing
//! a backend performs no file creation, bucket operation or network request.
use super::BootstrapError;
use crate::migration_content::storage::{
    CloudStorage, LocalStorage, ObjectStorage, OssHttpTransport, S3HttpTransport,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
pub enum ObjectStorageConfiguration {
    Local {
        root: PathBuf,
        prefix: String,
    },
    S3 {
        region: String,
        access_key: String,
        secret_key: Vec<u8>,
        endpoint: Option<String>,
        bucket: String,
        prefix: String,
        public_base: String,
        encryption: String,
        kms_key: Option<String>,
    },
    Oss {
        endpoint: String,
        access_key: String,
        secret_key: Vec<u8>,
        bucket: String,
        prefix: String,
        public_base: String,
    },
}
impl ObjectStorageConfiguration {
    pub fn build(self) -> Result<Arc<dyn ObjectStorage>, BootstrapError> {
        if let Self::Local { root, prefix } = self {
            return Ok(Arc::new(LocalStorage::new(root, prefix)));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|_| BootstrapError::Construction("storage_http"))?;
        match self {
            Self::S3 {
                region,
                access_key,
                secret_key,
                endpoint,
                bucket,
                prefix,
                public_base,
                encryption,
                kms_key,
            } => {
                if bucket.trim().is_empty()
                    || region.trim().is_empty()
                    || access_key.trim().is_empty()
                    || secret_key.is_empty()
                {
                    return Err(BootstrapError::Configuration("s3_storage_configuration"));
                }
                Ok(Arc::new(CloudStorage {
                    transport: Arc::new(
                        S3HttpTransport::new(client, region, access_key, secret_key, endpoint)
                            .and_then(|transport| {
                                transport.with_encryption(&encryption, kms_key.as_deref())
                            })
                            .map_err(|_| BootstrapError::Configuration("s3_storage"))?,
                    ),
                    bucket,
                    prefix,
                    public_base,
                    s3: true,
                }))
            }
            Self::Oss {
                endpoint,
                access_key,
                secret_key,
                bucket,
                prefix,
                public_base,
            } => Ok(Arc::new(CloudStorage {
                transport: Arc::new(
                    OssHttpTransport::new(client, &endpoint, access_key, secret_key)
                        .map_err(|_| BootstrapError::Configuration("oss_storage"))?,
                ),
                bucket,
                prefix,
                public_base,
                s3: false,
            })),
            Self::Local { .. } => unreachable!("handled local storage above"),
        }
    }
}
