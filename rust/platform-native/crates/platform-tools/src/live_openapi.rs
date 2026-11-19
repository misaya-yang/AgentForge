//! Live source-contract assertions; unavailable stack is explicitly SKIPPED.
use crate::*;
use std::{collections::BTreeSet, time::Duration};
fn required(spec: &Value, path: &str, method: &str) -> BTreeSet<String> {
    let mut schema = &spec["paths"][path][method.to_lowercase()]["requestBody"]["content"]["application/json"]
        ["schema"];
    if let Some(reference) = schema["$ref"]
        .as_str()
        .and_then(|s| s.strip_prefix("#/components/schemas/"))
    {
        if let Some(resolved) = spec["components"]["schemas"].get(reference) {
            schema = resolved;
        }
    }
    schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
fn compare(baseline: &Value, current: &Value) -> Value {
    let empty = serde_json::Map::new();
    let paths = baseline["paths"].as_object().unwrap_or(&empty);
    let live = current["paths"].as_object().unwrap_or(&empty);
    let removed: Vec<_> = paths
        .keys()
        .filter(|p| !live.contains_key(*p))
        .cloned()
        .collect();
    let mut drift = Vec::new();
    for (path, operations) in paths {
        for method in operations.as_object().into_iter().flat_map(|o| o.keys()) {
            if method.starts_with("x-") {
                continue;
            }
            let lost: Vec<_> = required(baseline, path, method)
                .difference(&required(current, path, method))
                .cloned()
                .collect();
            if !lost.is_empty() {
                drift
                    .push(json!({"method":method.to_uppercase(),"path":path,"lost_required":lost}));
            }
        }
    }
    json!({"schema":"gateway-live-openapi-contract/v1","result":if removed.is_empty()&&drift.is_empty(){"pass"}else{"fail"},"removed_paths":removed,"required_drift":drift,"scope":"published path superset and retained required request fields; not complete schema equivalence"})
}
pub fn run(args: &Args) -> Result<Value> {
    let root = args.root();
    let baseline = root.join("sdk/openapi.json");
    let value = if !baseline.is_file() {
        json!({"result":"skipped","reason":"Gateway OpenAPI snapshot missing"})
    } else {
        let baseline: Value = strict_json(&std::fs::read(baseline)?)?;
        let base = args
            .values
            .get("--base-url")
            .cloned()
            .or_else(|| std::env::var("GATEWAY_BASE_URL").ok())
            .unwrap_or_else(|| "http://localhost:8080".into());
        let url = reqwest::Url::parse(&format!("{}/openapi.json", base.trim_end_matches('/')))?;
        require(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none(),
            "live OpenAPI URL must be HTTP(S) without credentials",
        )?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let current = runtime.block_on(async {
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()
                .ok()?;
            let mut response = client.get(url).send().await.ok()?;
            if response.status() != 200 {
                return None;
            }
            let mut data = Vec::new();
            while let Some(chunk) = response.chunk().await.ok()? {
                if data.len() + chunk.len() > 32 * 1024 * 1024 {
                    return None;
                }
                data.extend_from_slice(&chunk);
            }
            let value: Value = serde_json::from_slice(&data).ok()?;
            value.is_object().then_some(value)
        });
        match current {
            Some(current) => compare(&baseline, &current),
            None => {
                json!({"result":"skipped","reason":"Gateway OpenAPI unavailable; run against the Gateway"})
            }
        }
    };
    if let Some(path) = args.values.get("--evidence-out") {
        write_json(Path::new(path), &value)?;
    }
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removed_paths_and_required_refs_fail() {
        let baseline = json!({"paths":{"/x":{"post":{"requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Input"}}}}}}},"components":{"schemas":{"Input":{"required":["name"]}}}});
        assert_eq!(compare(&baseline, &json!({"paths":{}}))["result"], "fail");
        assert_eq!(
            compare(&baseline, &json!({"paths":{"/x":{"post":{}}}}))["result"],
            "fail"
        );
        assert_eq!(compare(&baseline, &baseline)["result"], "pass");
    }
    #[test]
    fn additive_paths_and_optional_fields_are_allowed() {
        assert_eq!(
            compare(
                &json!({"paths":{"/x":{"get":{},"x-marker":{}}}}),
                &json!({"paths":{"/x":{"get":{}},"/y":{"post":{}}}})
            )["result"],
            "pass"
        );
    }
}
