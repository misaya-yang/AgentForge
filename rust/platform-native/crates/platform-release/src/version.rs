use crate::*;
use serde_json::json;
use std::collections::BTreeSet;
const HTTP: &[&str] = &["gateway", "knowledge-service"];
const CONTAINERS: &[&str] = &[
    "frontend",
    "knowledge-worker",
    "migrator",
    "agent-runtime",
    "capability-worker",
];
fn keys(v: &Value, expected: &[&str]) -> Result<()> {
    let m = v.as_object().ok_or("expected object")?;
    require(
        m.keys().map(String::as_str).collect::<BTreeSet<_>>() == expected.iter().copied().collect(),
        "identity observation set drift",
    )
}
fn identity<'a>(m: &'a Value, s: &str, mode: &str) -> Result<(&'a Value, &'a Value)> {
    let r = &m["services"][s];
    let i = &r["identity"];
    require(
        text(r, "image_digest")?
            .strip_prefix("sha256:")
            .is_some_and(|x| hex(x, 64)),
        "invalid image digest",
    )?;
    for (k, v) in [
        ("mode", json!(mode)),
        ("service_id", json!(s)),
        ("release_id", m["release_id"].clone()),
        ("git_sha", m["source"]["git_sha"].clone()),
        ("image_digest", r["image_digest"].clone()),
    ] {
        require(i[k] == v, format!("manifest identity mismatch: {s}.{k}"))?;
    }
    Ok((r, i))
}
pub fn labels(m: &Value, s: &str, i: &Value) -> Result<Value> {
    if ["frontend", "knowledge-worker", "migrator"].contains(&s) {
        let version = text(i, "image_version")?;
        let sha = text(&m["source"], "git_sha")?;
        require(hex(sha, 40), "invalid source SHA")?;
        return Ok(
            json!({"org.opencontainers.image.revision":sha,"org.opencontainers.image.version":version}),
        );
    }
    require(
        ["agent-runtime", "capability-worker"].contains(&s),
        "unsupported container",
    )?;
    let o = &m["runtime_overlay"];
    let upstream = text(o, "upstream_sha")?;
    let overlay = text(o, "overlay_sha256")?;
    require(
        hex(upstream, 40) && hex(overlay, 64),
        "invalid runtime source identity",
    )?;
    let key = if s == "agent-runtime" {
        "runtime_schema_digest"
    } else {
        "capability_worker_schema_digest"
    };
    let schema = text(o, key)?;
    require(hex(schema, 64), "invalid runtime schema")?;
    let mut labels = serde_json::Map::new();
    labels.insert(
        "org.opencontainers.image.revision".into(),
        format!("{upstream}+{}", &overlay[..12]).into(),
    );
    labels.insert(
        format!("com.misaya.ai-platform.{s}.schema-sha256"),
        schema.into(),
    );
    labels.insert(
        format!("com.misaya.ai-platform.{s}.artifact"),
        s.replace('-', "_").into(),
    );
    labels.insert(
        format!("com.misaya.ai-platform.{s}.binary"),
        format!("ai-platform-{s}").into(),
    );
    Ok(labels.into())
}
pub fn verify_container(
    m: &Value,
    s: &str,
    i: &Value,
    inspected: &Value,
    digest: &Value,
) -> Result<()> {
    require(
        inspected["Image"] == *digest,
        format!("running container image mismatch: {s}"),
    )?;
    let state = &inspected["State"];
    let live = if s == "migrator" {
        state["Running"] == true || (state["Status"] == "exited" && state["ExitCode"] == 0)
    } else {
        state["Running"] == true && state["Status"] == "running"
    };
    require(live, format!("container not running/succeeded: {s}"))?;
    for (k, v) in labels(m, s, i)?.as_object().unwrap() {
        require(
            inspected["Config"]["Labels"][k] == *v,
            format!("container label mismatch: {s}.{k}"),
        )?;
    }
    Ok(())
}
fn endpoint(s: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(s)?;
    require(
        ["http", "https"].contains(&u.scheme())
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
            && u.path() == "/version"
            && u.query().is_none()
            && u.fragment().is_none(),
        "version endpoint must be exact HTTP(S) /version without userinfo/query/fragment",
    )?;
    Ok(u)
}
pub async fn verify(m: &Value, endpoints: &Value, containers: &Value, hash: &str) -> Result<Value> {
    require(
        m["schema_version"] == "ai-platform/compatibility-manifest/v1"
            && ["release_candidate", "released"].contains(&text(m, "status")?)
            && regex::Regex::new(r"^[a-z0-9][a-z0-9._-]{7,127}$")?.is_match(text(m, "release_id")?)
            && hex(hash, 64),
        "manifest not runnable release",
    )?;
    keys(&m["services"], compatibility::SERVICES)?;
    keys(endpoints, HTTP)?;
    keys(containers, CONTAINERS)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut observations = serde_json::Map::new();
    for s in HTTP {
        let (r, i) = identity(m, s, "http_version")?;
        let u = endpoint(text(endpoints, s)?)?;
        let response = client
            .get(u)
            .send()
            .await
            .map_err(|_| "version endpoint unavailable")?;
        require(response.status().as_u16() == 200, "version endpoint non200")?;
        let p: Value = response
            .json()
            .await
            .map_err(|_| "version endpoint invalid JSON")?;
        let expected = json!({"schema_version":"ai-platform/service-version/v1","service_id":s,"release_id":i["release_id"],"git_sha":i["git_sha"],"image_digest":r["image_digest"],"manifest_sha256":hash});
        for (k, v) in expected.as_object().unwrap() {
            require(p[k] == *v, format!("live version mismatch: {s}.{k}"))?;
        }
        observations.insert(s.to_string(), json!({"mode":"http_version","replicas":1}));
    }
    let re = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$")?;
    let mut all = BTreeSet::new();
    for s in CONTAINERS {
        let (r, i) = identity(m, s, "container_image")?;
        let raw = &containers[*s];
        let targets = if let Some(s) = raw.as_str() {
            vec![s]
        } else {
            raw.as_array()
                .ok_or("invalid container target list")?
                .iter()
                .map(|x| x.as_str().ok_or("nontext target"))
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        require(!targets.is_empty(), "empty container targets")?;
        for target in &targets {
            require(
                re.is_match(target) && all.insert(target.to_string()),
                "invalid/duplicate container target",
            )?;
            let o = std::process::Command::new("docker")
                .args(["container", "inspect", target])
                .output()
                .map_err(|_| "Docker inspection unavailable")?;
            require(o.status.success(), "release container unavailable")?;
            let inspected: Value = serde_json::from_slice(&o.stdout)?;
            let a = inspected.as_array().ok_or("invalid inspect shape")?;
            require(a.len() == 1 && a[0].is_object(), "invalid inspect shape")?;
            verify_container(m, s, i, &a[0], &r["image_digest"])?;
        }
        observations.insert(
            s.to_string(),
            json!({"mode":"container_image","replicas":targets.len()}),
        );
    }
    let services = observations.keys().cloned().collect::<BTreeSet<_>>();
    Ok(
        json!({"result":"pass","services":services,"manifest_sha256":hash,"observations":observations}),
    )
}
pub async fn from_environment() -> Result<Value> {
    let path = std::env::var("AI_PLATFORM_COMPATIBILITY_MANIFEST_PATH")?;
    require(!path.is_empty(), "manifest path missing")?;
    let endpoints = serde_json::from_str(&std::env::var("AI_PLATFORM_VERSION_ENDPOINTS")?)?;
    let containers = serde_json::from_str(&std::env::var("AI_PLATFORM_VERSION_CONTAINERS")?)?;
    let p = Path::new(&path);
    verify(&load(p)?, &endpoints, &containers, &digest(p)?).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urls_are_exact() {
        assert!(endpoint("http://127.0.0.1:8000/version").is_ok());
        for s in [
            "https://u:p@example.com/version",
            "https://example.com/version?x=1",
            "https://example.com/foo",
            "file:///version",
        ] {
            assert!(endpoint(s).is_err());
        }
    }
}
