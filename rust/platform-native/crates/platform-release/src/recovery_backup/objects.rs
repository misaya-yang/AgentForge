use super::*;
use ai_platform_service_primitives::content::storage::{CloudObjectTransport, S3HttpTransport};
use std::collections::BTreeMap;
use tokio::io::AsyncWriteExt;
pub(super) struct Store {
    pub client: reqwest::Client,
    pub source: S3HttpTransport,
    pub replica: S3HttpTransport,
    pub drill: S3HttpTransport,
}
impl Store {
    pub fn new(c: &Config) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .timeout(c.timeout())
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if let Some(path) = c.v["s3_ca_certificate_file"].as_str() {
            let pem = super::secret(Path::new(path), &c.repo)?;
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(pem.as_bytes()).map_err(|_| "storage CA invalid")?,
            );
        }
        let client = builder.build().map_err(|_| "HTTP transport failed")?;
        let access = c.secret("s3_access_key_file")?;
        let secret = c.secret("s3_secret_key_file")?;
        let transport = |key| {
            S3HttpTransport::new(
                client.clone(),
                c.value("s3_region")?.into(),
                access.clone(),
                secret.as_bytes().to_vec(),
                Some(c.value(key)?.into()),
            )
            .map_err(|_| "S3 transport invalid".to_string())
        };
        Ok(Self {
            source: transport("s3_source")?,
            replica: transport("s3_replica")?,
            drill: transport("s3_drill")?,
            client,
        })
    }
    pub async fn request(
        &self,
        t: &S3HttpTransport,
        bucket: &str,
        key: &str,
        method: &str,
        query: BTreeMap<String, String>,
        headers: BTreeMap<String, String>,
        body: Option<reqwest::Body>,
    ) -> Result<reqwest::Response> {
        let url = t
            .presign(bucket, key, method, 300, headers.clone(), query)
            .await
            .map_err(|_| "S3 signing failed")?;
        let mut request = self.client.request(
            reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "invalid S3 method")?,
            url,
        );
        for (k, v) in headers {
            request = request.header(k, v);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        request
            .send()
            .await
            .map_err(|_| "S3 request failed")?
            .error_for_status()
            .map_err(|_| "S3 operation refused".into())
    }
    async fn control(&self, t: &S3HttpTransport, bucket: &str, key: &str) -> Result<String> {
        let mut r = self
            .request(
                t,
                bucket,
                "",
                "GET",
                BTreeMap::from([(key.into(), String::new())]),
                BTreeMap::new(),
                None,
            )
            .await?;
        let mut bytes = vec![];
        while let Some(chunk) = r.chunk().await.map_err(|_| "S3 control read failed")? {
            need(
                bytes.len() + chunk.len() <= 65536,
                "S3 control document too large",
            )?;
            bytes.extend_from_slice(&chunk);
        }
        String::from_utf8(bytes).map_err(|_| "S3 control document invalid".into())
    }
    pub async fn policy(&self, c: &Config) -> Result<()> {
        for (t, b) in [
            (&self.source, c.value("bucket")?),
            (&self.replica, c.value("replica_bucket")?),
        ] {
            let xml = self.control(t, b, "versioning").await?;
            need(
                xml.contains("<Status>Enabled</Status>"),
                "versioning must be Enabled on source and replica",
            )?;
            let enc = self.control(t, b, "encryption").await?;
            need(
                enc.contains("<SSEAlgorithm>AES256</SSEAlgorithm>")
                    || enc.contains("<SSEAlgorithm>aws:kms</SSEAlgorithm>"),
                "bucket default encryption required",
            )?;
        }
        let xml = self
            .control(&self.source, c.value("bucket")?, "replication")
            .await?;
        replication_policy(&xml, c.value("replica_bucket")?)
    }
    pub async fn upload(&self, c: &Config, path: &Path, key: &str) -> Result<Value> {
        let size = fs::metadata(path)
            .map_err(|_| "snapshot unavailable")?
            .len();
        need(
            size <= 5 * 1024 * 1024 * 1024,
            "single PUT snapshot exceeds 5 GiB; multi-part archival required",
        )?;
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|_| "snapshot open failed")?;
        let headers = BTreeMap::from([
            ("x-amz-server-side-encryption".into(), "AES256".into()),
            ("content-length".into(), size.to_string()),
        ]);
        let r = self
            .request(
                &self.source,
                c.value("bucket")?,
                key,
                "PUT",
                BTreeMap::new(),
                headers,
                Some(reqwest::Body::from(file)),
            )
            .await?;
        let version = r
            .headers()
            .get("x-amz-version-id")
            .and_then(|v| v.to_str().ok())
            .ok_or("versioned snapshot upload required")?
            .to_string();
        need(version != "null", "unversioned snapshot upload refused")?;
        self.version_pair(c, key, &version).await
    }
    pub async fn version_pair(&self, c: &Config, key: &str, version: &str) -> Result<Value> {
        let source = self
            .request(
                &self.source,
                c.value("bucket")?,
                key,
                "HEAD",
                BTreeMap::from([("versionId".into(), version.into())]),
                BTreeMap::new(),
                None,
            )
            .await?;
        let source_encryption = header(&source, "x-amz-server-side-encryption")?;
        need(
            matches!(source_encryption.as_str(), "AES256" | "aws:kms"),
            "source object encryption required",
        )?;
        let mut metadata = BTreeMap::new();
        for (name, value) in source.headers() {
            if name.as_str() == "content-type" || name.as_str().starts_with("x-amz-meta-") {
                metadata.insert(
                    name.as_str().to_string(),
                    value
                        .to_str()
                        .map_err(|_| "invalid object metadata")?
                        .to_string(),
                );
            }
        }
        let etag = header(&source, "etag")?;
        let source_size = header(&source, "content-length")?;
        let deadline = Instant::now() + c.timeout();
        loop {
            let replica = self
                .request(
                    &self.replica,
                    c.value("replica_bucket")?,
                    key,
                    "HEAD",
                    BTreeMap::new(),
                    BTreeMap::new(),
                    None,
                )
                .await;
            if let Ok(r) = replica {
                if header(&r, "etag")? == etag && header(&r, "content-length")? == source_size {
                    let rv = header(&r, "x-amz-version-id")?;
                    need(rv != "null", "replica versioning required")?;
                    let enc = header(&r, "x-amz-server-side-encryption")?;
                    need(
                        matches!(enc.as_str(), "AES256" | "aws:kms"),
                        "replica encryption required",
                    )?;
                    return Ok(
                        json!({"key":key,"source_version":version,"replica_version":rv,"etag":etag,"metadata":metadata,"size_bytes":source_size.parse::<u64>().map_err(|_|"invalid object size")?}),
                    );
                }
            }
            need(
                Instant::now() < deadline,
                "replica did not converge before deadline",
            )?;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    pub async fn inventory(&self, c: &Config) -> Result<Vec<Value>> {
        self.inventory_prefix(c, c.value("object_prefix")?).await
    }
    pub async fn inventory_prefix(&self, c: &Config, prefix: &str) -> Result<Vec<Value>> {
        need(
            !prefix.is_empty() && !prefix.starts_with('/') && !prefix.contains(".."),
            "explicit object prefix required",
        )?;
        let mut cursor = None;
        let mut result = vec![];
        loop {
            let (keys, next) = self
                .source
                .list(c.value("bucket")?, prefix, cursor.clone())
                .await
                .map_err(|_| "object inventory failed")?;
            for key in keys {
                need(key.starts_with(prefix), "foreign object prefix")?;
                let r = self
                    .request(
                        &self.source,
                        c.value("bucket")?,
                        &key,
                        "HEAD",
                        BTreeMap::new(),
                        BTreeMap::new(),
                        None,
                    )
                    .await?;
                let v = header(&r, "x-amz-version-id")?;
                need(v != "null", "object versioning required")?;
                result.push(self.version_pair(c, &key, &v).await?);
                need(result.len() <= 100000, "object inventory bound exceeded")?;
            }
            if next.is_none() {
                break;
            }
            need(next != cursor, "object inventory cursor repeated")?;
            cursor = next;
        }
        Ok(result)
    }
    pub async fn download(&self, c: &Config, entry: &Value, path: &Path) -> Result<()> {
        let r = self
            .request(
                &self.replica,
                c.value("replica_bucket")?,
                text(entry, "key")?,
                "GET",
                BTreeMap::from([("versionId".into(), text(entry, "replica_version")?.into())]),
                BTreeMap::from([("if-match".into(), text(entry, "etag")?.into())]),
                None,
            )
            .await?;
        download(
            r,
            path,
            entry["size_bytes"].as_u64().ok_or("object size missing")?,
        )
        .await
    }
    pub async fn restore_objects(&self, c: &Config, entries: &[Value]) -> Result<usize> {
        let (keys, next) = self
            .drill
            .list(c.value("drill_bucket")?, "", None)
            .await
            .map_err(|_| "drill bucket unavailable")?;
        need(
            keys.is_empty() && next.is_none(),
            "drill object bucket must be empty; overwrite forbidden",
        )?;
        for e in entries {
            let key = text(e, "key")?;
            let temp =
                Path::new(c.value("drill_root")?).join(format!("object-{}", uuid::Uuid::new_v4()));
            self.download(c, e, &temp).await?;
            let mut headers: BTreeMap<String, String> =
                serde_json::from_value(e["metadata"].clone())
                    .map_err(|_| "object metadata missing")?;
            headers.insert("if-none-match".into(), "*".into());
            headers.insert("x-amz-server-side-encryption".into(), "AES256".into());
            let file = tokio::fs::File::open(&temp)
                .await
                .map_err(|_| "restored object unavailable")?;
            self.request(
                &self.drill,
                c.value("drill_bucket")?,
                key,
                "PUT",
                BTreeMap::new(),
                headers,
                Some(reqwest::Body::from(file)),
            )
            .await?;
            fs::remove_file(temp).map_err(|_| "temporary restored object cleanup failed")?;
        }
        Ok(entries.len())
    }
}
fn header(r: &reqwest::Response, name: &str) -> Result<String> {
    r.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("required storage response header missing: {name}"))
}
pub(super) fn replication_policy(xml: &str, bucket: &str) -> Result<()> {
    let rules = regex::Regex::new(r"(?s)<Rule>(.*?)</Rule>")
        .map_err(|_| "replication parser unavailable")?;
    let dest = format!("<Bucket>arn:aws:s3:::{bucket}</Bucket>");
    let ok = rules.captures_iter(xml).any(|r| {
        let rule = &r[1];
        rule.contains("<Status>Enabled</Status>")
            && rule.contains(&dest)
            && !rule.contains("<Prefix>")
            && !rule.contains("<Tag>")
            && (rule.contains("<Filter/>")
                || rule.contains("<Filter />")
                || rule.contains("<Filter></Filter>"))
    });
    need(
        ok,
        "enabled bucket-wide replication to exact replica required (empty Filter)",
    )
}
pub(super) async fn download(mut r: reqwest::Response, path: &Path, max: u64) -> Result<()> {
    let mut o = tokio::fs::OpenOptions::new();
    o.create_new(true).write(true);
    #[cfg(unix)]
    {
        o.mode(0o600);
    }
    let mut f = o
        .open(path)
        .await
        .map_err(|_| "new private snapshot path required")?;
    let mut size = 0;
    while let Some(chunk) = r
        .chunk()
        .await
        .map_err(|_| "snapshot transport read failed")?
    {
        size += chunk.len() as u64;
        need(size <= max, "snapshot exceeds bound")?;
        f.write_all(&chunk)
            .await
            .map_err(|_| "snapshot write failed")?;
    }
    f.sync_all().await.map_err(|_| "snapshot sync failed")?;
    need(size == max, "snapshot size mismatch")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replication_rejects_wrong_scope() {
        let good = "<Rule><Status>Enabled</Status><Filter/><Destination><Bucket>arn:aws:s3:::replica</Bucket></Destination></Rule>";
        assert!(replication_policy(good, "replica").is_ok());
        assert!(replication_policy(good, "other").is_err());
        assert!(
            replication_policy(
                &good.replace("<Filter/>", "<Filter><Prefix>one/</Prefix></Filter>"),
                "replica"
            )
            .is_err()
        );
        assert!(replication_policy(&good.replace("Enabled", "Disabled"), "replica").is_err());
    }
}
