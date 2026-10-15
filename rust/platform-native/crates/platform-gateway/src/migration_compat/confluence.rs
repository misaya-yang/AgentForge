//! Gateway-only Confluence credentials, pinned transport and durable write receipts.
use super::{
    capability::{CapabilityAuthority, CapabilityScope, body_hash},
    common::{CompatError, object, text},
};
use crate::{
    connector_principal_store::ConnectorPrincipalRepository,
    connector_user_service::ConnectorService,
    connector_user_types::ConnectorUser,
    mcp_gateway_broker::McpSecretResolver,
    mcp_http::{McpDnsResolver, NativeMcpDnsResolver, select_pinned_address},
    mcp_registry_store::McpContext,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, Method},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
const PENDING: &str = "ai-platform/capability-broker-dispatch/v1";
const RECEIPT: &str = "ai-platform/durable-capability-receipt/v1";
const FAILURE_NOTE: &str = " | ⚠️ This call FAILED. Do not tell the user the action succeeded — explain what went wrong and what they should do next.";
fn failure(status: u16, message: &str) -> CompatError {
    CompatError::new(status, format!("{message}{FAILURE_NOTE}"))
}
fn numeric(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|c| c.is_ascii_digit())
}
fn space_key(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255 && value.bytes().all(|c| c.is_ascii_alphanumeric())
}
fn domain(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
}
fn valid_binding(binding: &Value, write: bool) -> bool {
    if !binding.is_object()
        || text(binding, "provider") != "confluence"
        || text(binding, "tool_name")
            != if write {
                "confluence_write"
            } else {
                "confluence_read"
            }
        || !matches!(
            text(binding, "channel"),
            "preview" | "hosted" | "hosted_private" | "hosted_public" | "embed" | "api" | "builtin"
        )
    {
        return false;
    }
    if binding.as_object().unwrap().keys().any(|k| {
        !matches!(
            k.as_str(),
            "binding_type" | "provider" | "tool_name" | "principal_type" | "grant_id" | "channel"
        )
    }) {
        return false;
    }
    match text(binding, "binding_type") {
        "catalog" => binding["principal_type"].is_null() && binding["grant_id"].is_null(),
        "grant" => {
            matches!(
                text(binding, "principal_type"),
                "service_account" | "user_delegated"
            ) && uuid::Uuid::parse_str(text(binding, "grant_id")).is_ok()
        }
        _ => false,
    }
}
pub struct ConfluenceBroker {
    pub authority: Arc<CapabilityAuthority>,
    pub principals: Arc<ConnectorPrincipalRepository>,
    pub connectors: Arc<ConnectorService>,
    pub secrets: Arc<dyn McpSecretResolver>,
    pub enabled: bool,
}
struct Connection {
    client: reqwest::Client,
    root: String,
    authorization: String,
}
impl ConfluenceBroker {
    async fn connection(
        &self,
        scope: &CapabilityScope,
        binding: &Value,
    ) -> Result<Connection, CompatError> {
        let channel = text(binding, "channel");
        let context = McpContext {
            tenant_id: scope.tenant_id.clone(),
            user_id: scope.user_id.clone(),
            authenticated: !matches!(channel, "hosted_public" | "embed"),
        };
        let (target, authorization) = if text(binding, "binding_type") == "catalog" {
            self.principals
                .authorize_connector_catalog(
                    &context,
                    "confluence",
                    text(binding, "tool_name"),
                    channel,
                )
                .await
                .map_err(|_| CompatError::new(403, "confluence connector authorization denied"))?;
            let credential = self
                .connectors
                .resolve_catalog_credential(
                    &ConnectorUser {
                        tenant_id: scope.tenant_id.clone(),
                        user_id: scope.user_id.clone(),
                    },
                    "confluence",
                )
                .await
                .map_err(|_| CompatError::new(503, "confluence credential unavailable"))?;
            let cloud = text(&credential.provider_metadata, "cloud_id").trim();
            let target = if !cloud.is_empty() {
                if cloud.len() > 255
                    || !cloud
                        .bytes()
                        .all(|v| v.is_ascii_alphanumeric() || b"._:-".contains(&v))
                {
                    return Err(CompatError::new(503, "confluence credential unavailable"));
                }
                format!("api.atlassian.com/ex/confluence/{cloud}")
            } else {
                let site = text(&credential.provider_metadata, "site_url");
                let site = if site.is_empty() {
                    text(&credential.provider_metadata, "domain")
                } else {
                    site
                };
                reqwest::Url::parse(&if site.contains("://") {
                    site.to_owned()
                } else {
                    format!("https://{site}")
                })
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .ok_or_else(|| CompatError::new(503, "confluence credential unavailable"))?
            };
            if credential.access_token.is_empty() {
                return Err(CompatError::new(503, "confluence credential unavailable"));
            }
            (target, format!("Bearer {}", credential.access_token))
        } else {
            let row = self
                .principals
                .authorize_connector_tool(
                    &context,
                    "confluence",
                    text(binding, "tool_name"),
                    text(binding, "principal_type"),
                    text(binding, "grant_id"),
                    channel,
                )
                .await
                .map_err(|_| CompatError::new(403, "confluence connector authorization denied"))?;
            let token = self
                .secrets
                .resolve(text(&row, "secret_ref"))
                .await
                .map_err(|_| CompatError::new(503, "confluence credential unavailable"))?;
            let metadata = &row["connection_metadata"];
            let domain = text(metadata, "domain").trim();
            let email = text(metadata, "email").trim();
            if !self::domain(domain) || email.is_empty() || token.is_empty() {
                return Err(CompatError::new(503, "confluence credential unavailable"));
            }
            (
                domain.to_owned(),
                format!("Basic {}", STANDARD.encode(format!("{email}:{token}"))),
            )
        };
        let host = target.split('/').next().unwrap_or("");
        if !domain(host) {
            return Err(CompatError::new(503, "confluence destination unavailable"));
        }
        let addresses = NativeMcpDnsResolver
            .resolve(host, 443)
            .await
            .map_err(|_| CompatError::new(503, "confluence destination unavailable"))?;
        let address = select_pinned_address(&addresses)
            .map_err(|_| CompatError::new(403, "confluence destination blocked"))?;
        let client = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .resolve(host, SocketAddr::new(address, 443))
            .build()?;
        Ok(Connection {
            client,
            root: format!("https://{}/wiki/", target.trim_end_matches('/')),
            authorization,
        })
    }
    pub async fn read(&self, headers: &HeaderMap, body: &Value) -> Result<Value, CompatError> {
        validate_envelope(body, false)?;
        let start = Instant::now();
        let scope = self.authority.verify(
            headers,
            "/internal/v2/agent-capabilities/confluence/read",
            body,
        )?;
        if !self.enabled {
            return Err(CompatError::new(404, "confluence capability unavailable"));
        }
        let connection = self.connection(&scope, &body["binding"]).await?;
        let args = &body["arguments"];
        let (result, metadata) = match text(args, "action") {
            "search" => {
                let (cql, strategy) = search_cql(args)?;
                let data = connection
                    .request(
                        Method::GET,
                        "content/search",
                        &[
                            ("cql", cql.clone()),
                            (
                                "limit",
                                args["limit"].as_u64().unwrap_or(10).min(25).to_string(),
                            ),
                            ("expand", "body.view,space,version,ancestors".into()),
                        ],
                        None,
                        false,
                    )
                    .await?;
                let hits = data["results"].as_array().cloned().unwrap_or_default();
                let result = if hits.is_empty() {
                    format!("No pages matched. CQL executed: `{cql}`")
                } else {
                    let mut lines = vec![format!(
                        "Found {} Confluence page(s) (strategy={strategy}, cql=`{cql}`):",
                        hits.len()
                    )];
                    for page in &hits {
                        let parent = page["ancestors"]
                            .as_array()
                            .and_then(|v| v.last())
                            .cloned()
                            .unwrap_or(Value::Null);
                        let parent_line = if parent.is_null() {
                            "Parent: (top-level — space homepage)".into()
                        } else {
                            format!(
                                "Parent: {} (id: {})",
                                text(&parent, "title"),
                                text(&parent, "id")
                            )
                        };
                        lines.push(format!(
                            "\n**{}** (Space: {}, key: {})\nID: {} | URL: {}\n{}\n{}",
                            text(page, "title"),
                            text(&page["space"], "name"),
                            text(&page["space"], "key"),
                            text(page, "id"),
                            text(&page["_links"], "webui"),
                            parent_line,
                            html_text(text(&page["body"]["view"], "value"))
                                .chars()
                                .take(800)
                                .collect::<String>()
                        ));
                    }
                    lines.join("\n---\n")
                };
                (
                    result,
                    json!({"count":hits.len(),"query":text(args,"query"),"cql_used":cql,"strategy":strategy}),
                )
            }
            "read_page" => {
                let mut id = text(args, "page_id").to_owned();
                if id.is_empty() {
                    let url = text(args, "url");
                    if let Some((_, tail)) = url.split_once("/pages/") {
                        id = tail.split('/').next().unwrap_or("").to_owned();
                    } else if let Ok(url) = reqwest::Url::parse(url) {
                        id = url
                            .query_pairs()
                            .find(|(k, _)| k.eq_ignore_ascii_case("pageId"))
                            .map(|(_, v)| v.into_owned())
                            .unwrap_or_default();
                    }
                }
                if !id.is_empty() && !numeric(&id) {
                    return Err(CompatError::new(400, "invalid page_id (must be numeric)"));
                }
                let mut params = vec![("expand", "body.view,space,version,ancestors".into())];
                let path = if id.is_empty() {
                    if text(args, "title").is_empty() {
                        return Err(CompatError::new(
                            400,
                            "`read_page` requires page_id, title, or url",
                        ));
                    }
                    params.push(("title", text(args, "title").into()));
                    params.push(("limit", "1".into()));
                    "content".into()
                } else {
                    format!("content/{id}")
                };
                let data = connection
                    .request(Method::GET, &path, &params, None, false)
                    .await?;
                let page = if id.is_empty() {
                    data["results"]
                        .as_array()
                        .and_then(|v| v.first())
                        .cloned()
                        .unwrap_or(Value::Null)
                } else {
                    data
                };
                if page.is_null() {
                    (
                        format!(
                            "No page found (page_id={id:?}, title={:?}).",
                            text(args, "title")
                        ),
                        json!({"found":false}),
                    )
                } else {
                    let ancestors = page["ancestors"].as_array().cloned().unwrap_or_default();
                    let parent = ancestors.last().cloned().unwrap_or(Value::Null);
                    let parent_line = if parent.is_null() {
                        "Parent: (none — this is a space homepage or top-level page)\n".into()
                    } else {
                        format!(
                            "Parent: {} (id: {})\nPath: {} > {}\n",
                            text(&parent, "title"),
                            text(&parent, "id"),
                            ancestors
                                .iter()
                                .map(|a| format!("{} ({})", text(a, "title"), text(a, "id")))
                                .collect::<Vec<_>>()
                                .join(" > "),
                            text(&page, "title")
                        )
                    };
                    let content = html_text(text(&page["body"]["view"], "value"));
                    (
                        format!(
                            "**{}** (space: {}, id: {})\nURL: {}{}\n{}Last modified: {}\n\n{}",
                            text(&page, "title"),
                            text(&page["space"], "name"),
                            text(&page, "id"),
                            text(&page["_links"], "base"),
                            text(&page["_links"], "webui"),
                            parent_line,
                            text(&page["version"], "when"),
                            content
                        ),
                        json!({"found":true,"page_id":page["id"],"parent_id":text(&parent,"id"),"space_key":page["space"]["key"],"chars":content.chars().count()}),
                    )
                }
            }
            "list_children" => {
                let id = text(args, "page_id");
                if !numeric(id) {
                    return Err(CompatError::new(400, "`list_children` requires page_id"));
                }
                let data = connection
                    .request(
                        Method::GET,
                        &format!("content/{id}/child/page"),
                        &[
                            (
                                "limit",
                                args["limit"].as_u64().unwrap_or(25).min(100).to_string(),
                            ),
                            ("expand", "version,_links".into()),
                        ],
                        None,
                        false,
                    )
                    .await?;
                let children = data["results"].as_array().cloned().unwrap_or_default();
                let result = if children.is_empty() {
                    format!("Page {id} has no direct child pages.")
                } else {
                    let mut lines =
                        vec![format!("Found {} child page(s) of {id}:", children.len())];
                    for child in &children {
                        lines.push(format!(
                            "- **{}** (id: {})\n  URL: {}",
                            text(child, "title"),
                            text(child, "id"),
                            text(&child["_links"], "webui")
                        ));
                    }
                    lines.join("\n")
                };
                (result, json!({"count":children.len(),"page_id":id}))
            }
            "list_spaces" => {
                let mut params = vec![
                    (
                        "limit",
                        args["limit"].as_u64().unwrap_or(50).min(250).to_string(),
                    ),
                    ("include-icon", "false".into()),
                    ("description-format", "plain".into()),
                ];
                for field in ["keys", "labels"] {
                    if let Some(list) = args[field].as_array().filter(|v| !v.is_empty()) {
                        params.push((
                            field,
                            list.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(","),
                        ));
                    }
                }
                if !text(args, "space_type").is_empty() {
                    params.push(("type", text(args, "space_type").into()));
                }
                let data = connection
                    .request(Method::GET, "v2/spaces", &params, None, false)
                    .await?;
                let query = text(args, "query");
                let spaces = data["results"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|s| {
                        query.is_empty()
                            || format!(
                                "{} {} {}",
                                text(s, "name"),
                                text(s, "key"),
                                text(s, "description")
                            )
                            .to_lowercase()
                            .contains(&query.to_lowercase())
                    })
                    .collect::<Vec<_>>();
                let mut lines = vec![format!(
                    "Found {} Confluence space(s){}:",
                    spaces.len(),
                    if query.is_empty() {
                        String::new()
                    } else {
                        format!(" matching '{query}'")
                    }
                )];
                for space in spaces.iter().take(25) {
                    lines.push(format!(
                        "- **{}** (key: `{}`, id: {}, type: {})\n  URL: {}",
                        text(space, "name"),
                        text(space, "key"),
                        text(space, "id"),
                        text(space, "type"),
                        text(&space["_links"], "webui")
                    ));
                }
                if spaces.is_empty() {
                    lines = vec![format!(
                        "No Confluence spaces matched query={query:?}. Try listing without a query, or different keywords."
                    )];
                }
                (
                    lines.join("\n"),
                    json!({"count":spaces.len(),"query":args["query"],"shown":spaces.len().min(25)}),
                )
            }
            "get_space" => {
                let id = text(args, "space_id");
                let key = text(args, "space_key");
                if id.is_empty() && key.is_empty() {
                    return Err(CompatError::new(
                        400,
                        "`get_space` requires space_id or space_key",
                    ));
                }
                if !id.is_empty() && !numeric(id) {
                    return Err(CompatError::new(400, "invalid space_id (must be numeric)"));
                }
                if !key.is_empty() && !space_key(key) {
                    return Err(CompatError::new(400, "invalid space_key"));
                }
                let path = if id.is_empty() {
                    "v2/spaces".into()
                } else {
                    format!("v2/spaces/{id}")
                };
                let mut params = vec![
                    ("description-format", "plain".into()),
                    ("include-icon", "false".into()),
                ];
                if !key.is_empty() {
                    params.push(("keys", key.into()));
                    params.push(("limit", "1".into()));
                }
                let data = connection
                    .request(Method::GET, &path, &params, None, false)
                    .await?;
                let space = if !key.is_empty() {
                    data["results"]
                        .as_array()
                        .and_then(|v| v.first())
                        .cloned()
                        .unwrap_or(Value::Null)
                } else {
                    data
                };
                if space.is_null() {
                    (
                        format!("No space found (id={id:?}, key={key:?})."),
                        json!({"found":false}),
                    )
                } else {
                    (
                        format!(
                            "**{}** (key: `{}`, id: {}, type: {})\nURL: {}\nHomepage ID: {}\nCreated: {}\nDescription: {}",
                            text(&space, "name"),
                            text(&space, "key"),
                            text(&space, "id"),
                            text(&space, "type"),
                            text(&space["_links"], "webui"),
                            text(&space, "homepageId"),
                            text(&space, "createdAt"),
                            text(&space, "description")
                        ),
                        json!({"found":true,"space_id":space["id"],"space_key":space["key"]}),
                    )
                }
            }
            _ => return Err(CompatError::new(422, "invalid confluence read action")),
        };
        Ok(
            json!({"success":true,"result":result,"duration_ms":start.elapsed().as_secs_f64()*1000.0,"metadata":metadata}),
        )
    }
    pub async fn write(&self, headers: &HeaderMap, body: &Value) -> Result<Value, CompatError> {
        validate_envelope(body, true)?;
        let scope = self.authority.verify(
            headers,
            "/internal/v2/agent-capabilities/confluence/write",
            body,
        )?;
        if !self.enabled {
            return Err(CompatError::new(404, "confluence capability unavailable"));
        }
        let execution = self
            .authority
            .active_execution(
                &scope,
                "confluence_write",
                "write",
                "consumed",
                text(body, "arguments_hash"),
            )
            .await?;
        if !execution["result_summary"].is_null() {
            return durable_result(&execution["result_summary"])
                .ok_or_else(|| CompatError::new(502, "confluence write outcome unknown"));
        }
        let connection = self.connection(&scope, &body["binding"]).await?;
        let args = &body["arguments"];
        let action = text(args, "action");
        let id = text(args, "page_id");
        if action != "create_page" && !numeric(id) {
            return Err(CompatError::new(
                400,
                format!("`{action}` requires numeric page_id"),
            ));
        }
        let (method, path, payload, fallback) = match action {
            "create_page" => {
                let title = text(args, "title").trim();
                if !space_key(text(args, "space_key"))
                    || title.is_empty()
                    || !args["content"].is_string()
                {
                    return Err(CompatError::new(
                        400,
                        "`create_page` requires space_key, title, and content",
                    ));
                }
                let mut payload = json!({"type":"page","title":title,"space":{"key":args["space_key"]},"body":{"storage":{"value":markdown_to_storage(text(args,"content")),"representation":"storage"}}});
                let parent = text(args, "parent_id");
                if !parent.is_empty() {
                    if !numeric(parent) {
                        return Err(CompatError::new(400, "invalid parent_id"));
                    }
                    payload["ancestors"] = json!([{"id":parent}]);
                }
                (Method::POST, "content".into(), Some(payload), "")
            }
            "update_page" | "find_replace" | "move_page" => {
                if action == "update_page" && !args["content"].is_string() {
                    return Err(CompatError::new(400, "`update_page` requires content"));
                }
                if action == "find_replace"
                    && (text(args, "find").is_empty() || !args["replace"].is_string())
                {
                    return Err(CompatError::new(
                        400,
                        "`find_replace` requires page_id, find, and replace",
                    ));
                }
                let target = text(args, "target_parent_id");
                if action == "move_page" && (!numeric(target) || target == id) {
                    return Err(CompatError::new(400, "invalid target_parent_id"));
                }
                let current = connection
                    .current_page(
                        id,
                        if action == "find_replace" {
                            "body.storage,version,space"
                        } else if action == "move_page" {
                            "version,ancestors,space"
                        } else {
                            "version,space"
                        },
                    )
                    .await?;
                let title = if action == "update_page" && !text(args, "title").is_empty() {
                    text(args, "title")
                } else {
                    text(&current, "title")
                };
                let mut payload = json!({"version":{"number":current["version"]["number"].as_i64().unwrap_or(0)+1},"title":title,"type":"page"});
                if action == "move_page" {
                    let target_page = connection.current_page(target, "version,space").await?;
                    if current["ancestors"]
                        .as_array()
                        .and_then(|v| v.last())
                        .is_some_and(|p| text(p, "id") == target)
                    {
                        return Err(CompatError::new(409, "page is already under target parent"));
                    }
                    if !text(&current["space"], "key").is_empty()
                        && !text(&target_page["space"], "key").is_empty()
                        && current["space"]["key"] != target_page["space"]["key"]
                    {
                        return Err(CompatError::new(409, "cross-space moves are not supported"));
                    }
                    payload["ancestors"] = json!([{"id":target}]);
                } else {
                    let content = if action == "find_replace" {
                        let content = current["body"]["storage"]["value"]
                            .as_str()
                            .ok_or_else(|| failure(424, "confluence response invalid"))?;
                        let count = content.matches(text(args, "find")).count();
                        if count != 1 {
                            return Err(CompatError::new(
                                400,
                                format!(
                                    "`find` must match exactly once on page {id} (matches={count})"
                                ),
                            ));
                        }
                        content.replacen(
                            text(args, "find"),
                            &if args["raw_html"].as_bool() == Some(true) {
                                text(args, "replace").to_owned()
                            } else {
                                escape_storage(text(args, "replace"))
                            },
                            1,
                        )
                    } else {
                        markdown_to_storage(text(args, "content"))
                    };
                    payload["body"] =
                        json!({"storage":{"value":content,"representation":"storage"}});
                }
                (Method::PUT, format!("content/{id}"), Some(payload), id)
            }
            "comment" => {
                if text(args, "body").is_empty() {
                    return Err(CompatError::new(400, "`comment` requires page_id and body"));
                }
                (
                    Method::POST,
                    "content".into(),
                    Some(
                        json!({"type":"comment","container":{"id":id,"type":"page"},"body":{"storage":{"value":markdown_to_storage(text(args,"body")),"representation":"storage"}}}),
                    ),
                    id,
                )
            }
            "delete_page" => (Method::DELETE, format!("content/{id}"), None, id),
            _ => return Err(CompatError::new(422, "invalid confluence write action")),
        };
        let pending = json!({"schema_version":PENDING,"capability_id":"confluence_write"});
        let changed=sqlx::query("UPDATE assistant_capability_executions SET result_summary=$2,updated_at=NOW() WHERE execution_id=$1::uuid AND result_summary IS NULL AND status IN ('dispatched','running')").bind(&scope.execution_id).bind(pending).execute(&self.authority.pool).await?;
        if changed.rows_affected() != 1 {
            return Err(CompatError::new(502, "confluence write outcome unknown"));
        }
        let outcome = connection
            .request(method, &path, &[], payload.as_ref(), true)
            .await;
        let data = match outcome {
            Ok(data) => data,
            Err(error) => {
                if (400..500).contains(&error.status) {
                    let changed=sqlx::query("UPDATE assistant_capability_executions SET result_summary=NULL,updated_at=NOW() WHERE execution_id=$1::uuid AND result_summary->>'schema_version'=$2").bind(&scope.execution_id).bind(PENDING).execute(&self.authority.pool).await?;
                    if changed.rows_affected() != 1 {
                        return Err(CompatError::new(502, "confluence write outcome unknown"));
                    }
                }
                return Err(error);
            }
        };
        let external = if text(&data, "id").is_empty() {
            fallback
        } else {
            text(&data, "id")
        };
        let url = format!(
            "{}{}",
            text(&data["_links"], "base"),
            text(&data["_links"], "webui")
        );
        let url = if url.starts_with("https://") {
            url
        } else {
            String::new()
        };
        let result = json!({"receipt_id":uuid::Uuid::parse_str(&scope.execution_id).map_err(|_|CompatError::new(403,"execution identity is invalid"))?.to_string(),"capability_id":"confluence_write","external_id":if external.is_empty(){Value::Null}else{json!(external)},"external_url":if url.is_empty(){Value::Null}else{json!(url)},"artifacts":[]});
        let receipt =
            json!({"schema_version":RECEIPT,"capability_id":"confluence_write","result":result});
        let changed=sqlx::query("UPDATE assistant_capability_executions SET result_summary=$2,updated_at=NOW() WHERE execution_id=$1::uuid AND result_summary->>'schema_version'=$3").bind(&scope.execution_id).bind(&receipt).bind(PENDING).execute(&self.authority.pool).await?;
        if changed.rows_affected() != 1 {
            return Err(CompatError::new(502, "confluence write outcome unknown"));
        }
        durable_result(&receipt)
            .ok_or_else(|| CompatError::new(502, "confluence write outcome unknown"))
    }
}
impl Connection {
    async fn request(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        body: Option<&Value>,
        write: bool,
    ) -> Result<Value, CompatError> {
        let url = if !write && path.starts_with("v2/") {
            format!("{}api/v2/{}", self.root, path.trim_start_matches("v2/"))
        } else {
            format!("{}rest/api/{}", self.root, path.trim_start_matches('/'))
        };
        let mut request = self
            .client
            .request(method, url)
            .header("authorization", &self.authorization)
            .header("accept", "application/json")
            .query(params);
        if let Some(body) = body {
            request = request.json(body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| failure(502, "confluence upstream unavailable"))?;
        let status = response.status().as_u16();
        if status >= 400 {
            return Err(failure(
                if write && status < 500 { status } else { 502 },
                &format!(
                    "Confluence API error (HTTP {status}) for action='{}'.",
                    if write {
                        "confluence_write"
                    } else {
                        "confluence_read"
                    }
                ),
            ));
        }
        if status == 204 {
            return Ok(json!({}));
        }
        if response.content_length().is_some_and(|v| v > 2_000_000) {
            return Err(CompatError::new(502, "confluence response too large"));
        }
        let mut bytes = vec![];
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| failure(502, "confluence upstream unavailable"))?
        {
            if bytes.len() + chunk.len() > 2_000_000 {
                return Err(CompatError::new(502, "confluence response too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        if write && bytes.is_empty() {
            return Ok(json!({}));
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| failure(502, "confluence response invalid"))?;
        if !value.is_object() {
            return Err(failure(502, "confluence response invalid"));
        }
        Ok(value)
    }
    async fn current_page(&self, id: &str, expand: &str) -> Result<Value, CompatError> {
        let page = self
            .request(
                Method::GET,
                &format!("content/{id}"),
                &[("expand", expand.into())],
                None,
                true,
            )
            .await
            .map_err(|e| {
                if e.status == 502 {
                    failure(424, "confluence write preflight failed")
                } else {
                    e
                }
            })?;
        if page["version"]["number"].as_i64().is_none_or(|v| v < 1) || !page["title"].is_string() {
            return Err(failure(424, "confluence response invalid"));
        }
        Ok(page)
    }
}
pub fn durable_result(value: &Value) -> Option<Value> {
    if text(value, "schema_version") != RECEIPT
        || text(value, "capability_id") != "confluence_write"
    {
        return None;
    }
    let result = &value["result"];
    if result.as_object()?.len() != 5
        || text(result, "capability_id") != "confluence_write"
        || text(result, "receipt_id").is_empty()
        || result["artifacts"] != json!([])
    {
        return None;
    }
    if !result["external_id"].is_null() && !result["external_id"].is_string() {
        return None;
    }
    if !result["external_url"].is_null() && !text(result, "external_url").starts_with("https://") {
        return None;
    }
    Some(
        json!({"receipt_id":result["receipt_id"],"external_id":text(result,"external_id"),"external_url":text(result,"external_url"),"artifacts":[]}),
    )
}
fn validate_envelope(body: &Value, write: bool) -> Result<(), CompatError> {
    let bad = || CompatError::new(422, "invalid confluence request");
    let root = body.as_object().ok_or_else(bad)?;
    if root
        .keys()
        .any(|key| !matches!(key.as_str(), "arguments" | "binding" | "arguments_hash"))
        || (!write && root.contains_key("arguments_hash"))
        || !valid_binding(&body["binding"], write)
    {
        return Err(bad());
    }
    let args = body["arguments"].as_object().ok_or_else(bad)?;
    let fields = if write {
        vec![
            "action",
            "page_id",
            "space_key",
            "title",
            "content",
            "parent_id",
            "target_parent_id",
            "find",
            "replace",
            "raw_html",
            "body",
        ]
    } else {
        vec![
            "action",
            "query",
            "page_id",
            "title",
            "url",
            "space_key",
            "space_id",
            "keys",
            "space_type",
            "labels",
            "fields",
            "author",
            "updated_since",
            "under_page_id",
            "cql",
            "limit",
        ]
    };
    if args.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err(bad());
    }
    for (key, value) in args {
        if value.is_null() {
            continue;
        }
        let limit = match key.as_str() {
            "content" => 100_000,
            "find" | "replace" | "body" => 20_000,
            "author" => 1024,
            "page_id" | "space_id" | "parent_id" | "target_parent_id" | "under_page_id"
            | "updated_since" => 64,
            "space_key" => 255,
            _ => 4096,
        };
        if !matches!(
            key.as_str(),
            "keys" | "labels" | "fields" | "limit" | "raw_html"
        ) && value.as_str().is_none_or(|v| v.chars().count() > limit)
        {
            return Err(bad());
        }
    }
    if args
        .get("limit")
        .is_some_and(|v| !v.is_null() && v.as_u64().is_none_or(|n| !(1..=250).contains(&n)))
    {
        return Err(bad());
    }
    if write && text(body, "arguments_hash") != format!("sha256:{}", body_hash(&body["arguments"]))
    {
        return Err(bad());
    }
    Ok(())
}
pub fn search_cql(args: &Value) -> Result<(String, String), CompatError> {
    let controls = |v: &str| v.chars().any(|c| c < ' ' || c == '\u{7f}');
    let escape = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
    let raw = text(args, "cql");
    if !raw.is_empty() {
        if controls(raw) {
            return Err(CompatError::new(
                400,
                "raw cql must be single-line, no control chars",
            ));
        }
        return Ok((raw.into(), "raw_cql".into()));
    }
    if ["query", "author", "updated_since", "under_page_id"]
        .iter()
        .all(|key| text(args, key).is_empty())
    {
        return Err(CompatError::new(
            400,
            "`search` needs at least one of: query, cql, author, updated_since, or under_page_id",
        ));
    }
    let mut parts = vec!["type=page".into()];
    let query = text(args, "query");
    let mut strategy = "filter_only".to_owned();
    if !query.is_empty() {
        if controls(query) {
            return Err(CompatError::new(400, "query contains control characters"));
        }
        let fields = args["fields"]
            .as_array()
            .filter(|v| !v.is_empty())
            .cloned()
            .unwrap_or_else(|| vec![json!("title"), json!("text")]);
        let query = escape(query);
        let mut clauses = vec![];
        if fields.contains(&json!("title")) {
            let title = query
                .chars()
                .map(|c| {
                    if "【】「」『』[]()（）".contains(c) {
                        ' '
                    } else {
                        c
                    }
                })
                .collect::<String>();
            clauses.push(format!("title ~ \"{}\"", title.trim()));
        }
        if fields.contains(&json!("text")) {
            clauses.push(format!("text ~ \"{query}\""));
        }
        strategy = if clauses.len() == 2 {
            "title+text".into()
        } else {
            fields.first().and_then(Value::as_str).unwrap_or("").into()
        };
        parts.push(format!("({})", clauses.join(" OR ")));
    }
    let key = text(args, "space_key");
    if !key.is_empty() {
        if !space_key(key) {
            return Err(CompatError::new(400, "invalid space_key"));
        }
        parts.push(format!("space={key}"));
    }
    let ancestor = text(args, "under_page_id");
    if !ancestor.is_empty() {
        if !numeric(ancestor) {
            return Err(CompatError::new(
                400,
                "invalid under_page_id (must be numeric)",
            ));
        }
        parts.push(format!("ancestor={ancestor}"));
    }
    let author = text(args, "author");
    if !author.is_empty() {
        if controls(author) {
            return Err(CompatError::new(400, "author contains control characters"));
        }
        parts.push(format!("creator = \"{}\"", escape(author)));
    }
    let since = text(args, "updated_since");
    if !since.is_empty() {
        let pattern = match since.len() {
            10 => "0000-00-00",
            16 => "0000-00-00T00:00",
            19 => "0000-00-00T00:00:00",
            _ => "",
        };
        if pattern.is_empty()
            || !since.bytes().zip(pattern.bytes()).all(|(a, b)| {
                if b == b'0' {
                    a.is_ascii_digit()
                } else {
                    a == b
                }
            })
        {
            return Err(CompatError::new(
                400,
                "updated_since must be ISO date (YYYY-MM-DD)",
            ));
        }
        parts.push(format!("lastModified >= \"{since}\""));
    }
    Ok((parts.join(" AND "), strategy))
}
pub fn escape_storage(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn inline(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while !rest.is_empty() {
        let marker = if rest.starts_with("**") {
            Some(("**", "strong"))
        } else if rest.starts_with('`') {
            Some(("`", "code"))
        } else if rest.starts_with('*') {
            Some(("*", "em"))
        } else {
            None
        };
        if let Some((marker, tag)) = marker {
            if let Some(end) = rest[marker.len()..].find(marker) {
                let content = &rest[marker.len()..marker.len() + end];
                if !content.is_empty() && !content.contains('\n') {
                    out.push_str(&format!("<{tag}>{}</{tag}>", escape_storage(content)));
                    rest = &rest[marker.len() * 2 + end..];
                    continue;
                }
            }
        }
        if rest.starts_with('[') {
            if let Some(end) = rest.find("](") {
                if let Some(close) = rest[end + 2..].find(')') {
                    let label = &rest[1..end];
                    let target = rest[end + 2..end + 2 + close]
                        .split_whitespace()
                        .next()
                        .unwrap_or("");
                    if target.starts_with("https://")
                        || target.starts_with("http://")
                        || target.starts_with('/')
                    {
                        out.push_str(&format!(
                            "<a href=\"{}\">{}</a>",
                            escape_storage(target)
                                .replace('"', "&quot;")
                                .replace('\'', "&#x27;"),
                            escape_storage(label)
                        ));
                    } else {
                        out.push_str(&escape_storage(label));
                    }
                    rest = &rest[end + 3 + close..];
                    continue;
                }
            }
        }
        let c = rest.chars().next().unwrap();
        out.push_str(&escape_storage(&c.to_string()));
        rest = &rest[c.len_utf8()..];
    }
    out
}
pub fn markdown_to_storage(value: &str) -> String {
    if value.is_empty() {
        return "<p></p>".into();
    }
    let mut content = value.trim().to_owned();
    let markdown = content.contains("**")
        || content.contains('`')
        || content
            .lines()
            .any(|line| line.trim_start().starts_with(['#', '-', '*']))
        || content.contains("](");
    if content.starts_with('<') && content.ends_with('>') && !markdown {
        return value.into();
    }
    if markdown {
        for tag in ["p", "div", "span", "br"] {
            for token in [format!("<{tag}>"), format!("</{tag}>"), format!("<{tag}/>")] {
                content = content.replace(&token, "\n");
            }
        }
    }
    let mut blocks = vec![];
    while let Some(start) = content.find("```") {
        let Some(newline) = content[start + 3..].find('\n') else {
            break;
        };
        let body_start = start + 4 + newline;
        let Some(end) = content[body_start..].find("```") else {
            break;
        };
        let language = &content[start + 3..body_start - 1];
        let body = content[body_start..body_start + end].replace("]]>", "]]]]><![CDATA[>");
        let block = format!(
            "<ac:structured-macro ac:name=\"code\">{}<ac:plain-text-body><![CDATA[{body}]]></ac:plain-text-body></ac:structured-macro>",
            if language.is_empty() {
                String::new()
            } else {
                format!(
                    "<ac:parameter ac:name=\"language\">{}</ac:parameter>",
                    escape_storage(language)
                )
            }
        );
        let placeholder = format!("\0FENCED{}\0", blocks.len());
        blocks.push(block);
        content.replace_range(start..body_start + end + 3, &placeholder);
    }
    let mut output = String::new();
    for block in content.split("\n\n").filter(|v| !v.trim().is_empty()) {
        let block = block.trim_end();
        if let Some(index) = block
            .strip_prefix("\0FENCED")
            .and_then(|v| v.strip_suffix('\0'))
            .and_then(|v| v.parse::<usize>().ok())
        {
            if let Some(code) = blocks.get(index) {
                output.push_str(code);
                continue;
            }
        }
        let lines = block.lines().collect::<Vec<_>>();
        let heading = block.chars().take_while(|c| *c == '#').count();
        if lines.len() == 1
            && (1..=6).contains(&heading)
            && block
                .as_bytes()
                .get(heading)
                .is_some_and(u8::is_ascii_whitespace)
        {
            output.push_str(&format!(
                "<h{heading}>{}</h{heading}>",
                inline(block[heading..].trim_start())
            ));
            continue;
        }
        let unordered = lines
            .iter()
            .all(|line| line.trim_start().starts_with("- ") || line.trim_start().starts_with("* "));
        let ordered = lines.iter().all(|line| {
            line.trim_start()
                .split_once(". ")
                .is_some_and(|(number, _)| {
                    !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
                })
        });
        if unordered || ordered {
            let tag = if unordered { "ul" } else { "ol" };
            output.push_str(&format!("<{tag}>"));
            for line in lines {
                let value = if unordered {
                    &line.trim_start()[2..]
                } else {
                    line.trim_start().split_once(". ").unwrap().1
                };
                output.push_str(&format!("<li>{}</li>", inline(value)));
            }
            output.push_str(&format!("</{tag}>"));
        } else {
            output.push_str(&format!(
                "<p>{}</p>",
                lines
                    .iter()
                    .map(|line| inline(line))
                    .collect::<Vec<_>>()
                    .join("<br/>")
            ));
        }
    }
    if output.is_empty() {
        "<p></p>".into()
    } else {
        output.replace("</ul><ul>", "").replace("</ol><ol>", "")
    }
}
pub fn html_text(value: &str) -> String {
    let mut output = String::new();
    let mut rest = value;
    let mut skip = 0usize;
    while let Some(start) = rest.find('<') {
        if skip == 0 {
            output.push_str(&rest[..start]);
        }
        let Some(end) = rest[start..].find('>') else {
            rest = "";
            break;
        };
        let tag = rest[start + 1..start + end]
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_lowercase();
        let close = tag.starts_with('/');
        let name = tag.trim_start_matches('/');
        if matches!(name, "script" | "style") {
            if close {
                skip = skip.saturating_sub(1);
            } else {
                skip += 1;
            }
        } else if skip == 0 && matches!(name, "br" | "p" | "div" | "li" | "h1" | "h2" | "h3") {
            output.push('\n');
        }
        rest = &rest[start + end + 1..];
    }
    if skip == 0 {
        output.push_str(rest);
    }
    for (from, to) in [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
    ] {
        output = output.replace(from, to);
    }
    let mut normalized = String::new();
    let mut spaces = false;
    let mut newlines = 0;
    for c in output.trim().chars() {
        if c == ' ' || c == '\t' {
            if !spaces {
                normalized.push(' ');
            }
            spaces = true;
        } else if c == '\n' {
            newlines += 1;
            if newlines <= 2 {
                normalized.push(c);
            }
            spaces = false;
        } else {
            normalized.push(c);
            spaces = false;
            newlines = 0;
        }
    }
    if normalized.chars().count() > 20_000 {
        format!(
            "{}\n…[truncated at 20K chars]",
            normalized.chars().take(20_000).collect::<String>()
        )
    } else {
        normalized
    }
}
pub fn router(broker: Arc<ConfluenceBroker>) -> Router {
    Router::new()
        .route(
            "/internal/v2/agent-capabilities/confluence/read",
            post(read),
        )
        .route(
            "/internal/v2/agent-capabilities/confluence/write",
            post(write),
        )
        .with_state(broker)
}
async fn read(
    State(broker): State<Arc<ConfluenceBroker>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    match broker.read(&headers, &body).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => error.into_response(),
    }
}
async fn write(
    State(broker): State<Arc<ConfluenceBroker>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    match broker.write(&headers, &body).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => error.into_response(),
    }
}
