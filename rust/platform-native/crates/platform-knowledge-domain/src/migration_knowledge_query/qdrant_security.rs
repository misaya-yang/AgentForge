//! Role-specific TLS construction; inherited public QdrantStore API.
use super::*;
impl QdrantStore {
    pub fn is_read_only(&self) -> bool {
        self.role == QdrantRole::ApiReadOnly
    }

    /// Dedicated client: the CA path is operator configuration, never request input.
    /// Existing platform/system roots and hostname verification remain enabled.
    pub fn for_role(
        origin: Url,
        api_key: Option<String>,
        role: QdrantRole,
        ca_file: Option<&std::path::Path>,
    ) -> Result<Self> {
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10));
        if let Some(path) = ca_file {
            if !path.is_absolute() || origin.scheme() != "https" {
                return Err(QueryError::Validation(
                    "Qdrant CA requires an absolute controlled path and HTTPS".into(),
                ));
            }
            let bytes = std::fs::read(path)
                .map_err(|_| QueryError::Validation("cannot read Qdrant TLS CA".into()))?;
            let certificates = reqwest::Certificate::from_pem_bundle(&bytes)
                .map_err(|_| QueryError::Validation("invalid Qdrant TLS CA PEM".into()))?;
            if certificates.is_empty() {
                return Err(QueryError::Validation("empty Qdrant TLS CA bundle".into()));
            }
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }
        let client = builder.build().map_err(|_| QueryError::VectorStore)?;
        let mut store = Self::new(client, origin, api_key)?;
        store.role = role;
        store.custom_ca = ca_file.is_some();
        Ok(store)
    }
}
