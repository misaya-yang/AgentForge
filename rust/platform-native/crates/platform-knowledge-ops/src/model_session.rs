//! Explicitly admitted operator identity plus the production Knowledge model plane.
//! No dotenv/provider-key discovery and no synthetic system actor.
use super::{Args, Result, error, fail};
use ai_platform_knowledge_service::{
    migration_knowledge_query::{
        model_plane::{self, ModelPlaneClient, ModelResource},
        types::UserContext,
    },
    model_plane_identity::ModelPlaneSettings,
};
use serde_json::Value;
use std::{collections::BTreeMap, future::Future, path::Path, sync::Arc, time::Duration};
pub const OPTIONS: &[&str] = &[
    "tenant-id",
    "user-id",
    "credential-file",
    "credential-kind",
    "model-plane-url",
    "workload-key-file",
    "gateway-public-keys-file",
    "model-revisions",
    "dataset-id",
];
pub struct ModelSession {
    client: Arc<ModelPlaneClient>,
    actor: UserContext,
    resource: ModelResource,
}
fn option(args: &Args, flag: &str, env: &str) -> Option<String> {
    args.options
        .get(flag)
        .cloned()
        .or_else(|| std::env::var(env).ok())
        .filter(|s| !s.trim().is_empty())
}
fn required(args: &Args, flag: &str) -> Result<String> {
    args.options
        .get(flag)
        .filter(|s| !s.trim().is_empty() && s.trim() == s.as_str())
        .cloned()
        .ok_or_else(|| error(&format!("--{flag} is required for live model evaluation")))
}
fn origin(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).map_err(|_| error("invalid Gateway origin"))?;
    let loopback = url.host_str().is_some_and(|host| {
        host.trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    });
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !["", "/"].contains(&url.path())
    {
        return fail(
            "operator Gateway credentials require HTTPS or literal loopback development HTTP origin",
        );
    }
    Ok(url)
}
fn credential(path: &Path) -> Result<String> {
    if !path.is_absolute() {
        return fail("credential file must use a controlled absolute path");
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| error("cannot read operator credential file"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > 16 * 1024
    {
        return fail("operator credential file must be a bounded regular non-symlink file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return fail("operator credential file permissions must be 0600");
        }
    }
    let mut file =
        std::fs::File::open(path).map_err(|_| error("cannot open operator credential file"))?;
    let opened = file
        .metadata()
        .map_err(|_| error("cannot inspect operator credential file"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            return fail("operator credential file changed while opening");
        }
    }
    use std::io::Read;
    let mut token = String::new();
    file.by_ref()
        .take(16 * 1024 + 1)
        .read_to_string(&mut token)
        .map_err(|_| error("operator credential file is not valid UTF-8"))?;
    if token.len() > 16 * 1024 {
        return fail("operator credential file exceeds budget");
    }
    let token = token.trim();
    if token.is_empty() || token.chars().any(char::is_control) {
        return fail("operator credential is empty or malformed");
    }
    Ok(token.into())
}
fn verified_actor(identity: Value, tenant: &str, user: &str) -> Result<UserContext> {
    if identity["tenant_id"] != tenant
        || identity["user_id"] != user
        || tenant.is_empty()
        || user.is_empty()
        || identity["is_authenticated"] == false
        || identity["status"].as_str().is_some_and(|s| s != "active")
    {
        return fail("Gateway operator identity does not match the explicit tenant/user");
    }
    let roles = identity["roles"]
        .as_array()
        .map(|roles| {
            roles
                .iter()
                .map(|role| {
                    role.as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .ok_or_else(|| error("Gateway operator roles are invalid"))
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let actor = UserContext {
        tenant_id: tenant.into(),
        user_id: user.into(),
        user_tier: identity["tier"]
            .as_str()
            .or_else(|| identity["user_tier"].as_str())
            .unwrap_or("normal")
            .into(),
        user_type: identity["user_type"].as_str().unwrap_or("user").into(),
        roles,
        is_authenticated: true,
    };
    if actor.anonymous() {
        return fail("anonymous operators cannot run model measurements");
    }
    Ok(actor)
}
impl ModelSession {
    pub async fn load(args: &Args) -> Result<Self> {
        let tenant = required(args, "tenant-id")?;
        let user = required(args, "user-id")?;
        let credential_path = required(args, "credential-file")?;
        let token = credential(Path::new(&credential_path))?;
        let gateway = origin(
            &option(args, "model-plane-url", "KNOWLEDGE_MODEL_PLANE__URL")
                .ok_or_else(|| error("Knowledge model-plane URL is required"))?,
        )?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;
        let mut request = client.get(gateway.join("/api/v1/auth/me")?);
        let mut header = reqwest::header::HeaderValue::from_str(&token)
            .map_err(|_| error("operator credential is invalid"))?;
        header.set_sensitive(true);
        request = match args.get("credential-kind", "bearer") {
            "bearer" => {
                let mut bearer = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| error("operator credential is invalid"))?;
                bearer.set_sensitive(true);
                request.header(reqwest::header::AUTHORIZATION, bearer)
            }
            "api-key" => request.header("x-api-key", header),
            _ => return fail("--credential-kind must be bearer or api-key"),
        };
        let mut response = request
            .send()
            .await
            .map_err(|_| error("Gateway operator identity verification unavailable"))?;
        if response.status().as_u16() != 200 {
            return fail("Gateway rejected operator credentials");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| error("Gateway identity response interrupted"))?
        {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return fail("Gateway identity response exceeds budget");
            }
            bytes.extend_from_slice(&chunk);
        }
        let identity = serde_json::from_slice(&bytes)
            .map_err(|_| error("Gateway operator identity response is invalid"))?;
        let actor = verified_actor(identity, &tenant, &user)?;
        let revisions: BTreeMap<String, String> = serde_json::from_str(
            &option(
                args,
                "model-revisions",
                "KNOWLEDGE_MODEL_PLANE__MODEL_REVISIONS",
            )
            .ok_or_else(|| error("registered model revisions are required"))?,
        )
        .map_err(|_| error("model revisions JSON is invalid"))?;
        if revisions.is_empty() {
            return fail("registered model revisions cannot be empty");
        }
        let settings = ModelPlaneSettings {
            origin: gateway.to_string(),
            workload_key_file: option(
                args,
                "workload-key-file",
                "KNOWLEDGE_MODEL_PLANE__WORKLOAD_KEY_FILE",
            )
            .map(Into::into),
            gateway_public_keys_file: option(
                args,
                "gateway-public-keys-file",
                "KNOWLEDGE_MODEL_PLANE__GATEWAY_PUBLIC_KEYS_FILE",
            )
            .map(Into::into),
            model_revisions: revisions,
        };
        let client = settings.construct()?;
        Ok(Self {
            client,
            actor,
            resource: ModelResource {
                dataset_id: args.options.get("dataset-id").cloned(),
                document_id: None,
                execution_id: None,
            },
        })
    }
    pub async fn run<F: Future>(&self, future: F) -> F::Output {
        model_plane::run_scoped(
            self.client.clone(),
            self.resource.clone(),
            self.actor.clone(),
            future,
        )
        .await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn credential_origin_rejects_remote_http_userinfo_redirect_origins() {
        for value in [
            "http://example.com",
            "http://localhost:8080",
            "https://u:secret@example.com",
            "https://example.com/path",
            "https://example.com?x=1",
        ] {
            assert!(origin(value).is_err());
        }
        for value in [
            "https://example.com",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            assert!(origin(value).is_ok());
        }
    }
    #[test]
    fn current_actor_requires_exact_tenant_user_and_active_authenticated_state() {
        for value in [
            json!({"tenant_id":"other","user_id":"u"}),
            json!({"tenant_id":"t","user_id":"other"}),
            json!({"tenant_id":"t","user_id":"u","status":"disabled"}),
            json!({"tenant_id":"t","user_id":"u","is_authenticated":false}),
        ] {
            assert!(verified_actor(value, "t", "u").is_err());
        }
        assert!(
            verified_actor(
                json!({"tenant_id":"t","user_id":"u","roles":["user"]}),
                "t",
                "u"
            )
            .is_ok()
        );
    }
    #[test]
    fn missing_explicit_identity_cannot_select_a_system_actor() {
        let args = Args::default();
        assert!(required(&args, "tenant-id").is_err());
        assert!(required(&args, "user-id").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn credential_file_requires_private_regular_bounded_absolute_file() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = std::env::temp_dir().join(format!(
            "knowledge-operator-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("credential");
        std::fs::write(&path, "fixture-token\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(credential(&path).unwrap(), "fixture-token");
        let link = root.join("link");
        symlink(&path, &link).unwrap();
        assert!(credential(&link).is_err());
        assert!(credential(Path::new("relative")).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(credential(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, vec![b'a'; 16385]).unwrap();
        assert!(credential(&path).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
