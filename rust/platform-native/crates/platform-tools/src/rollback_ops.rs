//! Rollback rehearsal helpers. Evidence is derived from real API responses.
use crate::*;
use std::time::Duration;
fn quoted(value: &str) -> Option<String> {
    let quote = value.chars().next()?;
    if !matches!(quote, '\'' | '"') || !value.ends_with(quote) || value.len() < 2 {
        return None;
    }
    let mut chars = value[1..value.len() - 1].chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == quote {
            return None;
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            '\n' => {}
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\x0b'),
            c @ ('x' | 'u' | 'U') => {
                let count = match c {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let mut n = 0;
                for _ in 0..count {
                    n = n * 16 + chars.next()?.to_digit(16)?;
                }
                out.push(char::from_u32(n)?);
            }
            c @ '0'..='7' => {
                let mut n = c.to_digit(8)?;
                for _ in 0..2 {
                    let Some(next) = chars.clone().next().and_then(|c| c.to_digit(8)) else {
                        break;
                    };
                    chars.next();
                    n = n * 8 + next;
                }
                out.push(char::from_u32(n)?);
            }
            c => {
                out.push('\\');
                out.push(c);
            }
        }
    }
    Some(out)
}
fn positional<'a>(args: &'a Args, n: usize) -> Result<&'a str> {
    args.positionals
        .get(n)
        .map(String::as_str)
        .ok_or_else(|| "missing rollback argument".into())
}
fn plain(s: impl Into<String>) -> Value {
    json!({"_plain":s.into()})
}
pub fn run(args: &Args) -> Result<Value> {
    match positional(args, 1)? {
        "env-value" => {
            let text = std::fs::read_to_string(positional(args, 2)?)?;
            let key = positional(args, 3)?;
            for raw in text.lines() {
                let line = raw.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let Some((candidate, value)) = line.split_once('=') else {
                    continue;
                };
                if candidate.trim() != key {
                    continue;
                }
                let value = value.trim();
                let result = if value.len() >= 2
                    && matches!(value.chars().next(), Some('\'' | '"'))
                    && value.chars().next() == value.chars().last()
                {
                    quoted(value).unwrap_or_else(|| value[1..value.len() - 1].to_owned())
                } else {
                    value.to_owned()
                };
                return Ok(plain(result));
            }
            Ok(plain(""))
        }
        "image-field" => {
            let value = object(Path::new(positional(args, 2)?))?;
            let service = positional(args, 3)?;
            let field = positional(args, 4)?;
            let entries: Vec<_> = value["images"]
                .as_array()
                .ok_or("missing image inventory")?
                .iter()
                .filter(|v| v["service"] == service)
                .collect();
            require(
                entries.len() == 1,
                "rollback bundle must contain exactly one service image",
            )?;
            Ok(plain(
                entries[0][field].as_str().ok_or("missing image field")?,
            ))
        }
        "api-fingerprint" => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(api_fingerprint(Path::new(positional(args, 2)?))),
        "report" => report(args),
        _ => Err("unknown rollback operation".into()),
    }
}
async fn response(request: reqwest::RequestBuilder) -> Result<Value> {
    let mut response = request.send().await?;
    require(
        response.status().is_success(),
        "rollback API request failed",
    )?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        require(
            bytes.len() + chunk.len() <= 16 * 1024 * 1024,
            "rollback API response too large",
        )?;
        bytes.extend_from_slice(&chunk);
    }
    strict_json(&bytes)
}
async fn api_fingerprint(path: &Path) -> Result<Value> {
    let credentials = object(path)?;
    let client = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?;
    let login = response(
        client
            .post("http://127.0.0.1:8080/api/v1/auth/login")
            .json(&credentials),
    )
    .await?;
    let token = login["access_token"]
        .as_str()
        .ok_or("login token missing")?;
    let payload = response(
        client
            .get("http://127.0.0.1:8080/api/v1/assistant/sessions?limit=50")
            .bearer_auth(token),
    )
    .await?;
    let empty = Vec::new();
    let sessions = if payload.is_array() {
        payload.as_array().unwrap()
    } else {
        payload["sessions"].as_array().unwrap_or(&empty)
    };
    let mut ids = Vec::new();
    for s in sessions {
        ids.push(
            s["session_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or(s["id"].as_str())
                .ok_or("invalid session ID")?
                .to_owned(),
        );
    }
    let sample = ids
        .first()
        .map(|s| format!("{:x}", Sha256::digest(s.as_bytes())));
    ids.sort();
    let mut history_digests = Vec::new();
    let mut count = 0;
    for id in &ids {
        let mut url = reqwest::Url::parse("http://127.0.0.1:8080/api/v1/assistant/sessions/")?;
        url.path_segments_mut()
            .map_err(|_| "invalid API base")?
            .pop_if_empty()
            .push(id)
            .push("history");
        url.query_pairs_mut().append_pair("limit", "500");
        let payload = response(client.get(url).bearer_auth(token)).await?;
        let history = if payload.is_array() {
            &payload
        } else {
            payload
                .get("history")
                .or_else(|| payload.get("messages"))
                .unwrap_or(&Value::Null)
        };
        let history = if history.is_null() {
            json!([])
        } else {
            history.clone()
        };
        count += history.as_array().ok_or("history must be array")?.len();
        history_digests.push(format!(
            "{id}:{:x}",
            Sha256::digest(canonical_sorted(&history).as_bytes())
        ));
    }
    Ok(plain(canonical_sorted(
        &json!({"session_count":sessions.len(),"session_set_sha256":format!("{:x}",Sha256::digest(ids.join("\n").as_bytes())),"sample_session_sha256":sample,"history_count":count,"history_sha256":format!("{:x}",Sha256::digest(history_digests.join("\n").as_bytes()))}),
    )))
}
fn report(args: &Args) -> Result<Value> {
    require(
        args.positionals.len() == 14,
        "rollback report requires twelve arguments",
    )?;
    let p = |n| positional(args, n);
    let before_db = strict_json(p(8)?.as_bytes())?;
    let old_db = strict_json(p(9)?.as_bytes())?;
    let after_db = strict_json(p(10)?.as_bytes())?;
    let before_api = strict_json(p(11)?.as_bytes())?;
    let old_api = strict_json(p(12)?.as_bytes())?;
    let after_api = strict_json(p(13)?.as_bytes())?;
    require(
        p(8)? == p(9)? && p(9)? == p(10)? && p(11)? == p(12)? && p(12)? == p(13)?,
        "rollback fingerprints differ; refusing pass report",
    )?;
    let value = json!({"schema_version":"agent-runtime-rollback-rehearsal/v1","completed_at":chrono::Utc::now().to_rfc3339(),"sequence":["current_new","frozen_old","current_new"],"rollback_commit":p(3)?,"images":{"rollback_runtime":p(4)?,"rollback_assistant":p(5)?,"current_runtime":p(6)?,"current_worker":p(7)?},"database":{"before":before_db,"rollback":old_db,"restored":after_db,"preserved":true},"api":{"before":before_api,"rollback":old_api,"restored":after_api,"preserved":true},"schema_downgrade":!(before_db["migrations_digest"]==old_db["migrations_digest"]&&old_db["migrations_digest"]==after_db["migrations_digest"]),"volumes_deleted":false,"status":"pass"});
    write_json(Path::new(p(2)?), &value)?;
    Ok(json!({"result":"pass","report":p(2)?}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dotenv_quoted_values_preserve_python_escape_contract() {
        assert_eq!(quoted("'a\\n\\x41\\101\\u4e2d'"), Some("a\nAA中".into()));
        assert_eq!(quoted("\"unknown\\q\""), Some("unknown\\q".into()));
        assert_eq!(quoted("'unterminated"), None);
    }
}
