//! Mountable enterprise OIDC, SCIM and WebAuthn boundaries; no listener startup.
// SPDX-License-Identifier: Apache-2.0
use super::*;
use axum::{
    Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderValue, Method},
    response::Response,
    routing::{delete, get, post},
};
use serde::Deserialize;
const OIDC_COOKIE: &str = "__Host-agentforge_oidc_flow";
const PASSKEY_COOKIE: &str = "__Host-agentforge_passkey_flow";
pub fn router(service: EnterpriseService) -> Router {
    Router::new()
        .route(
            "/api/auth/enterprise/oidc/{provider}/begin",
            post(oidc_begin),
        )
        .route("/api/auth/enterprise/oidc/callback", get(oidc_callback))
        .route(
            "/api/auth/enterprise/passkeys/register/start",
            post(register_start),
        )
        .route(
            "/api/auth/enterprise/passkeys/register/finish",
            post(register_finish),
        )
        .route(
            "/api/auth/enterprise/passkeys/authenticate/start",
            post(authenticate_start),
        )
        .route(
            "/api/auth/enterprise/passkeys/authenticate/finish",
            post(authenticate_finish),
        )
        .route("/api/auth/enterprise/passkeys/{id}", delete(remove_passkey))
        .route("/api/auth/enterprise/recovery", post(recovery))
        .route(
            "/api/auth/enterprise/recovery/rotate",
            post(rotate_recovery),
        )
        .route("/api/auth/enterprise/scim-credentials", post(issue_scim))
        .route(
            "/api/auth/enterprise/scim-credentials/{id}",
            delete(revoke_scim),
        )
        .route("/api/auth/enterprise/identities", post(link_identity))
        .route("/scim/v2/{tenant}/Users", get(list_users).post(create_user))
        .route(
            "/scim/v2/{tenant}/Users/{id}",
            get(user)
                .put(replace_user)
                .patch(patch_user)
                .delete(delete_user),
        )
        .route(
            "/scim/v2/{tenant}/Groups",
            get(list_groups).post(create_group),
        )
        .route(
            "/scim/v2/{tenant}/Groups/{id}",
            get(group)
                .put(replace_group)
                .patch(patch_group)
                .delete(delete_group),
        )
        .route("/scim/v2/{tenant}/ServiceProviderConfig", get(scim_config))
        .route("/scim/v2/{tenant}/Schemas", get(scim_schemas))
        .route("/scim/v2/{tenant}/ResourceTypes", get(scim_resource_types))
        .layer(axum::middleware::from_fn(scim_error_envelope))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .with_state(service)
}
fn response(body: Value, session_cookie: Option<&str>) -> Result<Response> {
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    if let Some(cookie) = session_cookie {
        let mut value = HeaderValue::from_str(cookie).map_err(|_| EnterpriseError::Invalid)?;
        value.set_sensitive(true);
        response.headers_mut().append("set-cookie", value);
    }
    Ok(response)
}
fn grant(grant: session::SessionGrant) -> Result<Response> {
    response(grant.body, Some(&grant.cookie))
}
fn binding(headers: &HeaderMap, name: &str) -> Result<Option<String>> {
    let mut result = None;
    for header in headers.get_all("cookie") {
        for part in header
            .to_str()
            .map_err(|_| EnterpriseError::Proof)?
            .split(';')
        {
            if let Some((key, value)) = part.trim().split_once('=') {
                if key == name {
                    if result.is_some()
                        || value.len() != 64
                        || !value.bytes().all(|c| c.is_ascii_hexdigit())
                    {
                        return Err(EnterpriseError::Proof);
                    }
                    result = Some(value.to_owned());
                }
            }
        }
    }
    Ok(result)
}
fn flow_cookie(name: &str, value: &str, ttl: u64) -> String {
    format!("{name}={value}; Path=/; Max-Age={ttl}; HttpOnly; Secure; SameSite=Lax")
}
async fn oidc_begin(
    State(s): State<EnterpriseService>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Result<Response> {
    s.browser
        .check_origin(&headers, true)
        .map_err(|_| EnterpriseError::Forbidden)?;
    let id = binding(&headers, OIDC_COOKIE)?.unwrap_or_else(crate::sessions::browser::opaque);
    let url = s.oidc_begin(&provider, &id).await?;
    response(
        json!({"authorization_url":url}),
        Some(&flow_cookie(OIDC_COOKIE, &id, 300)),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OidcCallback {
    state: String,
    code: String,
    #[serde(default)]
    session_state: Option<String>,
    #[serde(default)]
    iss: Option<String>,
}
async fn oidc_callback(
    State(s): State<EnterpriseService>,
    headers: HeaderMap,
    Query(query): Query<OidcCallback>,
) -> Result<Response> {
    // This is the sole literal cross-site cookie exception. Its own independent
    // flow cookie is matched atomically against one-use state before code/PKCE,
    // issuer/audience/signature/nonce validation. Generic BFF exceptions are not allowed.
    let _ = query.session_state;
    let id = binding(&headers, OIDC_COOKIE)?.ok_or(EnterpriseError::Proof)?;
    let identity = s
        .oidc_finish(&query.state, &query.code, &id, query.iss.as_deref())
        .await?;
    let mut response = grant(
        s.establish(
            &identity.user,
            &identity.tenant,
            identity.method,
            identity.epoch,
        )
        .await?,
    )?;
    response.headers_mut().append(
        "set-cookie",
        HeaderValue::from_str(&flow_cookie(OIDC_COOKIE, "", 0))
            .map_err(|_| EnterpriseError::Invalid)?,
    );
    *response.status_mut() = StatusCode::SEE_OTHER;
    response.headers_mut().insert(
        "location",
        HeaderValue::from_str(&identity.return_url).map_err(|_| EnterpriseError::Invalid)?,
    );
    *response.body_mut() = axum::body::Body::empty();
    Ok(response)
}
async fn register_start(
    State(s): State<EnterpriseService>,
    headers: HeaderMap,
    Json(input): Json<passkeys::RegistrationStart>,
) -> Result<Response> {
    let p = s.principal(&headers).await?;
    response(s.passkey_registration_start(&p, input).await?, None)
}
async fn register_finish(
    State(s): State<EnterpriseService>,
    headers: HeaderMap,
    Json(input): Json<passkeys::RegistrationFinish>,
) -> Result<Response> {
    let p = s.principal(&headers).await?;
    let (mut session, codes) = s.passkey_registration_finish(&p, input).await?;
    session.body["recovery_codes"] = json!(codes);
    grant(session)
}
async fn authenticate_start(
    State(s): State<EnterpriseService>,
    headers: HeaderMap,
    Json(input): Json<passkeys::AuthenticationStart>,
) -> Result<Response> {
    s.browser
        .check_origin(&headers, true)
        .map_err(|_| EnterpriseError::Forbidden)?;
    let id = binding(&headers, PASSKEY_COOKIE)?.unwrap_or_else(crate::sessions::browser::opaque);
    response(
        s.passkey_authentication_start(input, &id).await?,
        Some(&flow_cookie(PASSKEY_COOKIE, &id, 300)),
    )
}
async fn authenticate_finish(
    State(s): State<EnterpriseService>,
    headers: HeaderMap,
    Json(input): Json<passkeys::AuthenticationFinish>,
) -> Result<Response> {
    s.browser
        .check_origin(&headers, true)
        .map_err(|_| EnterpriseError::Forbidden)?;
    let id = binding(&headers, PASSKEY_COOKIE)?.ok_or(EnterpriseError::Proof)?;
    grant(s.passkey_authentication_finish(input, &id).await?)
}
async fn remove_passkey(
    State(s): State<EnterpriseService>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response> {
    let p = s.principal(&headers).await?;
    s.remove_passkey(&p, &id).await?;
    response(json!({"message":"Passkey removed; sign in again"}), None)
}
async fn recovery(
    State(s): State<EnterpriseService>,
    headers: HeaderMap,
    Json(input): Json<passkeys::RecoveryInput>,
) -> Result<Response> {
    s.browser
        .check_origin(&headers, true)
        .map_err(|_| EnterpriseError::Forbidden)?;
    let id = binding(&headers, PASSKEY_COOKIE)?.unwrap_or_else(crate::sessions::browser::opaque);
    grant(s.recovery(input, &id).await?)
}
async fn rotate_recovery(
    State(s): State<EnterpriseService>,
    headers: HeaderMap,
) -> Result<Response> {
    let p = s.principal(&headers).await?;
    response(
        json!({"recovery_codes":s.rotate_recovery_codes(&p).await?,"message":"Previous sessions revoked; sign in again"}),
        None,
    )
}
async fn issue_scim(
    State(s): State<EnterpriseService>,
    headers: HeaderMap,
    Json(input): Json<scim::CredentialRequest>,
) -> Result<Response> {
    let p = s.principal(&headers).await?;
    response(s.issue_scim_credential(&p, input).await?, None)
}
async fn revoke_scim(
    State(s): State<EnterpriseService>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response> {
    let p = s.principal(&headers).await?;
    s.require_recent_mfa(&p).await?;
    if !p.authority.roles.iter().any(|r| r == "admin")
        && !p
            .authority
            .permissions
            .iter()
            .any(|r| r == "identity:provision")
    {
        return Err(EnterpriseError::Forbidden);
    }
    uuid::Uuid::parse_str(&id).map_err(|_| EnterpriseError::NotFound)?;
    if sqlx::query("UPDATE gateway.enterprise_scim_credentials SET revoked_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND credential_id=$2::text::uuid AND revoked_at IS NULL").bind(&p.account.tenant_id).bind(id).execute(&s.pool).await?.rows_affected()!=1{return Err(EnterpriseError::NotFound);}
    response(json!({"revoked":true}), None)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityLink {
    provider_id: String,
    subject: String,
    user_id: String,
}
async fn link_identity(
    State(s): State<EnterpriseService>,
    headers: HeaderMap,
    Json(input): Json<IdentityLink>,
) -> Result<Response> {
    let p = s.principal(&headers).await?;
    s.require_recent_mfa(&p).await?;
    if !p.authority.roles.iter().any(|r| r == "admin")
        && !p
            .authority
            .permissions
            .iter()
            .any(|r| r == "identity:provision")
    {
        return Err(EnterpriseError::Forbidden);
    }
    let provider = s
        .providers
        .get(&input.provider_id)
        .ok_or(EnterpriseError::NotFound)?;
    if provider.tenant() != p.account.tenant_id {
        return Err(EnterpriseError::Forbidden);
    }
    bounded(&input.subject, 2048)?;
    let changed=sqlx::query("INSERT INTO gateway.enterprise_identity_links(tenant_id,provider_id,issuer,subject,user_id) SELECT $1,$2,$3,$4,$5 WHERE EXISTS(SELECT 1 FROM gateway.users WHERE tenant_id=$1 AND user_id=$5 AND status='active')").bind(&p.account.tenant_id).bind(&input.provider_id).bind(provider.issuer()).bind(input.subject).bind(input.user_id).execute(&s.pool).await?.rows_affected();
    if changed != 1 {
        return Err(EnterpriseError::NotFound);
    }
    response(json!({"linked":true}), None)
}
#[derive(Deserialize)]
struct Page {
    #[serde(rename = "startIndex", default = "first")]
    start: u64,
    #[serde(default = "page_size")]
    count: u64,
    filter: Option<String>,
}
fn first() -> u64 {
    1
}
fn page_size() -> u64 {
    100
}
fn version(headers: &HeaderMap) -> Result<Option<&str>> {
    if headers.get_all("if-match").iter().count() > 1 {
        return Err(EnterpriseError::Invalid);
    }
    headers
        .get("if-match")
        .map(|v| v.to_str().map_err(|_| EnterpriseError::Invalid))
        .transpose()
}
fn scim_result(body: Value) -> Result<Response> {
    let mut response = response(body.clone(), None)?;
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/scim+json"),
    );
    if let Some(version) = body["meta"]["version"].as_str() {
        response.headers_mut().insert(
            "etag",
            HeaderValue::from_str(version).map_err(|_| EnterpriseError::Invalid)?,
        );
    }
    Ok(response)
}
async fn list_users(
    State(s): State<EnterpriseService>,
    Path(tenant): Path<String>,
    headers: HeaderMap,
    Query(p): Query<Page>,
) -> Result<Response> {
    let a = s.scim_actor(&tenant, &headers).await?;
    scim_result(
        s.scim_list_users(&a, p.start, p.count, p.filter.as_deref())
            .await?,
    )
}
async fn user(
    State(s): State<EnterpriseService>,
    Path((tenant, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response> {
    let a = s.scim_actor(&tenant, &headers).await?;
    a.require("scim.users.read")?;
    scim_result(s.scim_user(&a, &id).await?)
}
async fn create_user(
    State(s): State<EnterpriseService>,
    Path(tenant): Path<String>,
    headers: HeaderMap,
    Json(input): Json<scim::UserInput>,
) -> Result<Response> {
    let a = s.scim_actor(&tenant, &headers).await?;
    let mut r = scim_result(s.scim_create_user(&a, input).await?)?;
    *r.status_mut() = StatusCode::CREATED;
    Ok(r)
}
async fn replace_user(
    State(s): State<EnterpriseService>,
    Path((tenant, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<scim::UserInput>,
) -> Result<Response> {
    let a = s.scim_actor(&tenant, &headers).await?;
    let old = s.scim_user(&a, &id).await?;
    let v = version(&headers)?
        .or_else(|| old["meta"]["version"].as_str())
        .ok_or(EnterpriseError::Precondition)?;
    scim_result(s.scim_replace_user(&a, &id, input, v).await?)
}
async fn patch_user(
    State(s): State<EnterpriseService>,
    Path((tenant, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<scim::Patch>,
) -> Result<Response> {
    let a = s.scim_actor(&tenant, &headers).await?;
    scim_result(
        s.scim_patch_user(&a, &id, input, version(&headers)?)
            .await?,
    )
}
async fn delete_user(
    State(s): State<EnterpriseService>,
    Path((tenant, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response> {
    let a = s.scim_actor(&tenant, &headers).await?;
    let old = s.scim_user(&a, &id).await?;
    let v = version(&headers)?
        .or_else(|| old["meta"]["version"].as_str())
        .ok_or(EnterpriseError::Precondition)?;
    s.scim_delete_user(&a, &id, v).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
async fn list_groups(
    State(s): State<EnterpriseService>,
    Path(tenant): Path<String>,
    headers: HeaderMap,
    Query(p): Query<Page>,
) -> Result<Response> {
    if p.filter.is_some() {
        return Err(EnterpriseError::Invalid);
    }
    let a = s.scim_actor(&tenant, &headers).await?;
    scim_result(s.scim_groups(&a, p.start, p.count).await?)
}
async fn group(
    State(s): State<EnterpriseService>,
    Path((tenant, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response> {
    let a = s.scim_actor(&tenant, &headers).await?;
    a.require("scim.groups.read")?;
    scim_result(s.scim_group(&a, &id).await?)
}
async fn create_group(
    State(s): State<EnterpriseService>,
    Path(tenant): Path<String>,
    headers: HeaderMap,
    Json(input): Json<scim::GroupInput>,
) -> Result<Response> {
    let a = s.scim_actor(&tenant, &headers).await?;
    let mut r = scim_result(s.scim_write_group(&a, None, input, None).await?)?;
    *r.status_mut() = StatusCode::CREATED;
    Ok(r)
}
async fn replace_group(
    State(s): State<EnterpriseService>,
    Path((tenant, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<scim::GroupInput>,
) -> Result<Response> {
    let a = s.scim_actor(&tenant, &headers).await?;
    let old = s.scim_group(&a, &id).await?;
    scim_result(
        s.scim_write_group(
            &a,
            Some(&id),
            input,
            version(&headers)?.or_else(|| old["meta"]["version"].as_str()),
        )
        .await?,
    )
}
async fn patch_group(
    State(s): State<EnterpriseService>,
    Path((tenant, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<scim::Patch>,
) -> Result<Response> {
    let a = s.scim_actor(&tenant, &headers).await?;
    scim_result(
        s.scim_patch_group(&a, &id, input, version(&headers)?)
            .await?,
    )
}
async fn delete_group(
    State(s): State<EnterpriseService>,
    Path((tenant, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response> {
    let a = s.scim_actor(&tenant, &headers).await?;
    s.scim_delete_group(&a, &id, version(&headers)?).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
async fn scim_config(
    State(s): State<EnterpriseService>,
    Path(tenant): Path<String>,
    headers: HeaderMap,
) -> Result<Response> {
    s.scim_actor(&tenant, &headers).await?;
    scim_result(
        json!({"schemas":["urn:ietf:params:scim:schemas:core:2.0:ServiceProviderConfig"],"patch":{"supported":true},"bulk":{"supported":false,"maxOperations":0,"maxPayloadSize":0},"filter":{"supported":true,"maxResults":100},"changePassword":{"supported":false},"sort":{"supported":false},"etag":{"supported":true},"authenticationSchemes":[{"type":"oauthbearertoken","name":"Tenant scoped provisioner","description":"Dedicated SCIM credential","primary":true}]}),
    )
}
async fn scim_schemas(
    State(s): State<EnterpriseService>,
    Path(tenant): Path<String>,
    headers: HeaderMap,
) -> Result<Response> {
    s.scim_actor(&tenant, &headers).await?;
    scim_result(
        json!({"schemas":["urn:ietf:params:scim:api:messages:2.0:ListResponse"],"totalResults":2,"Resources":[{"id":"urn:ietf:params:scim:schemas:core:2.0:User","name":"User","attributes":[{"name":"userName","type":"string","multiValued":false,"required":true},{"name":"active","type":"boolean","multiValued":false,"required":false}]},{"id":"urn:ietf:params:scim:schemas:core:2.0:Group","name":"Group","attributes":[{"name":"displayName","type":"string","multiValued":false,"required":true},{"name":"members","type":"complex","multiValued":true,"required":false}]}]}),
    )
}
async fn scim_resource_types(
    State(s): State<EnterpriseService>,
    Path(tenant): Path<String>,
    headers: HeaderMap,
) -> Result<Response> {
    s.scim_actor(&tenant, &headers).await?;
    scim_result(
        json!({"schemas":["urn:ietf:params:scim:api:messages:2.0:ListResponse"],"totalResults":2,"Resources":[{"id":"User","name":"User","endpoint":"/Users","schema":"urn:ietf:params:scim:schemas:core:2.0:User"},{"id":"Group","name":"Group","endpoint":"/Groups","schema":"urn:ietf:params:scim:schemas:core:2.0:Group"}]}),
    )
}

// Normalize handler and extractor failures to the SCIM protocol envelope.
async fn scim_error_envelope(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let scim = request.uri().path().starts_with("/scim/v2/");
    let response = next.run(request).await;
    if !scim || !response.status().is_client_error() && !response.status().is_server_error() {
        return response;
    }
    let status = response.status();
    let mut response = (
        status,
        Json(json!({
            "schemas":["urn:ietf:params:scim:api:messages:2.0:Error"],
            "status":status.as_u16().to_string(),
            "detail":status.canonical_reason().unwrap_or("SCIM request failed")
        })),
    )
        .into_response();
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/scim+json"),
    );
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}
