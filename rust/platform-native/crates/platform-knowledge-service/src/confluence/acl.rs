//! Confluence is the first connector using source permission snapshots. Platform
//! identities are mapped only by an operator-owned artifact, never by accountId.
use super::*;
use query::document_acl::{Acl, Clause};
use serde::Deserialize;
use sqlx::Connection;
use std::collections::BTreeMap;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping {
    schema: String,
    connections: BTreeMap<String, Principals>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Principals {
    tenant_id: String,
    #[serde(default)]
    users: BTreeMap<String, String>,
    #[serde(default)]
    groups: BTreeMap<String, String>,
    space: Clause,
}
fn mapping(id: &str, tenant: &str) -> Result<Principals> {
    use std::io::Read;
    let path = std::env::var_os("KNOWLEDGE_CONNECTOR_PRINCIPALS_FILE")
        .ok_or_else(|| Error::unavailable("Connector principal mapping is not configured"))?;
    let mut bytes = vec![];
    std::fs::File::open(path)
        .map_err(|_| Error::unavailable("Connector principal mapping unavailable"))?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::unavailable("Connector principal mapping unavailable"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::unavailable(
            "Connector principal mapping exceeds budget",
        ));
    }
    let mut m: Mapping = serde_json::from_slice(&bytes)
        .map_err(|_| Error::unavailable("Invalid connector principal mapping"))?;
    if m.schema != "connector-principals/v1" {
        return Err(Error::unavailable(
            "Unsupported connector principal mapping",
        ));
    }
    let p = m
        .connections
        .remove(id)
        .filter(|p| {
            p.tenant_id == tenant && (!p.space.users.is_empty() || !p.space.roles.is_empty())
        })
        .ok_or_else(|| Error::unavailable("Source space read grants are unresolved"))?;
    if p.users.len() + p.groups.len() > 10000 {
        return Err(Error::unavailable(
            "Connector principal mapping exceeds budget",
        ));
    }
    Ok(p)
}
fn restriction(value: &Value, p: &Principals) -> Result<Option<Clause>> {
    let restrictions = &value["restrictions"];
    let users = &restrictions["user"];
    let groups = &restrictions["group"];
    let us = users["results"]
        .as_array()
        .ok_or_else(|| Error::unavailable("Confluence user restrictions unresolved"))?;
    let gs = groups["results"]
        .as_array()
        .ok_or_else(|| Error::unavailable("Confluence group restrictions unresolved"))?;
    // Pagination cannot silently turn a restriction into public access.
    for v in [users, groups] {
        if v["_links"]["next"].as_str().is_some_and(|s| !s.is_empty())
            || v["size"].as_u64().unwrap_or(0) > v["results"].as_array().map_or(0, Vec::len) as u64
        {
            return Err(Error::unavailable(
                "Confluence restriction exceeds the bounded complete snapshot",
            ));
        }
    }
    if us.is_empty() && gs.is_empty() {
        return Ok(None);
    }
    let mut c = Clause::default();
    for u in us {
        let id = text(u, "accountId");
        let mapped = p
            .users
            .get(id)
            .ok_or_else(|| Error::unavailable("Confluence restricted user is unmapped"))?;
        c.users.push(mapped.clone());
    }
    for g in gs {
        let id = text(g, "id");
        let mapped = p
            .groups
            .get(id)
            .ok_or_else(|| Error::unavailable("Confluence restricted group is unmapped"))?;
        c.roles.push(mapped.clone());
    }
    c.users.sort();
    c.users.dedup();
    c.roles.sort();
    c.roles.dedup();
    Ok(Some(c))
}
impl Sources {
    pub(super) async fn throttle(&self, domain: &str) -> Result<()> {
        let now = tokio::time::Instant::now();
        let due = {
            let mut next = self.runtime.rate_next.lock().await;
            if next.len() > 1024 {
                next.retain(|_, t| *t > now);
            }
            let due = next.get(domain).copied().unwrap_or(now).max(now);
            if due.duration_since(now) > std::time::Duration::from_secs(5) {
                return Err(Error::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Connector rate limit queue is full",
                ));
            }
            next.insert(domain.into(), due + std::time::Duration::from_millis(500));
            due
        };
        tokio::time::sleep_until(due).await;
        Ok(())
    }
    async fn legacy_acl(
        &self,
        c: &credentials::Credentials,
        id: &str,
        suffix: &[&str],
        expand: Option<&str>,
    ) -> Result<Value> {
        if !client::cloud_domain(&c.domain) {
            return Err(Error::unavailable("Confluence origin invalid"));
        }
        let mut url = reqwest::Url::parse(&format!("https://{}/wiki/rest/api/content/", c.domain))
            .map_err(|_| Error::unavailable("Confluence origin invalid"))?;
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| Error::unavailable("Confluence origin invalid"))?;
            path.pop_if_empty().push(id);
            for s in suffix {
                path.push(s);
            }
        }
        if let Some(expand) = expand {
            url.query_pairs_mut().append_pair("expand", expand);
        }
        self.throttle(&c.domain).await?;
        let mut response = self
            .client
            .get(url)
            .basic_auth(&c.email, Some(&c.token))
            .send()
            .await
            .map_err(|_| Error::unavailable("Confluence ACL snapshot failed"))?;
        if !response.status().is_success() {
            return Err(Error::unavailable("Confluence ACL snapshot failed"));
        }
        let mut bytes = vec![];
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Error::unavailable("Confluence ACL snapshot failed"))?
        {
            if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err(Error::unavailable("Confluence ACL snapshot exceeds budget"));
            }
            bytes.extend_from_slice(&chunk)
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| Error::unavailable("Confluence ACL snapshot invalid"))
    }
    pub(super) async fn source_acl(
        &self,
        e: &sync::Execution,
        c: &credentials::Credentials,
        page: &Value,
    ) -> Result<Value> {
        let tenant = text(&e.binding, "tenant_id");
        let connection = text(&e.binding, "connection_id");
        let p = mapping(connection, tenant)?;
        let id = client::scalar(&page["id"]);
        let ancestors = self.legacy_acl(c, &id, &[], Some("ancestors")).await?;
        let ancestors = ancestors["ancestors"]
            .as_array()
            .ok_or_else(|| Error::unavailable("Confluence ancestor ACLs unresolved"))?;
        if ancestors.len() > 100 {
            return Err(Error::unavailable("Confluence ancestry exceeds budget"));
        }
        let mut clauses = vec![p.space.clone()];
        let mut revisions = vec![];
        for id in ancestors
            .iter()
            .map(|a| client::scalar(&a["id"]))
            .chain(std::iter::once(id.clone()))
        {
            if id.is_empty() {
                return Err(Error::unavailable("Confluence ancestor identity missing"));
            }
            let value = self
                .legacy_acl(
                    c,
                    &id,
                    &["restriction", "byOperation", "read"],
                    Some("restrictions.user,restrictions.group"),
                )
                .await?;
            if let Some(clause) = restriction(&value, &p)? {
                clauses.push(clause)
            }
            revisions.push(json!({"page":id,"restrictions":value}));
        }
        let acl = Acl {
            schema: "connector-acl/v1".into(),
            verified: true,
            tenant_id: tenant.into(),
            connection_id: connection.into(),
            source_revision: ingest::sha256(ingest::canonical_json(&json!(revisions)).as_bytes()),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            clauses,
        };
        serde_json::to_value(acl)
            .map_err(|_| Error::unavailable("Confluence ACL serialization failed"))
    }
    pub(super) async fn apply_acl(
        &self,
        e: &sync::Execution,
        page: &str,
        acl: &Value,
    ) -> Result<()> {
        let dataset = text(&e.binding, "dataset_id");
        let mut lease = self
            .ingestion
            .store
            .dataset_delete_lease(dataset)
            .await
            .map_err(|_| Error::conflict("Connector ACL publication is busy"))?;
        let mut tx = lease.connection().begin().await?;
        ai_platform_knowledge_domain::database::bind_current(&mut tx).await?;
        self.guard_on(e, &mut tx).await?;
        let current: Value = sqlx::query_scalar(
            "SELECT to_jsonb(d) FROM knowledge.datasets d WHERE dataset_id=$1 FOR UPDATE",
        )
        .bind(dataset)
        .fetch_one(&mut *tx)
        .await?;
        if current["content_revision"].as_i64().unwrap_or(-1) < 0 {
            return Err(Error::conflict(
                "Connector ACL publication waits for index recovery",
            ));
        }
        let changed=sqlx::query("UPDATE knowledge.documents d SET metadata=jsonb_set(COALESCE(d.metadata,'{}'::jsonb),'{_connector_acl}',$3::jsonb,TRUE),updated_at=NOW() FROM confluence_pages p WHERE p.document_id=d.document_id AND p.binding_id=$1 AND p.page_id=$2 AND d.dataset_id=$4 AND d.metadata->'_connector_acl' IS DISTINCT FROM $3::jsonb").bind(text(&e.binding,"binding_id")).bind(page).bind(acl).bind(dataset).execute(&mut *tx).await?.rows_affected();
        if changed > 0 {
            sqlx::query("UPDATE knowledge.datasets SET content_revision=content_revision+1,updated_at=NOW() WHERE dataset_id=$1").bind(dataset).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        lease
            .release()
            .await
            .map_err(|_| Error::unavailable("Connector ACL lease release failed"))?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unresolved_or_paginated_permissions_cannot_become_public() {
        let p = Principals {
            tenant_id: "t".into(),
            users: BTreeMap::new(),
            groups: BTreeMap::new(),
            space: Clause {
                users: vec!["owner".into()],
                roles: vec![],
            },
        };
        assert!(restriction(&json!({}), &p).is_err());
        let mut r = json!({"restrictions":{"user":{"results":[],"size":0},"group":{"results":[],"size":0}}});
        assert!(restriction(&r, &p).unwrap().is_none());
        r["restrictions"]["user"]["results"] = json!([{"accountId":"unknown"}]);
        assert!(restriction(&r, &p).is_err());
        r["restrictions"]["user"]["results"] = json!([]);
        r["restrictions"]["user"]["_links"] = json!({"next":"more"});
        assert!(restriction(&r, &p).is_err());
    }
}
