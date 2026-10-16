use ai_platform_release::{Result, require};
use serde_json::{Value, json};
async fn run() -> Result<i32> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let raw = if a.is_empty() {
        std::env::var("E2E_API_URL").unwrap_or_default()
    } else {
        require(
            a.len() == 2 && a[0] == "--api-url",
            "usage: verify-local-stack [--api-url URL]",
        )?;
        a[1].clone()
    };
    let base = raw.trim_end_matches('/');
    if base.is_empty() {
        eprintln!("Missing API URL. Set E2E_API_URL or pass --api-url.");
        return Ok(2);
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let fetch = |path: &str| client.get(format!("{base}{path}"));
    let health: Value = fetch("/health")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let openapi: Value = fetch("/openapi.json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let mut missing = [
        "/api/v1/auth/login",
        "/api/v1/auth/me",
        "/api/v1/assistant/chat/stream",
        "/api/v1/sessions",
    ]
    .into_iter()
    .filter(|p| openapi["paths"].get(*p).is_none())
    .collect::<Vec<_>>();
    missing.sort();
    if !missing.is_empty() {
        eprintln!(
            "Target API does not look like ai-gateway. Missing required paths: {}",
            missing.join(", ")
        );
        return Ok(1);
    }
    let title = openapi["info"]["title"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");
    println!(
        "{}",
        json!({"status":"ok","title":title,"health":health,"api_url":base})
    );
    Ok(0)
}
#[tokio::main]
async fn main() {
    match run().await {
        Ok(c) => std::process::exit(c),
        Err(_) => {
            eprintln!("Stack verification failed: invalid configuration or unavailable endpoint");
            std::process::exit(1)
        }
    }
}
