//! Existing Knowledge-owner manual sync contract. No connector execution or retries.
use crate::{ApiRequest, Error};
use reqwest::Method;
use serde_json::{Value, json};

pub fn request_id_valid(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                *c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
        && matches!(b[14], b'1'..=b'8')
        && matches!(b[19], b'8' | b'9' | b'a' | b'A' | b'b' | b'B')
}
fn path<'a>(dataset: &'a str, binding: &'a str) -> Result<Vec<&'a str>, Error> {
    if [dataset, binding]
        .iter()
        .any(|s| s.is_empty() || *s == "." || *s == ".." || s.contains(['\\', '\r', '\n', '\0']))
    {
        return Err(Error::Request);
    }
    Ok(vec![
        "api",
        "v1",
        "knowledge",
        dataset,
        "sources",
        "confluence",
        binding,
        "sync",
    ])
}
pub fn start_request(dataset: &str, binding: &str, id: &str) -> Result<ApiRequest, Error> {
    if !request_id_valid(id) {
        return Err(Error::Request);
    }
    Ok(ApiRequest::mutation(
        Method::POST,
        &path(dataset, binding)?,
        Some(json!({"request_id": id})),
    ))
}
pub fn read_request(dataset: &str, binding: &str, id: &str) -> Result<ApiRequest, Error> {
    if !request_id_valid(id) {
        return Err(Error::Request);
    }
    let mut segments = path(dataset, binding)?;
    segments.push(id);
    Ok(ApiRequest::get(&segments))
}
pub fn validate_receipt(value: Value, binding: &str, id: &str) -> Result<Value, Error> {
    if !request_id_valid(id)
        || value["task_id"].as_str() != Some(id)
        || value["binding_id"].as_str() != Some(binding)
        || !matches!(
            value["status"].as_str(),
            Some("pending" | "processing" | "completed" | "failed")
        )
    {
        return Err(Error::Contract(
            "Mismatched source sync receipt; recheck the original request",
        ));
    }
    for field in [
        "total_items",
        "processed_items",
        "synced_items",
        "failed_items",
        "error_count",
    ] {
        if !value[field].is_null() && value[field].as_u64().is_none() {
            return Err(Error::Response);
        }
    }
    Ok(value)
}
pub fn terminal(value: &Value) -> bool {
    matches!(value["status"].as_str(), Some("completed" | "failed"))
}
pub fn active(value: &Value) -> bool {
    matches!(value["status"].as_str(), Some("pending" | "processing"))
}

#[cfg(test)]
mod source_sync_tests {
    use super::*;
    const ID: &str = "7d73497c-7f68-4c13-a5d4-4681c3f7aaae";
    #[test]
    fn retained_web_producer_fixtures_match_native_contract() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../../../../../web/src/pages/knowledge/sources/sourceSyncContract.fixture.json"
        ))
        .unwrap();
        for task in fixtures.as_object().unwrap().values() {
            let id = task["task_id"].as_str().unwrap();
            validate_receipt(task.clone(), "binding-a", id).unwrap();
        }
    }
    #[test]
    fn requests_preserve_exact_identity() {
        let post = start_request("dataset/a", "binding b", ID).unwrap();
        assert_eq!(post.method, Method::POST);
        assert_eq!(post.body, Some(json!({"request_id":ID})));
        assert_eq!(post.segments[3], "dataset/a");
        let get = read_request("d", "b", ID).unwrap();
        assert_eq!(get.method, Method::GET);
        assert_eq!(get.segments.last().unwrap(), ID);
        assert!(get.body.is_none());
    }
    #[test]
    fn malformed_identity_never_constructs_write() {
        for id in [
            "",
            "not-an-id",
            "7d73497c-7f68-0c13-a5d4-4681c3f7aaae",
            "7d73497c-7f68-4c13-15d4-4681c3f7aaae",
        ] {
            assert!(start_request("d", "b", id).is_err());
        }
        assert!(start_request("", "b", ID).is_err());
    }
    #[test]
    fn receipt_requires_exact_owner_and_known_status() {
        for status in ["pending", "processing", "completed", "failed"] {
            let value = json!({"task_id":ID,"binding_id":"b","status":status});
            assert!(validate_receipt(value.clone(), "b", ID).is_ok());
            assert!(validate_receipt(value, "other", ID).is_err());
        }
        for status in ["succeeded", "cancelled", "unknown"] {
            assert!(
                validate_receipt(
                    json!({"task_id":ID,"binding_id":"b","status":status}),
                    "b",
                    ID
                )
                .is_err()
            );
        }
    }
    #[test]
    fn failed_partial_counts_stay_failed_and_unknown_totals_stay_null() {
        let v = validate_receipt(json!({"task_id":ID,"binding_id":"b","status":"failed","synced_items":3,"failed_items":2,"total_items":null}), "b", ID).unwrap();
        assert!(terminal(&v));
        assert!(!active(&v));
        assert!(v["total_items"].is_null());
        assert!(
            validate_receipt(
                json!({"task_id":ID,"binding_id":"b","status":"completed","synced_items":-1}),
                "b",
                ID
            )
            .is_err()
        );
    }
}
