//! Explicit enterprise identity startup on the existing listener/account authority.
use super::{BootstrapError, auth::key_file};
use ai_platform_auth::enterprise::{
    self,
    oidc::{OidcConfiguration, OidcProvider},
};
use serde::Deserialize;
use std::{collections::BTreeMap, sync::Arc};

const CONFIG: &str = "AI_PLATFORM_ENTERPRISE_IDENTITY_CONFIG_FILE";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Configuration {
    schema: String,
    rp_id: String,
    rp_origin: String,
    #[serde(default)]
    providers: Vec<Provider>,
    #[serde(skip)]
    origins: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provider {
    configuration: OidcConfiguration,
    client_secret_file: Option<String>,
}
impl Configuration {
    pub(super) fn from_environment(
        environment: &BTreeMap<String, String>,
    ) -> Result<Option<Self>, BootstrapError> {
        let Some(path) = environment.get(CONFIG) else {
            return Ok(None);
        };
        let mut value: Self = serde_json::from_str(&key_file(path, false)?)
            .map_err(|_| BootstrapError::Configuration(CONFIG))?;
        value.origins = serde_json::from_str(
            environment
                .get("AI_PLATFORM_BFF_ORIGINS")
                .ok_or(BootstrapError::Configuration("enterprise_requires_bff"))?,
        )
        .map_err(|_| BootstrapError::Configuration("enterprise_requires_bff"))?;
        value.validate()?;
        Ok(Some(value))
    }
    fn validate(&self) -> Result<(), BootstrapError> {
        let invalid = || BootstrapError::Configuration(CONFIG);
        ai_platform_auth::browser_policy::BrowserPolicy::new(self.origins.clone())
            .map_err(|_| invalid())?;
        let origin = reqwest::Url::parse(&self.rp_origin).map_err(|_| invalid())?;
        if self.schema != "agentforge/enterprise-identity/v1"
            || self.providers.len() > 32
            || self.rp_id.is_empty()
            || self.rp_id.len() > 255
            || origin.scheme() != "https"
            || origin.origin().ascii_serialization() != self.rp_origin
            || !self.origins.contains(&self.rp_origin)
        {
            return Err(invalid());
        }
        let mut ids = std::collections::BTreeSet::new();
        for provider in &self.providers {
            let c = &provider.configuration;
            let redirect = reqwest::Url::parse(&c.redirect_url).map_err(|_| invalid())?;
            if !ids.insert(&c.id)
                || !self
                    .origins
                    .contains(&redirect.origin().ascii_serialization())
            {
                return Err(invalid());
            }
            if let Some(path) = &provider.client_secret_file {
                let secret = key_file(path, true)?;
                if secret.is_empty() || secret.len() > 4096 || secret.contains(['\0', '\r', '\n']) {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
    pub(super) async fn router(
        self,
        auth: Arc<ai_platform_auth::service::AuthService>,
        coordination: redis::aio::ConnectionManager,
    ) -> Result<axum::Router, BootstrapError> {
        if !auth.enterprise_policy_enabled() {
            return Err(BootstrapError::Construction("enterprise_policy_gate"));
        }
        let mut providers = Vec::with_capacity(self.providers.len());
        for provider in self.providers {
            let secret = provider
                .client_secret_file
                .as_deref()
                .map(|path| key_file(path, true))
                .transpose()?;
            providers.push(
                OidcProvider::discover(provider.configuration, secret)
                    .await
                    .map_err(|_| BootstrapError::Construction("enterprise_oidc_discovery"))?,
            );
        }
        let service = enterprise::EnterpriseService::new(
            auth,
            coordination,
            self.origins,
            &self.rp_id,
            &self.rp_origin,
            providers,
        )
        .map_err(|_| BootstrapError::Construction("enterprise_identity"))?;
        Ok(enterprise::http::router(service))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enterprise_is_explicit_and_requires_a_canonical_trusted_https_rp_origin() {
        assert!(
            Configuration::from_environment(&BTreeMap::new())
                .unwrap()
                .is_none()
        );
        let mut c = Configuration {
            schema: "agentforge/enterprise-identity/v1".into(),
            rp_id: "console.example".into(),
            rp_origin: "https://console.example".into(),
            providers: vec![],
            origins: vec!["https://console.example".into()],
        };
        assert!(c.validate().is_ok());
        c.rp_origin = "http://console.example".into();
        assert!(c.validate().is_err());
        c.rp_origin = "https://console.example/path".into();
        assert!(c.validate().is_err());
        c.rp_origin = "https://foreign.example".into();
        assert!(c.validate().is_err());
        c.rp_origin = "https://console.example".into();
        c.schema = "unknown".into();
        assert!(c.validate().is_err());
        assert!(
            Configuration::from_environment(&BTreeMap::from([(CONFIG.into(), String::new())]))
                .is_err()
        );
    }
}
