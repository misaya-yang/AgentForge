use crate::*;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
const HEADERS: &[&str] = &[
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
    "retry-after",
    "x-gateway-capacity-key",
    "x-gateway-queue-wait-ms",
    "x-request-id",
];
fn args() -> Result<(BTreeMap<String, String>, BTreeSet<String>)> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut options = BTreeMap::new();
    let mut flags = BTreeSet::new();
    let mut i = 0;
    while i < a.len() {
        if [
            "--readonly",
            "--allow-mutation",
            "--validate-fixtures",
            "--require-admin-jwt",
            "--require-non-admin-jwt",
            "--require-api-key",
            "--expect-capacity-denial",
        ]
        .contains(&a[i].as_str())
        {
            flags.insert(a[i].clone());
            i += 1;
            continue;
        }
        require(
            [
                "--base-url",
                "--service-id",
                "--concurrency",
                "--requests",
                "--stream-ratio",
                "--path",
            ]
            .contains(&a[i].as_str()),
            "unknown capacity option",
        )?;
        options.insert(
            a[i].clone(),
            a.get(i + 1).ok_or("missing option value")?.clone(),
        );
        i += 2;
    }
    Ok((options, flags))
}
fn header_map(smoke: bool) -> Result<reqwest::header::HeaderMap> {
    use reqwest::header::*;
    let mut h = HeaderMap::new();
    let mut token = std::env::var("GATEWAY_TOKEN").unwrap_or_default();
    if token.is_empty() && smoke {
        token = std::env::var("GATEWAY_ADMIN_JWT").unwrap_or_default();
    }
    if !token.is_empty() {
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| "invalid token header")?,
        );
    }
    let key = std::env::var("GATEWAY_API_KEY").unwrap_or_default();
    if !key.is_empty() {
        h.insert(
            "X-API-Key",
            HeaderValue::from_str(&key).map_err(|_| "invalid key header")?,
        );
    }
    Ok(h)
}
fn redact(mut s: String) -> String {
    for k in ["GATEWAY_TOKEN", "GATEWAY_API_KEY", "GATEWAY_ADMIN_JWT"] {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                s = s.replace(&v, "[redacted]");
            }
        }
    }
    s
}
async fn probe_request(
    c: &reqwest::Client,
    base: &str,
    method: &str,
    path: &str,
    body: Value,
) -> Result<Value> {
    let m = reqwest::Method::from_bytes(method.as_bytes())?;
    let r = c
        .request(m, format!("{}{path}", base.trim_end_matches('/')))
        .json(&body)
        .send()
        .await
        .map_err(|_| "capacity probe transport error")?;
    let status = r.status().as_u16();
    let present: serde_json::Map<String, Value> = HEADERS
        .iter()
        .map(|h| (h.to_string(), r.headers().contains_key(*h).into()))
        .collect();
    let preview = redact(
        r.text()
            .await
            .map_err(|_| "capacity probe body error")?
            .chars()
            .take(500)
            .collect(),
    );
    Ok(
        json!({"method":method,"path":path,"status_code":status,"headers_present":present,"body_preview":preview}),
    )
}
pub async fn probe() -> Result<i32> {
    let (o, f) = args()?;
    if !f.contains("--allow-mutation") && !f.contains("--readonly") {
        return Ok(3);
    }
    if f.contains("--validate-fixtures") {
        let missing = [
            ("--require-admin-jwt", "GATEWAY_ADMIN_JWT"),
            ("--require-non-admin-jwt", "GATEWAY_NON_ADMIN_JWT"),
            ("--require-api-key", "GATEWAY_API_KEY"),
        ]
        .into_iter()
        .filter(|(flag, key)| {
            f.contains(*flag) && std::env::var(key).unwrap_or_default().is_empty()
        })
        .map(|(_, k)| k)
        .collect::<Vec<_>>();
        if !missing.is_empty() {
            println!("{}", json!({"fixture_status":"missing","missing":missing}));
            return Ok(2);
        }
    }
    let base = o.get("--base-url").ok_or("--base-url required")?;
    let service = o
        .get("--service-id")
        .map(String::as_str)
        .unwrap_or("local-2024-agent");
    let path = o
        .get("--path")
        .map(String::as_str)
        .unwrap_or("assistants/search");
    let n = o
        .get("--concurrency")
        .map(String::as_str)
        .unwrap_or("4")
        .parse::<usize>()?;
    let c = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .default_headers(header_map(false)?)
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    let mut results = vec![];
    for p in [
        "/api/v1/config/status".into(),
        "/api/v1/config/rate-limits".into(),
        format!("/api/v1/config/services/{service}/config"),
    ] {
        results.push(probe_request(&c, base, "GET", &p, Value::Null).await?);
    }
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..n {
        let c = c.clone();
        let base = base.clone();
        let p = format!("/api/v1/proxy/{service}/{path}");
        tasks.spawn(async move {
            let r = probe_request(&c, &base, "POST", &p, json!({"limit":5,"probe":i}))
                .await
                .map_err(|_| "capacity probe failed".to_string());
            (i, r)
        });
    }
    let mut ordered = BTreeMap::new();
    while let Some(r) = tasks.join_next().await {
        let (i, v) = r?;
        ordered.insert(
            i,
            v.map_err(|e| -> Box<dyn std::error::Error> { e.into() })?,
        );
    }
    results.extend(ordered.into_values());
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"base_url":base,"service_id":service,"readonly":f.contains("--readonly"),"results":results})
        )?
    );
    Ok(0)
}
pub async fn smoke() -> Result<i32> {
    let (o, f) = args()?;
    let headers = header_map(true)?;
    if headers.is_empty() {
        eprintln!("GATEWAY_TOKEN/GATEWAY_ADMIN_JWT or GATEWAY_API_KEY is required");
        return Ok(2);
    }
    let base = o.get("--base-url").ok_or("--base-url required")?.clone();
    let service = o
        .get("--service-id")
        .cloned()
        .unwrap_or("local-2024-agent".into());
    let path = o
        .get("--path")
        .cloned()
        .unwrap_or("assistants/search".into());
    let concurrency = o
        .get("--concurrency")
        .map(String::as_str)
        .unwrap_or("20")
        .parse::<usize>()?;
    require(concurrency > 0, "concurrency must be positive")?;
    let count = o
        .get("--requests")
        .map(String::as_str)
        .unwrap_or("60")
        .parse::<usize>()?;
    let ratio = o
        .get("--stream-ratio")
        .map(String::as_str)
        .unwrap_or("0")
        .parse::<f64>()?;
    require(ratio.is_finite(), "invalid ratio")?;
    let cutoff = (count as f64 * ratio.clamp(0., 1.)) as usize;
    let c = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .default_headers(headers)
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..count {
        let c = c.clone();
        let base = base.clone();
        let service = service.clone();
        let path = path.clone();
        let semaphore = semaphore.clone();
        tasks.spawn(async move{let _permit=semaphore.acquire_owned().await.unwrap();let(endpoint,body)=if i<cutoff{(format!("/api/v1/proxy/{service}/runs/stream"),json!({"input":{"messages":[{"role":"user","content":format!("capacity smoke {i}")}]},"stream_mode":["updates"]}))}else{(format!("/api/v1/proxy/{service}/{path}"),json!({"limit":5,"probe":i}))};let result=async{let r=c.post(format!("{}{endpoint}",base.trim_end_matches('/'))).json(&body).send().await?;let status=r.status().as_u16();let h=r.headers().contains_key("x-gateway-capacity-key");let body=r.text().await?.chars().take(1000).collect::<String>();Ok::<_,reqwest::Error>((status,h,redact(body)))}.await;(i,result.map_err(|_|"connection_error"))});
    }
    let mut ordered = BTreeMap::new();
    while let Some(r) = tasks.join_next().await {
        let (i, r) = r?;
        ordered.insert(i, r);
    }
    let mut counts = BTreeMap::<String, usize>::new();
    let mut unexpected = vec![];
    let mut capacity_headers = 0;
    for r in ordered.into_values() {
        match r {
            Err(_) => {
                *counts.entry("connection_error".into()).or_default() += 1;
                unexpected.push(json!({"status":"connection_error","body":"request failed"}));
            }
            Ok((status, header, body)) => {
                let key = if (200..300).contains(&status) {
                    "2xx".into()
                } else {
                    status.to_string()
                };
                *counts.entry(key).or_default() += 1;
                let short = body.chars().take(300).collect::<String>();
                if !(200..300).contains(&status) && status != 429 && status != 503 {
                    unexpected.push(json!({"status":status,"body":short}));
                }
                if status >= 500 && status != 503 {
                    unexpected.push(json!({"status":status,"body":short}));
                }
                let lower = body.to_lowercase();
                if [
                    "api_key",
                    "_api_key",
                    "authorization",
                    "bearer ",
                    "cookie",
                    "auth_token",
                ]
                .iter()
                .any(|s| lower.contains(s))
                {
                    unexpected.push(json!({"status":status,"body":"secret marker detected"}));
                }
                if header {
                    capacity_headers += 1;
                }
            }
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"base_url":base,"service_id":service,"requests":count,"concurrency":concurrency,"stream_ratio":ratio,"counts":counts,"capacity_header_responses":capacity_headers,"unexpected":unexpected.iter().take(10).collect::<Vec<_>>()})
        )?
    );
    if !unexpected.is_empty() {
        return Ok(1);
    }
    if f.contains("--expect-capacity-denial")
        && counts.get("429").unwrap_or(&0) + counts.get("503").unwrap_or(&0) == 0
    {
        return Ok(2);
    }
    Ok(0)
}
