//! Native Confluence client discovery and legacy page-tree projection helpers.
//! These do not mount additional HTTP routes or start polling in the entrypoint.
use super::{credentials::Credentials, *};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Space {
    pub space_id: String,
    pub space_key: String,
    pub name: String,
    pub r#type: String,
    pub status: String,
    pub homepage_id: Option<String>,
    pub description: Option<String>,
}
impl Space {
    fn from_value(v: &Value, description: bool) -> Self {
        Self {
            space_id: client::scalar(&v["id"]),
            space_key: text(v, "key").into(),
            name: text(v, "name").into(),
            r#type: v["type"].as_str().unwrap_or("global").into(),
            status: v["status"].as_str().unwrap_or("current").into(),
            homepage_id: v
                .get("homepageId")
                .filter(|v| !v.is_null())
                .map(client::scalar),
            description: if description {
                v["description"]["plain"]["value"]
                    .as_str()
                    .map(str::to_owned)
            } else {
                None
            },
        }
    }
}
fn next_cursor(data: &Value) -> Option<String> {
    let next = data["_links"]["next"].as_str()?;
    reqwest::Url::parse("https://pagination.invalid/")
        .ok()?
        .join(next)
        .ok()?
        .query_pairs()
        .find(|(k, v)| k == "cursor" && !v.is_empty())
        .map(|(_, v)| v.into_owned())
}
impl Sources {
    pub async fn get_space(&self, credentials: &Credentials, space_id: &str) -> Result<Space> {
        Ok(Space::from_value(
            &self
                .provider(credentials, &["spaces", space_id], &[])
                .await?,
            true,
        ))
    }
    pub async fn get_space_by_key(&self, credentials: &Credentials, key: &str) -> Result<Space> {
        let data = self
            .provider(credentials, &["spaces"], &[("keys", key)])
            .await?;
        let row = data["results"]
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| Error::new(StatusCode::BAD_GATEWAY, "Confluence space not found"))?;
        Ok(Space::from_value(row, false))
    }
    pub async fn list_spaces(
        &self,
        credentials: &Credentials,
        type_filter: Option<&str>,
        status: &str,
        limit: usize,
    ) -> Result<Vec<Space>> {
        let limit = limit.to_string();
        let mut params = vec![("status", status), ("limit", &limit)];
        if let Some(filter) = type_filter {
            params.push(("type", filter));
        }
        let data = self.provider(credentials, &["spaces"], &params).await?;
        Ok(data["results"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| Space::from_value(v, false))
            .collect())
    }
    pub async fn list_space_pages(
        &self,
        credentials: &Credentials,
        space_id: &str,
        status: &str,
        limit: usize,
        cursor: Option<&str>,
        body_format: Option<&str>,
    ) -> Result<(Vec<Value>, Option<String>)> {
        let limit = limit.min(250).to_string();
        let mut params = vec![("status", status), ("limit", &limit)];
        if let Some(cursor) = cursor.filter(|v| !v.is_empty()) {
            params.push(("cursor", cursor));
        }
        if let Some(format) = body_format.filter(|v| !v.is_empty()) {
            params.push(("body-format", format));
        }
        let data = self
            .provider(credentials, &["spaces", space_id, "pages"], &params)
            .await?;
        Ok((
            data["results"].as_array().cloned().unwrap_or_default(),
            next_cursor(&data),
        ))
    }
    pub async fn discover_pages(
        &self,
        credentials: &Credentials,
        space_key: &str,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let space = self.get_space_by_key(credentials, space_key).await?;
        let mut cursor = None;
        let mut seen = BTreeSet::new();
        let mut result = Vec::new();
        loop {
            let (pages, next) = self
                .list_space_pages(
                    credentials,
                    &space.space_id,
                    "current",
                    25,
                    cursor.as_deref(),
                    None,
                )
                .await?;
            for page in pages {
                result.push(json!({"page_id":page["id"],"title":page["title"],"status":page["status"],"version":page["version"]["number"]}));
                if result.len() >= limit {
                    return Ok(result);
                }
            }
            let Some(next) = next else { return Ok(result) };
            if !seen.insert(next.clone()) {
                return Err(Error::new(
                    StatusCode::BAD_GATEWAY,
                    "Repeated Confluence discovery cursor",
                ));
            }
            cursor = Some(next);
        }
    }
    pub async fn discover_space_page_tree(
        &self,
        credentials: &Credentials,
        space_key: &str,
        max_depth: usize,
    ) -> Result<Value> {
        let space = self.get_space_by_key(credentials, space_key).await?;
        let mut cursor = None;
        let mut seen = BTreeSet::new();
        let mut all = Vec::new();
        loop {
            let (pages, next) = self
                .list_space_pages(
                    credentials,
                    &space.space_id,
                    "current",
                    50,
                    cursor.as_deref(),
                    None,
                )
                .await?;
            all.extend(pages);
            let Some(next) = next else { break };
            if !seen.insert(next.clone()) {
                return Err(Error::new(
                    StatusCode::BAD_GATEWAY,
                    "Repeated Confluence discovery cursor",
                ));
            }
            cursor = Some(next);
        }
        Ok(page_tree(
            &credentials.domain,
            space_key,
            &space.name,
            &all,
            max_depth,
        ))
    }
}
/// Matches source root/orphan handling, title ordering and truncated-child flags.
pub fn page_tree(
    domain: &str,
    space_key: &str,
    space_name: &str,
    pages: &[Value],
    max_depth: usize,
) -> Value {
    let mut map = BTreeMap::<String, &Value>::new();
    let mut order = Vec::new();
    for page in pages {
        let id = client::scalar(&page["id"]);
        if !map.contains_key(&id) {
            order.push(id.clone());
        }
        map.insert(id, page);
    }
    let mut children = BTreeMap::<String, Vec<String>>::new();
    let mut roots = Vec::new();
    for id in &order {
        let page = map[id];
        // Source only joins string parent IDs against string keys.
        if let Some(parent) = page["parentId"].as_str().filter(|p| map.contains_key(*p)) {
            children.entry(parent.into()).or_default().push(id.clone());
        } else {
            roots.push(id.clone());
        }
    }
    let sort = |ids: &mut Vec<String>| {
        ids.sort_by(|a, b| text(map[a], "title").cmp(text(map[b], "title")))
    };
    sort(&mut roots);
    for ids in children.values_mut() {
        sort(ids);
    }
    fn node(
        id: &str,
        depth: usize,
        max: usize,
        domain: &str,
        map: &BTreeMap<String, &Value>,
        children: &BTreeMap<String, Vec<String>>,
    ) -> Value {
        let page = map[id];
        let kids = children.get(id).map(Vec::as_slice).unwrap_or(&[]);
        let mut result = json!({"page_id":id,"title":page["title"].as_str().unwrap_or(""),"parent_id":page["parentId"],"has_children":!kids.is_empty(),"children":[],"depth":depth,"web_url":page["_links"]["webui"].as_str().filter(|s|!s.is_empty()).map(|path|format!("https://{domain}{path}"))});
        if depth >= max {
            if !kids.is_empty() {
                result["children_truncated"] = json!(true);
            }
        } else {
            result["children"] = json!(
                kids.iter()
                    .map(|id| node(id, depth + 1, max, domain, map, children))
                    .collect::<Vec<_>>()
            );
        }
        result
    }
    json!({"space_key":space_key,"space_name":space_name,"root_pages":roots.iter().map(|id|node(id,0,max_depth,domain,&map,&children)).collect::<Vec<_>>(),"total_pages":map.len()})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roots_orphans_and_depth_preserve_source_projection() {
        let tree = page_tree(
            "a.atlassian.net",
            "K",
            "Name",
            &[
                json!({"id":"1","title":"Z"}),
                json!({"id":"2","title":"child","parentId":"1","_links":{"webui":"/wiki/x"}}),
                json!({"id":"3","title":"A","parentId":"missing"}),
            ],
            0,
        );
        assert_eq!(tree["total_pages"], 3);
        assert_eq!(tree["root_pages"][0]["page_id"], "3");
        assert_eq!(tree["root_pages"][1]["children_truncated"], true);
        assert_eq!(tree["root_pages"][1]["has_children"], true);
        assert_eq!(tree["root_pages"][1]["children"], json!([]));
    }
    #[test]
    fn pagination_extracts_cursor_without_following_other_origin() {
        assert_eq!(
            next_cursor(&json!({"_links":{"next":"https://other.invalid/path?cursor=a%2Bb"}})),
            Some("a+b".into())
        );
    }
}

