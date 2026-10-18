use super::*;
use futures_util::stream;
use reqwest::{Method, Url};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
pub(super) fn endpoint(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| error("Qdrant endpoint invalid"))?;
    let private = match url.host_str() {
        Some(host) => match host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(ip)) => ip.is_private() || ip.is_loopback(),
            Ok(std::net::IpAddr::V6(ip)) => ip.is_unique_local() || ip.is_loopback(),
            Err(_) => {
                host == "localhost"
                    || !host.contains('.')
                    || host.ends_with(".internal")
                    || host.ends_with(".svc.cluster.local")
            }
        },
        None => false,
    };
    if url.scheme() != "https"
        || !private
        || url.username() != ""
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || raw.contains('\\')
    {
        return fail("Qdrant requires an explicit private HTTPS origin");
    }
    Ok(url)
}
pub(super) struct Node {
    client: reqwest::Client,
    origin: Url,
}
impl Node {
    pub fn new(e: &Endpoint) -> Result<Self> {
        let origin = endpoint(&e.url)?;
        let token = private_read(&e.api_key_file, 65536)?;
        let token = std::str::from_utf8(&token)?.trim_end_matches(['\n', '\r']);
        if token.len() < 16
            || token
                .bytes()
                .any(|b| !b.is_ascii() || b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return fail("Qdrant file API key invalid");
        }
        let mut value = reqwest::header::HeaderValue::from_str(token)?;
        value.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("api-key", value);
        let ca = read_file(&e.ca_file, 1024 * 1024, false)?;
        let certificates =
            reqwest::Certificate::from_pem_bundle(&ca).map_err(|_| error("Qdrant CA invalid"))?;
        if certificates.is_empty() {
            return fail("Qdrant CA empty");
        }
        let client = reqwest::Client::builder()
            .https_only(true)
            .tls_certs_only(certificates)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(1800))
            .build()
            .map_err(|_| error("Qdrant client unavailable"))?;
        Ok(Self { client, origin })
    }
    fn url(&self, path: &str) -> Result<Url> {
        if !path.starts_with('/') || path.starts_with("//") || path.contains(['\\', '#']) {
            return fail("invalid Qdrant operation path");
        }
        let url = self.origin.join(path)?;
        if url.origin() != self.origin.origin() {
            return fail("Qdrant origin escaped");
        }
        Ok(url)
    }
    pub async fn json(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut request =
            self.client
                .request(method, self.url(path)?)
                .timeout(Duration::from_secs(if path.contains("/snapshots?") {
                    1800
                } else {
                    30
                }));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| error("Qdrant operation transport failed"))?;
        let value = bounded_json(response, 16 * 1024 * 1024).await?;
        if value.get("status").is_some_and(|s| s != "ok") {
            return fail("Qdrant operation status failed");
        }
        Ok(value)
    }
    pub async fn version(&self, expected: &str) -> Result<()> {
        let v = self.json(Method::GET, "/", None).await?;
        if v["version"] != expected {
            return fail("Qdrant runtime version mismatch");
        }
        Ok(())
    }
    pub async fn wait_version(&self, expected: &str) -> Result<()> {
        for _ in 0..60 {
            if self.version(expected).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        fail("Qdrant upgraded node did not become ready")
    }
    pub async fn single_node(&self) -> Result<()> {
        let v = self.json(Method::GET, "/cluster", None).await?;
        let r = &v["result"];
        if r["status"] != "disabled" && r["peers"].as_object().is_none_or(|p| p.len() != 1) {
            return fail(
                "collection snapshot upgrade is limited to a single node; distributed/shard restore requires another workflow",
            );
        }
        Ok(())
    }
    pub async fn collections(&self) -> Result<Vec<String>> {
        let v = self.json(Method::GET, "/collections", None).await?;
        let mut names = v["result"]["collections"]
            .as_array()
            .ok_or_else(|| error("Qdrant collection inventory invalid"))?
            .iter()
            .map(|v| {
                v["name"]
                    .as_str()
                    .filter(|s| component(s))
                    .map(str::to_owned)
                    .ok_or_else(|| error("collection name invalid"))
            })
            .collect::<Result<Vec<_>>>()?;
        names.sort();
        if names.is_empty() {
            return Ok(names);
        }
        if names.windows(2).any(|v| v[0] == v[1]) {
            return fail("duplicate collections");
        }
        Ok(names)
    }
    pub async fn aliases(&self) -> Result<Value> {
        let value = self.json(Method::GET, "/aliases", None).await?;
        let mut aliases = value["result"]["aliases"]
            .as_array()
            .ok_or_else(|| error("alias inventory invalid"))?
            .clone();
        if aliases.iter().any(|a| {
            !a["alias_name"].as_str().is_some_and(component)
                || !a["collection_name"].as_str().is_some_and(component)
        }) {
            return fail("alias name/collection invalid");
        }
        aliases.sort_by(|a, b| a["alias_name"].as_str().cmp(&b["alias_name"].as_str()));
        Ok(json!(aliases))
    }
    pub async fn verify_aliases(&self, expected: &Value) -> Result<()> {
        if &self.aliases().await? != expected {
            return fail("Qdrant alias inventory changed");
        }
        Ok(())
    }
    pub async fn wait_unchanged(&self, expected: &[Snapshot]) -> Result<()> {
        for _ in 0..60 {
            if self.unchanged(expected).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        fail("restored/upgraded collection did not become green with expected config/count")
    }
    pub async fn inventory(&self, names: &[String]) -> Result<Vec<Snapshot>> {
        let mut out = Vec::new();
        for name in names {
            let info = self
                .json(Method::GET, &format!("/collections/{name}"), None)
                .await?;
            let info = &info["result"];
            if info["status"] != "green"
                || (!info["optimizer_status"].is_null() && info["optimizer_status"] != "ok")
            {
                return fail("Qdrant collection not green");
            }
            let count = self
                .json(
                    Method::POST,
                    &format!("/collections/{name}/points/count"),
                    Some(json!({"exact":true})),
                )
                .await?["result"]["count"]
                .as_u64()
                .ok_or_else(|| error("Qdrant exact count invalid"))?;
            out.push(Snapshot {
                collection: name.clone(),
                file: String::new(),
                sha256: String::new(),
                bytes: 0,
                count,
                config: info["config"].clone(),
                payload_schema: info["payload_schema"].clone(),
            });
        }
        Ok(out)
    }
    pub async fn unchanged(&self, expected: &[Snapshot]) -> Result<()> {
        let names = self.collections().await?;
        if names
            != expected
                .iter()
                .map(|s| s.collection.clone())
                .collect::<Vec<_>>()
        {
            return fail("collection inventory changed");
        }
        let actual = self.inventory(&names).await?;
        for (a, b) in actual.iter().zip(expected) {
            if a.count != b.count
                || !retained(&b.config, &a.config)
                || !retained(&b.payload_schema, &a.payload_schema)
            {
                return fail("Qdrant collection configuration/count changed");
            }
        }
        Ok(())
    }
    pub async fn golden(&self, golden: &Golden) -> Result<BTreeMap<String, Vec<Hit>>> {
        let mut out = BTreeMap::new();
        for c in &golden.cases {
            let v = self
                .json(
                    Method::POST,
                    &format!("/collections/{}/points/query", c.collection),
                    Some(c.request.clone()),
                )
                .await?;
            let hits = parse_hits(c, &v)?;
            out.insert(c.id.clone(), hits);
        }
        Ok(out)
    }
    pub async fn download(&self, path: &str, target: &Path, limit: u64) -> Result<()> {
        let mut response = self
            .client
            .get(self.url(path)?)
            .send()
            .await
            .map_err(|_| error("snapshot download failed"))?;
        if !response.status().is_success() || response.content_length().is_some_and(|n| n > limit) {
            return fail("snapshot download status/size invalid");
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(target).await?;
        let mut n = 0u64;
        while let Some(bytes) = response
            .chunk()
            .await
            .map_err(|_| error("snapshot download interrupted"))?
        {
            n = n
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| error("snapshot size overflow"))?;
            if n > limit {
                return fail("snapshot exceeds byte budget; partial evidence retained");
            }
            file.write_all(&bytes).await?;
        }
        file.sync_all().await?;
        Ok(())
    }
    pub async fn upload(
        &self,
        collection: &str,
        path: &Path,
        bytes: u64,
        checksum: &str,
    ) -> Result<()> {
        let file = tokio::fs::File::open(path).await?;
        let boundary = format!("agentforge-{}", sha(collection.as_bytes()));
        let header=format!("--{boundary}\r\nContent-Disposition: form-data; name=\"snapshot\"; filename=\"restore.snapshot\"\r\nContent-Type: application/octet-stream\r\n\r\n").into_bytes();
        let footer = format!("\r\n--{boundary}--\r\n").into_bytes();
        let size = bytes
            .checked_add((header.len() + footer.len()) as u64)
            .ok_or_else(|| error("snapshot upload length overflow"))?;
        let stream = stream::try_unfold(
            (file, 0u8, header, footer),
            |(mut file, stage, header, footer)| async move {
                let next = match stage {
                    0 => Some((header.clone(), 1)),
                    1 => {
                        let mut buffer = vec![0; 65536];
                        let n = file.read(&mut buffer).await?;
                        if n == 0 {
                            Some((footer.clone(), 2))
                        } else {
                            buffer.truncate(n);
                            Some((buffer, 1))
                        }
                    }
                    _ => None,
                };
                Ok::<_, std::io::Error>(
                    next.map(|(bytes, stage)| (bytes, (file, stage, header, footer))),
                )
            },
        );
        let response=self.client.post(self.url(&format!("/collections/{collection}/snapshots/upload?wait=true&priority=snapshot&checksum={checksum}"))?).header("content-type",format!("multipart/form-data; boundary={boundary}")).header("content-length",size).body(reqwest::Body::wrap_stream(stream)).send().await.map_err(|_|error("snapshot upload failed"))?;
        let value = bounded_json(response, 1024 * 1024).await?;
        if value["status"] != "ok" {
            return fail("snapshot restore failed");
        }
        Ok(())
    }
}
pub(super) fn parse_hits(c: &Case, v: &Value) -> Result<Vec<Hit>> {
    let rows = v["result"]["points"]
        .as_array()
        .ok_or_else(|| error("Qdrant query result invalid"))?;
    let mut hits = Vec::new();
    let mut ids = BTreeSet::new();
    for row in rows {
        if row["payload"]["tenant_id"] != c.tenant_id
            || row["payload"]["dataset_id"] != c.dataset_id
        {
            return fail("Qdrant golden detected cross-tenant/dataset payload");
        }
        let id = row["id"].clone();
        let score = row["score"]
            .as_f64()
            .ok_or_else(|| error("query score invalid"))?;
        if !score.is_finite() || (!id.is_string() && !id.is_u64()) || !ids.insert(id.to_string()) {
            return fail("query point ID/score invalid");
        }
        hits.push(Hit {
            id,
            score,
            payload_sha256: sha(&serde_json::to_vec(&row["payload"])?),
        })
    }
    if hits.len() > 20 {
        return fail("query result limit exceeded");
    }
    Ok(hits)
}

pub(super) fn retained(old: &Value, new: &Value) -> bool {
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => a
            .iter()
            .all(|(k, v)| b.get(k).is_some_and(|n| retained(v, n))),
        _ => old == new,
    }
}
pub(super) async fn bounded_json(mut response: reqwest::Response, max: usize) -> Result<Value> {
    if !response.status().is_success() || response.content_length().is_some_and(|n| n > max as u64)
    {
        return fail("HTTP metadata/Qdrant response status or size invalid");
    }
    let mut bytes = Vec::new();
    while let Some(b) = response
        .chunk()
        .await
        .map_err(|_| error("HTTP response interrupted"))?
    {
        if bytes.len() + b.len() > max {
            return fail("HTTP response exceeds limit");
        }
        bytes.extend_from_slice(&b);
    }
    Ok(serde_json::from_slice(&bytes).map_err(|_| error("HTTP JSON response invalid"))?)
}
