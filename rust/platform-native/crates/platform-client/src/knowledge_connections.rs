//! Metadata-only controls for the retained Knowledge owner. Never retries writes.
use crate::{ApiRequest, Error};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Connection {
    pub connection_id: String,
    pub name: String,
    pub domain: String,
    pub status: String,
    pub sync_mode: String,
    pub revision: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Binding {
    pub binding_id: String,
    pub dataset_id: String,
    pub space_name: Option<String>,
    pub status: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Managed {
    pub connections: Vec<Connection>,
    pub next_offset: Option<u32>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Impact {
    pub connection: Connection,
    pub binding_count: u64,
    pub bindings: Vec<Binding>,
    /// Hidden bindings on this page, not across all pages.
    pub hidden_count: u64,
    pub next_offset: Option<u32>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub connection_id: String,
    pub expected_revision: String,
    pub expected_status: String,
    pub status: String,
}
fn identifier(id: &str) -> bool {
    !id.is_empty() && ![".", ".."].contains(&id) && !id.contains(['\\', '\r', '\n', '\0'])
}
fn supported(status: &str) -> bool {
    matches!(status, "active" | "disabled")
}
impl Connection {
    fn valid(&self) -> bool {
        identifier(&self.connection_id)
            && [
                &self.name,
                &self.domain,
                &self.status,
                &self.sync_mode,
                &self.revision,
            ]
            .iter()
            .all(|s| !s.is_empty())
            && self.revision.len() <= 80
    }
    pub fn change(&self) -> Result<Change, Error> {
        if !self.valid() || !supported(&self.status) {
            return Err(Error::Response);
        }
        Ok(Change {
            connection_id: self.connection_id.clone(),
            expected_revision: self.revision.clone(),
            expected_status: self.status.clone(),
            status: if self.status == "active" {
                "disabled"
            } else {
                "active"
            }
            .into(),
        })
    }
}
impl Change {
    pub fn valid(&self) -> bool {
        identifier(&self.connection_id)
            && !self.expected_revision.is_empty()
            && self.expected_revision.len() <= 80
            && supported(&self.expected_status)
            && supported(&self.status)
            && self.expected_status != self.status
    }
}
fn path<'a>(dataset: &'a str, id: Option<&'a str>) -> Result<Vec<&'a str>, Error> {
    if !identifier(dataset) || id.is_some_and(|id| !identifier(id)) {
        return Err(Error::Request);
    }
    let mut parts = vec![
        "api",
        "v1",
        "knowledge",
        dataset,
        "sources",
        "confluence",
        "connections",
    ];
    if let Some(id) = id {
        parts.extend([id, "control"]);
    } else {
        parts.push("manage");
    }
    Ok(parts)
}
fn offset_valid(offset: u32) -> bool {
    offset <= 100000
}
fn next_valid(next: Option<u32>, offset: u32) -> bool {
    offset_valid(offset) && next.is_none_or(|n| n == offset + 50 && offset_valid(n))
}
pub fn list_request(dataset: &str, offset: u32) -> Result<ApiRequest, Error> {
    if !offset_valid(offset) {
        return Err(Error::Request);
    }
    Ok(ApiRequest::get(&path(dataset, None)?).query("offset", offset))
}
pub fn impact_request(dataset: &str, id: &str, offset: u32) -> Result<ApiRequest, Error> {
    if !offset_valid(offset) {
        return Err(Error::Request);
    }
    Ok(ApiRequest::get(&path(dataset, Some(id))?).query("offset", offset))
}
pub fn change_request(dataset: &str, change: &Change) -> Result<ApiRequest, Error> {
    if !change.valid() {
        return Err(Error::Request);
    }
    Ok(ApiRequest::mutation(
        Method::PATCH,
        &path(dataset, Some(&change.connection_id))?,
        Some(
            serde_json::json!({"expected_revision":change.expected_revision,"expected_status":change.expected_status,"status":change.status}),
        ),
    ))
}
pub fn parse_connection(value: Value, id: &str) -> Result<Connection, Error> {
    let row: Connection = serde_json::from_value(value).map_err(|_| Error::Response)?;
    if !row.valid() || row.connection_id != id {
        return Err(Error::Response);
    }
    Ok(row)
}
pub fn parse_managed(value: Value, offset: u32) -> Result<Managed, Error> {
    if value.get("next_offset").is_none() {
        return Err(Error::Response);
    }
    let result: Managed = serde_json::from_value(value).map_err(|_| Error::Response)?;
    let mut ids = std::collections::BTreeSet::new();
    if !next_valid(result.next_offset, offset)
        || result.connections.len() > 50
        || (result.next_offset.is_some() && result.connections.len() != 50)
        || result
            .connections
            .iter()
            .any(|c| !c.valid() || !ids.insert(&c.connection_id))
    {
        return Err(Error::Response);
    }
    Ok(result)
}
pub fn parse_impact(value: Value, id: &str, offset: u32) -> Result<Impact, Error> {
    if value.get("next_offset").is_none()
        || value["bindings"].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|b| b.get("space_name").is_none() || b.get("status").is_none())
        })
    {
        return Err(Error::Response);
    }
    let result: Impact = serde_json::from_value(value).map_err(|_| Error::Response)?;
    let mut ids = std::collections::BTreeSet::new();
    let page_count = result
        .hidden_count
        .saturating_add(result.bindings.len() as u64);
    if !result.connection.valid()
        || result.connection.connection_id != id
        || !next_valid(result.next_offset, offset)
        || page_count > 50
        || result.binding_count < page_count
        || (result.next_offset.is_some() && page_count != 50)
        || result.bindings.iter().any(|b| {
            !identifier(&b.binding_id) || !identifier(&b.dataset_id) || !ids.insert(&b.binding_id)
        })
    {
        return Err(Error::Response);
    }
    Ok(result)
}