/// Identifier and literal escaping used by the source's CQL query builders.
pub fn escape_cql_value(value: &str) -> Result<&str> {
    if value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        Ok(value)
    } else {
        Err(Error::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid CQL identifier",
        ))
    }
}
pub fn escape_cql_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
fn url_path(url: &str) -> &str {
    let rest = if let Some((_, rest)) = url.split_once("://") {
        let rest = rest.split(['?', '#']).next().unwrap_or("");
        rest.find('/').map(|i| &rest[i..]).unwrap_or("")
    } else if let Some(rest) = url.strip_prefix("//") {
        let rest = rest.split(['?', '#']).next().unwrap_or("");
        rest.find('/').map(|i| &rest[i..]).unwrap_or("")
    } else {
        url
    };
    rest.split(['?', '#']).next().unwrap_or("")
}
pub fn parse_page_id_from_url(url: &str) -> Result<String> {
    let path = url_path(url);
    for pattern in [r"/wiki/spaces/[^/]+/pages/(\d+)", r"/pages/(\d+)"] {
        if let Some(found) = regex::Regex::new(pattern).unwrap().captures(path) {
            return Ok(found[1].into());
        }
    }
    Err(Error::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "Cannot parse Confluence page ID from URL",
    ))
}
pub fn parse_space_key_from_url(url: &str) -> Option<String> {
    regex::Regex::new(r"/wiki/spaces/([^/]+)")
        .unwrap()
        .captures(url_path(url))
        .map(|m| m[1].into())
}
/// Source netloc utility; this is parsing only, never provider-origin admission.
pub fn parse_domain_from_url(url: &str) -> String {
    let rest = url
        .split_once("://")
        .map(|(_, r)| r)
        .or_else(|| url.strip_prefix("//"));
    rest.map(|r| r.split(['/', '?', '#']).next().unwrap_or("").to_owned())
        .unwrap_or_default()
}
#[cfg(test)]
mod url_tests {
    use super::*;
    #[test]
    fn source_url_shapes_and_cql_rules() {
        assert_eq!(
            parse_page_id_from_url("https://a.atlassian.net/wiki/spaces/K/pages/00123/Title?x=1")
                .unwrap(),
            "00123"
        );
        assert_eq!(parse_page_id_from_url("/pages/42").unwrap(), "42");
        assert!(parse_page_id_from_url("https://a.atlassian.net?next=/pages/42").is_err());
        assert!(parse_page_id_from_url("https://a.atlassian.net/?next=/pages/42").is_err());
        assert_eq!(
            parse_space_key_from_url("/wiki/spaces/A%20B/pages/42"),
            Some("A%20B".into())
        );
        assert_eq!(parse_domain_from_url("/wiki/spaces/K"), "");
        assert_eq!(
            parse_domain_from_url("https://a.atlassian.net:443/path"),
            "a.atlassian.net:443"
        );
        assert!(escape_cql_value("A-b_4").is_ok());
        assert!(escape_cql_value("A OR true").is_err());
        assert_eq!(escape_cql_string("a\\b\"c"), "a\\\\b\\\"c");
    }
}
