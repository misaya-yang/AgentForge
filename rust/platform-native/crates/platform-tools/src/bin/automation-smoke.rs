//! Exercises an explicitly configured existing acceptance stack. Starts no services.
use hmac::{Hmac, Mac};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use sha2::Sha256;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Smoke {
    client: Client,
    base: String,
    key: String,
}
fn env(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("required environment: {name}").into())
}
impl Smoke {
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        revision: Option<i64>,
        expected: u16,
        other: Option<&str>,
    ) -> Result<Value> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(other.unwrap_or(&self.key));
        if let Some(body) = body {
            request = request.json(&body);
        }
        if let Some(revision) = revision {
            request = request.header("if-match", format!("\"{revision}\""));
        }
        let response = request.send().await?;
        if response.status().as_u16() != expected {
            return Err(format!("unexpected status from {path}; response withheld").into());
        }
        Ok(response.json().await?)
    }
    async fn create(&self, definition: Value) -> Result<Value> {
        let row = self
            .request(
                Method::POST,
                "/api/v1/automations",
                Some(definition),
                None,
                200,
                None,
            )
            .await?["automation"]
            .clone();
        if row["status"] != "draft" {
            return Err("creation must remain an inactive draft".into());
        }
        Ok(row)
    }
    async fn activate(&self, row: &Value) -> Result<Value> {
        let id = row["automation_id"]
            .as_str()
            .ok_or("missing automation id")?;
        self.request(
            Method::POST,
            &format!("/api/v1/automations/{id}/confirm"),
            Some(json!({"confirm":true})),
            row["revision"].as_i64(),
            200,
            None,
        )
        .await
    }
    async fn wait(&self, id: &str, count: usize, seconds: u64) -> Result<()> {
        for _ in 0..seconds {
            let data = self
                .request(
                    Method::GET,
                    &format!("/api/v1/automations/{id}/executions"),
                    None,
                    None,
                    200,
                    None,
                )
                .await?;
            let executions = data["executions"].as_array().ok_or("missing executions")?;
            if executions.len() >= count
                && executions.iter().all(|row| row["status"] == "succeeded")
            {
                return Ok(());
            }
            if executions
                .iter()
                .any(|row| ["failed", "uncertain"].contains(&row["status"].as_str().unwrap_or("")))
            {
                return Err("automation effect did not succeed; response withheld".into());
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        Err("automation execution timed out".into())
    }
    async fn webhook(
        &self,
        id: &str,
        secret: &str,
        timestamp: i64,
        delivery: &str,
        body: &str,
        corrupt: bool,
        expected: u16,
    ) -> Result<()> {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())?;
        mac.update(format!("{timestamp}.{delivery}.{body}").as_bytes());
        let mut signature = mac.finalize().into_bytes().to_vec();
        if corrupt {
            signature[0] ^= 1;
        }
        let hex = signature
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let response = self
            .client
            .post(format!("{}/api/v1/automations/{id}/webhook", self.base))
            .header("x-automation-timestamp", timestamp.to_string())
            .header("x-automation-delivery", delivery)
            .header("x-automation-signature", format!("sha256={hex}"))
            .body(body.to_owned())
            .send()
            .await?;
        if response.status().as_u16() != expected {
            return Err("webhook signature/replay expectation failed".into());
        }
        Ok(())
    }
}
#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!(
            "automation smoke failed; verify existing stack and AUTOMATION_SMOKE_* inputs (responses and credentials withheld)"
        );
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let base = env("AUTOMATION_SMOKE_BASE_URL")?;
    let url = reqwest::Url::parse(&base)?;
    if !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
        || url.path() != "/"
        || url.query().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("existing loopback acceptance stack required".into());
    }
    let smoke = Smoke {
        client: Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(150))
            .build()?,
        base: base.trim_end_matches('/').into(),
        key: env("AUTOMATION_SMOKE_API_KEY")?,
    };
    let other = env("AUTOMATION_SMOKE_OTHER_API_KEY")?;
    let model = env("AUTOMATION_SMOKE_MODEL")?;
    let skill = env("AUTOMATION_SMOKE_SKILL_ID")?;
    let space = smoke
        .request(
            Method::POST,
            "/api/v1/knowledge/spaces",
            Some(json!({"name":"automation acceptance","kind":"personal","visibility":"private"})),
            None,
            200,
            None,
        )
        .await?;
    let space_id = space["space_id"].as_str().ok_or("space id missing")?;
    let mut pages = Vec::new();
    for title in ["event source", "action destination"] {
        let item=smoke.request(Method::POST,&format!("/api/v1/knowledge/spaces/{space_id}/items"),Some(json!({"title":title,"kind":"page","content_format":"markdown","source":"native"})),None,200,None).await?;
        pages.push(
            item["item_id"]
                .as_str()
                .ok_or("item id missing")?
                .to_owned(),
        );
    }
    let marker = format!("automation-{}", uuid::Uuid::new_v4());
    let draft=smoke.create(json!({"title":"webhook acceptance","model_id":model,"trigger":{"kind":"webhook"},"actions":[{"kind":"notify","message":marker},{"kind":"write_space_page","item_id":pages[1],"markdown":marker},{"kind":"run_agent","prompt":"Reply with the word accepted. Do not call tools."},{"kind":"run_skill","skill_id":skill,"prompt":"Read-only acceptance: follow the skill instructions and reply briefly; do not mutate any resource."}]})).await?;
    let id = draft["automation_id"].as_str().ok_or("draft id missing")?;
    let path = format!("/api/v1/automations/{id}");
    smoke
        .request(Method::GET, &path, None, None, 404, Some(&other))
        .await?;
    smoke
        .request(
            Method::POST,
            &format!("{path}/confirm"),
            Some(json!({"confirm":false})),
            Some(1),
            422,
            None,
        )
        .await?;
    let inactive = smoke
        .request(
            Method::GET,
            &format!("{path}/executions"),
            None,
            None,
            200,
            None,
        )
        .await?;
    if inactive["executions"]
        .as_array()
        .is_none_or(|rows| !rows.is_empty())
    {
        return Err("draft dispatched without confirmation".into());
    }
    let active = smoke.activate(&draft).await?;
    let secret = active["webhook_secret"]
        .as_str()
        .ok_or("webhook secret not issued")?;
    smoke
        .request(
            Method::POST,
            &format!("{path}/confirm"),
            Some(json!({"confirm":true})),
            Some(1),
            409,
            None,
        )
        .await?;
    let now = chrono::Utc::now().timestamp();
    smoke
        .webhook(id, secret, now, "wrong-signature", "{}", true, 401)
        .await?;
    smoke
        .webhook(id, secret, now - 301, "expired", "{}", false, 401)
        .await?;
    smoke
        .webhook(id, secret, now, "delivery-1", "{}", false, 200)
        .await?;
    smoke
        .webhook(id, secret, now, "delivery-1", "{}", false, 409)
        .await?;
    smoke.wait(id, 4, 90).await?;
    let notifications = smoke
        .request(
            Method::GET,
            "/api/v1/automations/notifications",
            None,
            None,
            200,
            None,
        )
        .await?;
    if !notifications["notifications"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["message"] == marker))
    {
        return Err("notification action missing".into());
    }
    let page = smoke
        .request(
            Method::GET,
            &format!("/api/v1/knowledge/space-items/{}/content", pages[1]),
            None,
            None,
            200,
            None,
        )
        .await?;
    if !page["markdown"]
        .as_str()
        .is_some_and(|v| v.contains(&marker))
    {
        return Err("space-page action missing".into());
    }
    let event=smoke.create(json!({"title":"page event acceptance","model_id":model,"trigger":{"kind":"space_page_changed","item_id":pages[0]},"actions":[{"kind":"notify","message":"source page changed"}]})).await?;
    let event_active = smoke.activate(&event).await?;
    let source = smoke
        .request(
            Method::GET,
            &format!("/api/v1/knowledge/space-items/{}/content", pages[0]),
            None,
            None,
            200,
            None,
        )
        .await?;
    smoke
        .request(
            Method::PUT,
            &format!("/api/v1/knowledge/space-items/{}/content", pages[0]),
            Some(json!({"content_format":"markdown","markdown":marker})),
            source["revision"].as_i64(),
            200,
            None,
        )
        .await?;
    smoke
        .wait(
            event["automation_id"].as_str().ok_or("event id missing")?,
            1,
            30,
        )
        .await?;
    let connections = smoke
        .request(Method::GET, "/api/v1/connections", None, None, 200, None)
        .await?;
    if connections["connections"].as_array().is_none_or(|rows| {
        rows.iter()
            .any(|row| row["entry_id"] == "local-files" && row["status"] != "revoked")
    }) {
        return Err(
            "local-files fixture already connected; use an isolated acceptance account".into(),
        );
    }
    let connector=smoke.create(json!({"title":"connector event acceptance","model_id":model,"trigger":{"kind":"connector_event","entry_id":"local-files"},"actions":[{"kind":"notify","message":"catalog connection changed"}]})).await?;
    let connector_active = smoke.activate(&connector).await?;
    let connection = smoke
        .request(
            Method::POST,
            "/api/v1/connections",
            Some(json!({"entry_id":"local-files","owner_kind":"user"})),
            None,
            201,
            None,
        )
        .await?;
    smoke
        .wait(
            connector["automation_id"]
                .as_str()
                .ok_or("connector automation id missing")?,
            1,
            30,
        )
        .await?;
    let cron=smoke.create(json!({"title":"cron acceptance","model_id":model,"trigger":{"kind":"cron","expression":"* * * * *","timezone":"America/New_York"},"actions":[{"kind":"notify","message":"cron timezone accepted"}]})).await?;
    let cron_active = smoke.activate(&cron).await?;
    smoke
        .wait(
            cron["automation_id"].as_str().ok_or("cron id missing")?,
            1,
            75,
        )
        .await?;
    for row in [
        &active["automation"],
        &event_active["automation"],
        &cron_active["automation"],
        &connector_active["automation"],
    ] {
        let id = row["automation_id"].as_str().ok_or("active id missing")?;
        smoke
            .request(
                Method::POST,
                &format!("/api/v1/automations/{id}/cancel"),
                Some(json!({})),
                row["revision"].as_i64(),
                200,
                None,
            )
            .await?;
    }
    let connection_id = connection["connection_id"]
        .as_str()
        .ok_or("fixture connection id missing")?;
    smoke
        .request(
            Method::POST,
            &format!("/api/v1/connections/{connection_id}/revoke"),
            Some(json!({})),
            None,
            200,
            None,
        )
        .await?;
    let draft=smoke.request(Method::POST,"/api/v1/automations/draft",Some(json!({"prompt":format!("Use model {model}. Every weekday at 09:00 America/New_York send me a notification saying review complete. Return an inactive draft.")})),None,200,None).await?;
    if draft["automation"]["status"] != "draft" {
        return Err("natural-language draft activated itself".into());
    }
    println!(
        "AUTOMATION_OK draft-confirm tenant-isolation HMAC expiry replay notification page-write agent skill page-event connector-event cron-timezone natural-language-draft"
    );
    Ok(())
}
