use super::*;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Metadata {
    pub schema: String,
    pub verified_at: String,
    pub images: Vec<Image>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Image {
    pub version: String,
    pub image: String,
    pub digest: String,
    pub registry: String,
    pub release: String,
    pub published_at: String,
    pub platforms: BTreeMap<String, String>,
}
pub(super) fn one_minor(from: &str, to: &str) -> bool {
    let parse = |s: &str| {
        let v = s
            .split('.')
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .ok()?;
        if v.len() != 3 {
            return None;
        }
        Some((v[0], v[1], v[2]))
    };
    matches!((parse(from),parse(to)),(Some(a),Some(b)) if a.0==b.0&&a.1.checked_add(1)==Some(b.1))
}
fn digest_valid(s: &str) -> bool {
    s.strip_prefix("sha256:").is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}
pub(super) fn index(raw: &[u8], header: &str, expected: &str) -> Result<BTreeMap<String, String>> {
    if header != expected || format!("sha256:{}", sha(raw)) != expected {
        return fail("official registry tag/index digest changed or body checksum failed");
    }
    let v: Value = serde_json::from_slice(raw)?;
    if ![
        "application/vnd.oci.image.index.v1+json",
        "application/vnd.docker.distribution.manifest.list.v2+json",
    ]
    .contains(&v["mediaType"].as_str().unwrap_or(""))
    {
        return fail("official image must be an OCI multi-platform index");
    }
    let mut platforms = BTreeMap::new();
    for m in v["manifests"]
        .as_array()
        .ok_or_else(|| error("registry index lacks manifests"))?
    {
        if m["platform"]["os"] != "linux" {
            continue;
        }
        let arch = m["platform"]["architecture"].as_str().unwrap_or("");
        if !["amd64", "arm64"].contains(&arch) {
            continue;
        }
        let digest = m["digest"]
            .as_str()
            .filter(|s| digest_valid(s))
            .ok_or_else(|| error("platform manifest digest invalid"))?;
        if platforms
            .insert(format!("linux/{arch}"), digest.into())
            .is_some()
        {
            return fail("duplicate platform manifest");
        }
    }
    if platforms.len() != 2 {
        return fail("official image lacks linux/amd64 or linux/arm64");
    }
    Ok(platforms)
}
pub(super) fn validate_saved(m: &Metadata) -> Result<()> {
    if m.schema != "qdrant-image-metadata/v1"
        || m.images.len() != 2
        || !one_minor(OLD_VERSION, NEW_VERSION)
    {
        return fail("upgrade image metadata invalid");
    }
    let time = chrono::DateTime::parse_from_rfc3339(&m.verified_at)
        .map_err(|_| error("metadata timestamp invalid"))?
        .timestamp();
    let now = chrono::Utc::now().timestamp();
    if time > now + 300 || now - time > 7 * 86400 {
        return fail("metadata is stale; begin a new verified upgrade run");
    }
    for (image, (version, pin)) in m
        .images
        .iter()
        .zip([(OLD_VERSION, OLD_IMAGE), (NEW_VERSION, NEW_IMAGE)])
    {
        if image.version != version
            || image.image != pin
            || image.digest != pin.split('@').nth(1).unwrap_or("")
            || image.registry
                != format!("https://registry-1.docker.io/v2/qdrant/qdrant/manifests/v{version}")
            || image.release != format!("https://github.com/qdrant/qdrant/releases/tag/v{version}")
            || image.published_at.is_empty()
            || image.platforms.len() != 2
            || ["linux/amd64", "linux/arm64"]
                .iter()
                .any(|p| image.platforms.get(*p).is_none_or(|d| !digest_valid(d)))
        {
            return fail("metadata does not bind the reviewed official image pair");
        }
    }
    Ok(())
}
pub(super) async fn verify() -> Result<Metadata> {
    let client = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .user_agent("AgentForge-Qdrant-upgrade-metadata/v1")
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let token=super::transport::bounded_json(client.get("https://auth.docker.io/token?service=registry.docker.io&scope=repository:qdrant/qdrant:pull").send().await.map_err(|_|error("official registry token metadata unavailable"))?,1024*1024).await?;
    let token = token["token"]
        .as_str()
        .ok_or_else(|| error("registry token metadata invalid"))?;
    let mut images = Vec::new();
    for (version, pin) in [(OLD_VERSION, OLD_IMAGE), (NEW_VERSION, NEW_IMAGE)] {
        let release = super::transport::bounded_json(
            client
                .get(format!(
                    "https://api.github.com/repos/qdrant/qdrant/releases/tags/v{version}"
                ))
                .send()
                .await
                .map_err(|_| error("official release metadata unavailable"))?,
            2 * 1024 * 1024,
        )
        .await?;
        let release_url = format!("https://github.com/qdrant/qdrant/releases/tag/v{version}");
        if release["tag_name"] != format!("v{version}")
            || release["draft"] != false
            || release["prerelease"] != false
            || release["html_url"] != release_url
        {
            return fail("official stable release metadata mismatch");
        }
        let registry =
            format!("https://registry-1.docker.io/v2/qdrant/qdrant/manifests/v{version}");
        let mut response=client.get(&registry).bearer_auth(token).header("accept","application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json").send().await.map_err(|_|error("official OCI index metadata unavailable"))?;
        if !response.status().is_success() {
            return fail("official registry rejected metadata request");
        }
        let digest = response
            .headers()
            .get("docker-content-digest")
            .and_then(|s| s.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let mut raw = Vec::new();
        while let Some(bytes) = response
            .chunk()
            .await
            .map_err(|_| error("OCI index interrupted"))?
        {
            if raw.len() + bytes.len() > 2 * 1024 * 1024 {
                return fail("OCI index too large");
            }
            raw.extend_from_slice(&bytes);
        }
        let platforms = index(&raw, &digest, pin.split('@').nth(1).unwrap_or(""))?;
        images.push(Image {
            version: version.into(),
            image: pin.into(),
            digest,
            registry,
            release: release_url,
            published_at: release["published_at"]
                .as_str()
                .ok_or_else(|| error("release publication absent"))?
                .into(),
            platforms,
        });
    }
    let m = Metadata {
        schema: "qdrant-image-metadata/v1".into(),
        verified_at: chrono::Utc::now().to_rfc3339(),
        images,
    };
    validate_saved(&m)?;
    Ok(m)
}