#[cfg(test)]
mod connection_control_tests {
    use super::*;
    use serde_json::json;
    fn row() -> Value {
        json!({"connection_id":"c","name":"Saved","domain":"example.atlassian.net","status":"active","sync_mode":"manual","revision":"r1"})
    }
    #[test]
    fn exact_cas_body_and_encoded_segments_only() {
        let c = parse_connection(row(), "c").unwrap().change().unwrap();
        let r = change_request("dataset/a", &c).unwrap();
        assert_eq!(r.method, Method::PATCH);
        assert_eq!(r.segments[3], "dataset/a");
        assert_eq!(r.segments.last().unwrap(), "control");
        assert_eq!(
            r.body.unwrap(),
            json!({"expected_revision":"r1","expected_status":"active","status":"disabled"})
        );
        let mut c = c;
        c.expected_status = "disabled".into();
        c.status = "active".into();
        assert!(change_request("d", &c).is_ok());
        c.status = c.expected_status.clone();
        assert!(change_request("d", &c).is_err());
    }
    #[test]
    fn unsupported_states_and_malformed_recovery_never_write() {
        let mut v = row();
        v["status"] = json!("error");
        assert!(parse_connection(v, "c").unwrap().change().is_err());
        let mut c = parse_connection(row(), "c").unwrap().change().unwrap();
        for s in ["", ".", "..", "a\nb"] {
            assert!(change_request(s, &c).is_err());
        }
        c.expected_revision.clear();
        assert!(change_request("d", &c).is_err());
        assert!(parse_connection(row(), "other").is_err());
    }
    #[test]
    fn bounded_list_and_read_requests_are_get_only() {
        for offset in [0, 50, 100000] {
            assert_eq!(list_request("d", offset).unwrap().method, Method::GET);
            assert_eq!(
                impact_request("d", "c", offset).unwrap().method,
                Method::GET
            );
        }
        assert!(list_request("d", 100001).is_err());
        assert!(parse_managed(json!({"connections":[row()],"next_offset":0}), 0).is_err());
        assert!(parse_managed(json!({"connections":[row(),row()],"next_offset":null}), 0).is_err());
        assert!(parse_managed(json!({"connections":[row()],"next_offset":null}), 0).is_ok());
    }
    #[test]
    fn hidden_counts_are_per_page_and_never_zeroed() {
        let v = json!({"connection":row(),"binding_count":76,"bindings":[],"hidden_count":50,"next_offset":50});
        let p = parse_impact(v.clone(), "c", 0).unwrap();
        assert_eq!(p.hidden_count, 50);
        assert_eq!(p.binding_count, 76);
        assert!(parse_impact(v.clone(), "other", 0).is_err());
        assert!(parse_impact(v, "c", 50).is_err());
        for field in ["binding_count", "hidden_count"] {
            let mut v = json!({"connection":row(),"binding_count":0,"bindings":[],"hidden_count":0,"next_offset":null});
            v[field] = json!(-1);
            assert!(parse_impact(v, "c", 0).is_err());
        }
    }
    #[test]
    fn malformed_impact_never_grants_confirmation() {
        let base = json!({"connection":row(),"binding_count":1,"bindings":[{"binding_id":"b","dataset_id":"d","space_name":null,"status":null}],"hidden_count":0,"next_offset":null});
        assert!(parse_impact(base.clone(), "c", 0).is_ok());
        for (field, val) in [
            ("hidden_count", json!(51)),
            ("next_offset", json!(50)),
            ("binding_count", json!(0)),
        ] {
            let mut v = base.clone();
            v[field] = val;
            assert!(parse_impact(v, "c", 0).is_err());
        }
        let mut v = base;
        v["bindings"][0]["dataset_id"] = json!("");
        assert!(parse_impact(v, "c", 0).is_err());
    }
}
