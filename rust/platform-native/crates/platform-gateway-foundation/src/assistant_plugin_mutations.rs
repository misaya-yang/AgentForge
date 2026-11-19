//! Tenant-local optimistic lifecycle writes and their atomic audit receipt.
use super::{CatalogState, authenticate, error, is_name, lifecycle, user_has_required_permissions};
use axum::{Json, extract::{Path, State, rejection::JsonRejection}, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}};
use crate::native_contract::UpdatePlugin;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) async fn update_plugin(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    body: Result<Json<UpdatePlugin>, JsonRejection>,
) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    if !lifecycle::can_manage(&principal.tier, &principal.roles) {
        return error(StatusCode::FORBIDDEN, "Administrator access is required");
    }
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid plugin lifecycle request"),
    };
    if body.expected_tenant_id != principal.tenant_id {
        return error(StatusCode::CONFLICT, "The authenticated tenant changed; refresh before saving");
    }
    let revision = match body.expected_revision.parse::<i64>() {
        Ok(value) if value >= 0 && value < i64::MAX && value.to_string() == body.expected_revision => value,
        _ => return error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid plugin revision"),
    };
    if !body.expected_catalog_revision.parse::<u64>().is_ok_and(|n| n > 0 && n.to_string() == body.expected_catalog_revision) {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid deployment catalog revision");
    }
    if !is_name(&plugin_id) || !is_name(&body.selected_version) {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid plugin identity or version");
    }
    let catalog = match &state.options.catalog {
        Ok(catalog) => catalog,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "Assistant plugin catalog is unavailable"),
    };
    if catalog.revision().to_string() != body.expected_catalog_revision {
        return error(StatusCode::CONFLICT, "The deployment catalog changed; refresh before saving");
    }
    if lifecycle::denied_capabilities(catalog, &Default::default()).is_err() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "Assistant plugin catalog is unavailable");
    }
    let hash = match lifecycle::selection_hash(catalog, &plugin_id, &body.selected_version) {
        Ok(value) => value,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "Assistant plugin catalog is unavailable"),
    };
    if hash.is_none() && body.enabled {
        return error(StatusCode::CONFLICT, "The selected version is no longer registered; refresh before saving");
    }
    let records: Vec<_> = catalog.records().iter().filter(|record| record["plugin_id"] == plugin_id).collect();
    if records.iter().any(|record| record["plugin_version"].is_null()) {
        return error(StatusCode::SERVICE_UNAVAILABLE, "Assistant plugin catalog is unavailable");
    }
    for record in records.iter().filter(|record| record["plugin_version"] == body.selected_version) {
        if body.enabled && !super::publisher_admitted(record, &principal.tenant_id) {
            return error(StatusCode::FORBIDDEN, "The publisher is not admitted for this tenant");
        }
        let required: Vec<_> = record["required_permissions"].as_array().into_iter().flatten()
            .filter_map(Value::as_str).map(str::to_owned).collect();
        if !user_has_required_permissions(&principal.tier, &principal.roles, &required) {
            return error(StatusCode::FORBIDDEN, "The selected version requires additional authority");
        }
    }
    let tenant = &principal.tenant_id;
    let user = &principal.user_id;
    // All errors below are redacted. A dropped transaction rolls back both state
    // and audit, while uncertain commit responses must be resolved by re-reading.
    let outcome: Result<Option<lifecycle::PluginBinding>, sqlx::Error> = async {
        let mut tx = state.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout = '2s'").execute(&mut *tx).await?;
        sqlx::query("SET LOCAL statement_timeout = '5s'").execute(&mut *tx).await?;
        // Serialize tenant insertions so the bounded read contract cannot be
        // exceeded by concurrent configuration of new plugins.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 410217))")
            .bind(tenant).execute(&mut *tx).await?;
        if revision == 0 {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM gateway.tenant_plugin_bindings WHERE tenant_id=$1")
                .bind(tenant).fetch_one(&mut *tx).await?;
            if count >= 1024 { return Ok(None); }
        }
        let previous = sqlx::query_as::<_, lifecycle::PluginBinding>(
            "SELECT plugin_id,selected_version,enabled,revision,selected_catalog_revision,selection_hash,capability_names,updated_at FROM gateway.tenant_plugin_bindings WHERE tenant_id=$1 AND plugin_id=$2 FOR UPDATE",
        ).bind(tenant).bind(&plugin_id).fetch_optional(&mut *tx).await?;
        if previous.as_ref().map_or(0, |row| row.revision) != revision {
            return Ok(None);
        }
        // A disabled orphan can remain disabled without inventing an installed
        // version. Enabling always requires a currently registered descriptor set.
        let hash = hash.or_else(|| previous.as_ref().filter(|row| {
            !body.enabled && row.selected_version == body.selected_version
        }).map(|row| row.selection_hash.clone()));
        let Some(hash) = hash else {
            return Ok(None);
        };
        let mut names: BTreeSet<String> = records.iter().filter_map(|record| record["name"].as_str()).map(str::to_owned).collect();
        if let Some(previous) = &previous {
            names.extend(previous.capability_names.iter().cloned());
        }
        if names.len() > 1024 {
            return Err(sqlx::Error::Protocol("plugin capability history limit".into()));
        }
        let names: Vec<_> = names.into_iter().collect();
        let statement = if revision == 0 {
            "INSERT INTO gateway.tenant_plugin_bindings (tenant_id,plugin_id,selected_version,enabled,revision,selected_catalog_revision,selection_hash,capability_names,updated_by) VALUES ($1,$2,$3,$4,1,$5,$6,$7,$8) ON CONFLICT (tenant_id,plugin_id) DO NOTHING RETURNING plugin_id,selected_version,enabled,revision,selected_catalog_revision,selection_hash,capability_names,updated_at"
        } else {
            "UPDATE gateway.tenant_plugin_bindings SET selected_version=$3,enabled=$4,revision=revision+1,selected_catalog_revision=$5,selection_hash=$6,capability_names=$7,updated_by=$8,updated_at=NOW() WHERE tenant_id=$1 AND plugin_id=$2 RETURNING plugin_id,selected_version,enabled,revision,selected_catalog_revision,selection_hash,capability_names,updated_at"
        };
        let saved = sqlx::query_as::<_, lifecycle::PluginBinding>(statement)
            .bind(tenant).bind(&plugin_id).bind(&body.selected_version).bind(body.enabled)
            .bind(&body.expected_catalog_revision).bind(hash).bind(names).bind(user)
            .fetch_optional(&mut *tx).await?;
        let Some(saved) = saved else { return Ok(None); };
        // Metadata only: no manifests, credentials, descriptions or package text.
        let before = previous.map(|row| json!({"revision":row.revision.to_string(),"selected_version":row.selected_version,"enabled":row.enabled}));
        let after = json!({"revision":saved.revision.to_string(),"selected_version":saved.selected_version,"enabled":saved.enabled,"catalog_revision":saved.selected_catalog_revision});
        sqlx::query("INSERT INTO gateway.audit_logs (event_type,user_id,tenant_id,resource_type,resource_id,action,request_summary,response_summary,status) VALUES ('config_changed',$1,$2,'tenant_plugin',$3,'set_lifecycle',$4,$5,'success')")
            .bind(user).bind(tenant).bind(&plugin_id).bind(json!({"before":before})).bind(after)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(saved))
    }.await;
    match outcome {
        Ok(Some(binding)) => match lifecycle::project_binding(catalog, &binding) {
            Ok(lifecycle) => Json(crate::native_contract::responses::plugin_update::value(plugin_id, tenant, lifecycle)).into_response(),
            Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "Plugin state was saved; refresh to verify"),
        },
        Ok(None) => error(StatusCode::CONFLICT, "Plugin state or selected version changed; refresh before saving"),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "Plugin lifecycle could not be confirmed; refresh before retrying"),
    }
}
