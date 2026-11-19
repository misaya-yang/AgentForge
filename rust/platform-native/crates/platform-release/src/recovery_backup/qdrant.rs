use super::*;
pub(super) async fn request(
    c: &Config,
    client: &reqwest::Client,
    drill: bool,
    method: reqwest::Method,
    path: &[&str],
) -> Result<reqwest::RequestBuilder> {
    let mut url = local_url(c.value(if drill {
        "qdrant_drill"
    } else {
        "qdrant_source"
    })?)?;
    {
        let mut seg = url
            .path_segments_mut()
            .map_err(|_| "invalid Qdrant origin")?;
        seg.clear();
        for p in path {
            need(
                !p.is_empty() && !p.contains('/') && !p.contains(".."),
                "invalid Qdrant path segment",
            )?;
            seg.push(p);
        }
    }
    Ok(client.request(method, url).header(
        "api-key",
        c.secret(if drill {
            "qdrant_drill_key_file"
        } else {
            "qdrant_source_key_file"
        })?,
    ))
}
pub(super) async fn json(request: reqwest::RequestBuilder) -> Result<Value> {
    let mut r = request
        .send()
        .await
        .map_err(|_| "Qdrant connection failed")?
        .error_for_status()
        .map_err(|_| "Qdrant operation refused")?;
    let mut body = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|_| "Qdrant response interrupted")? {
        need(
            body.len() + chunk.len() <= 4 * 1024 * 1024,
            "Qdrant control response too large",
        )?;
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| "Qdrant response invalid".into())
}
