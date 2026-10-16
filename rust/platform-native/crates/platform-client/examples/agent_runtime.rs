//! Native translation of the scoped immutable-publication HTTP example.
use serde_json::{Value, json};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base =
        std::env::var("AI_GATEWAY_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".into());
    let publication = std::env::var("AGENT_PUBLICATION_ID")?;
    let token = std::env::var("AGENT_RUNTIME_TOKEN")?;
    let mut endpoint = url::Url::parse(&base)?;
    endpoint
        .path_segments_mut()
        .map_err(|_| "invalid gateway URL")?
        .clear()
        .extend(["api", "v1", "agent-runtime", &publication]);
    let route = |suffix: &[&str]| -> Result<url::Url, Box<dyn std::error::Error>> {
        let mut u = endpoint.clone();
        u.path_segments_mut()
            .map_err(|_| "invalid gateway URL")?
            .extend(suffix);
        Ok(u)
    };
    let client = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let mut attachments = Vec::new();
    if let Some(path) = std::env::var("AGENT_ATTACHMENT_PATH")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        let path = path.trim();
        let path = if let Some(rest) = path.strip_prefix("~/") {
            std::path::PathBuf::from(std::env::var("HOME")?).join(rest)
        } else {
            path.into()
        };
        let name = path
            .file_name()
            .ok_or("attachment filename absent")?
            .to_string_lossy()
            .into_owned();
        let part = reqwest::multipart::Part::file(&path).await?.file_name(name);
        let uploaded: Value = client
            .post(route(&["attachments"])?)
            .bearer_auth(&token)
            .multipart(reqwest::multipart::Form::new().part("file", part))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let mut item = serde_json::Map::new();
        for key in ["artifact_id", "filename", "mime_type"] {
            item.insert(
                key.into(),
                uploaded
                    .get(key)
                    .ok_or("invalid attachment response")?
                    .clone(),
            );
        }
        attachments.push(Value::Object(item));
    }
    let session: Value = client
        .post(route(&["sessions"])?)
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let mut response=client.post(route(&["chat","stream"])?).bearer_auth(&token).header("Idempotency-Key","sdk-example-turn-1").json(&json!({"message":"Summarize the approved policy.","session_id":session.get("session_id").ok_or("session_id absent")?,"attachments":attachments})).send().await?.error_for_status()?;
    let mut pending = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        pending.extend_from_slice(&chunk);
        while let Some(index) = pending.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = pending.drain(..=index).collect();
            print_line(std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']))?;
        }
    }
    if !pending.is_empty() {
        print_line(std::str::from_utf8(&pending)?)?;
    }
    Ok(())
}
fn print_line(line: &str) -> Result<(), serde_json::Error> {
    if let Some(data) = line.strip_prefix("data:") {
        if data.trim() != "[DONE]" {
            println!("{}", serde_json::from_str::<Value>(data)?);
        }
    }
    Ok(())
}
