//! Session-only reviewed provider catalog workflow. No legacy mutation fallback.
use super::{
    super::api::{ApiRequest, DesktopApi},
    common::*,
};
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::BTreeSet;
#[derive(Default)]
pub struct ProviderCatalog {
    job: RemoteJob,
    provider: String,
    preview: Value,
    result: Value,
    selected: BTreeSet<String>,
    replace: bool,
    consumed: bool,
}
impl ProviderCatalog {
    pub fn propagate_expiry(&self, parent: &mut RemoteJob) {
        if self.job.expired {
            parent.expired = true;
        }
    }
    pub fn start(
        &mut self,
        provider: &str,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.job.cancel_observation();
        *self = Self::default();
        self.provider = provider.into();
        self.job.request(
            "preview",
            api,
            ApiRequest::mutation(
                Method::POST,
                &[
                    "api",
                    "v1",
                    "providers",
                    provider,
                    "models",
                    "sync",
                    "preview",
                ],
                None,
            ),
            runtime,
            ctx,
        );
    }
    fn apply_body(&mut self) -> Option<Value> {
        if self.consumed || self.job.busy || self.selected.is_empty() {
            return None;
        }
        let id = self.preview["review_id"].as_str()?;
        let changes = self.preview["changes"].as_array()?;
        for id in &self.selected {
            let entry = changes
                .iter()
                .find(|v| v["model_id"].as_str() == Some(id))?;
            if !entry["existing"].is_null() && !self.replace {
                return None;
            }
        }
        self.consumed = true;
        Some(
            json!({"review_id":id,"model_ids":self.selected,"replace_existing_metadata":self.replace}),
        )
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        provider: &str,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if !self.provider.is_empty() && self.provider != provider {
            self.job.cancel_observation();
            *self = Self::default();
        }
        if let Some(message) = self.job.poll() {
            if let Ok(value) = message.result {
                let matches = value["provider_id"].as_str() == Some(self.provider.as_str());
                if message.kind == "preview" && matches && value["state"] == "ready" {
                    self.preview = value;
                } else if matches && value["review_id"] == self.preview["review_id"] {
                    if value["state"] != "ready" {
                        self.consumed = true;
                    }
                    self.result = value;
                } else {
                    self.job.status =
                        "Unexpected review identity; no changes can be applied".into();
                    self.consumed = true;
                }
            }
        }
        if self.provider.is_empty() {
            return;
        }
        egui::CollapsingHeader::new("Review model catalog changes").default_open(true).show(ui,|ui|{
            self.job.status_ui(ui);
            ui.label("Discovery is not a capability test. New models stay disabled. Existing prices, access, enabled state and capability overrides are preserved. Missing IDs never trigger removal.");
            if let Some(changes)=self.preview["changes"].as_array(){for change in changes{
                if let Some(id)=change["model_id"].as_str(){let mut selected=self.selected.contains(id);if ui.add_enabled(!self.job.busy&&!self.consumed&&change["kind"]!="unchanged",egui::Checkbox::new(&mut selected,format!("{} · {}",id,change["kind"].as_str().unwrap_or("unknown")))).changed(){if selected{self.selected.insert(id.into());}else{self.selected.remove(id);}}
                    ui.collapsing(format!("{id}: exact changes and preserved settings"),|ui|json_view(ui,change));
                }
            }}
            ui.collapsing("Warnings, retained models and skipped discovery",|ui|{for key in ["discovery_warnings","retained_models","skipped_models"]{ui.label(key);json_view(ui,&self.preview[key]);}});
            ui.collapsing("Accessible Agent references (not a complete impact inventory)",|ui|{ui.label(self.preview["reference_coverage"].as_str().unwrap_or("Not assessed"));json_view(ui,&self.preview["affected_references"]);});
            ui.add_enabled_ui(!self.consumed&&!self.job.busy,|ui|{ui.checkbox(&mut self.replace,"I reviewed and approve replacing the selected existing catalog metadata, including any manual values in the changed fields");});
            if ui.add_enabled(!self.consumed&&!self.job.busy&&!self.selected.is_empty(),egui::Button::new("Apply selected reviewed changes")).clicked(){if let Some(body)=self.apply_body(){self.job.request("apply",api,ApiRequest::mutation(Method::POST,&["api","v1","providers",&self.provider,"models","sync","apply"],Some(body)),runtime,ctx);}}
            if let Some(id)=self.preview["review_id"].as_str(){if ui.add_enabled(!self.job.busy,egui::Button::new("Read back review and current models")).clicked(){self.job.request("readback",api,ApiRequest::get(&["api","v1","providers",&self.provider,"models","sync","reviews",id]),runtime,ctx);}}
            if self.consumed{ui.label("This receipt cannot be applied again. If the outcome is unknown, read back before preparing a new review. Closing the app loses this session-only receipt.");}
            if !self.result.is_null(){json_view(ui,&self.result);}
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn ready() -> ProviderCatalog {
        ProviderCatalog {
            preview: json!({"review_id":"receipt","changes":[{"model_id":"m","existing":{"display_name":"manual"}}]}),
            selected: BTreeSet::from(["m".into()]),
            ..Default::default()
        }
    }
    #[test]
    fn existing_selection_requires_ack_and_is_single_use() {
        let mut s = ready();
        assert!(s.apply_body().is_none());
        s.replace = true;
        assert_eq!(s.apply_body().unwrap()["review_id"], "receipt");
        assert!(s.apply_body().is_none());
    }
    #[test]
    fn absent_and_empty_models_never_apply() {
        let mut s = ready();
        s.replace = true;
        s.selected.clear();
        assert!(s.apply_body().is_none());
        s.selected.insert("not-reviewed".into());
        assert!(s.apply_body().is_none());
        assert!(!s.consumed);
    }
    #[test]
    fn new_model_needs_selection_but_not_overwrite_ack() {
        let mut s = ready();
        s.preview["changes"][0]["existing"] = Value::Null;
        assert!(s.apply_body().is_some());
    }
    #[test]
    fn nested_auth_expiry_reaches_the_existing_app_boundary() {
        let mut catalog = ProviderCatalog::default();
        let mut parent = RemoteJob::default();
        catalog.propagate_expiry(&mut parent);
        assert!(!parent.expired);
        catalog.job.expired = true;
        catalog.propagate_expiry(&mut parent);
        assert!(parent.expired);
    }
}
