//! Usage and reliability query controls preserve each endpoint's supported filters.
use super::super::api::{ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use serde_json::Value;
pub struct UsageExplorer {
    pub job: RemoteJob,
    start: String,
    end: String,
    user: String,
    model: String,
    service: String,
    assistant: String,
    provider: String,
    dimension: String,
    granularity: String,
    status: String,
    error_type: String,
    limit: u32,
    trace_id: String,
    sort: String,
    result: Value,
    view: String,
}
impl Default for UsageExplorer {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            start: String::new(),
            end: String::new(),
            user: String::new(),
            model: String::new(),
            service: String::new(),
            assistant: String::new(),
            provider: String::new(),
            dimension: "model".into(),
            granularity: "day".into(),
            status: String::new(),
            error_type: String::new(),
            limit: 20,
            trace_id: String::new(),
            sort: "status".into(),
            result: Value::Null,
            view: String::new(),
        }
    }
}
impl UsageExplorer {
    fn filtered(&self, path: &str, keys: &[&str]) -> ApiRequest {
        let mut r = ApiRequest::get(&["api", "v1", "usage", path]);
        for (key, value) in [
            ("start_date", &self.start),
            ("end_date", &self.end),
            ("user_id", &self.user),
            ("model", &self.model),
            ("service_id", &self.service),
            ("assistant_id", &self.assistant),
            ("provider", &self.provider),
            ("status", &self.status),
            ("error_type", &self.error_type),
        ] {
            if keys.contains(&key) && !value.trim().is_empty() {
                r = r.query(key, value);
            }
        }
        r
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if let Some(message) = self.job.poll() {
            if let Ok(value) = message.result {
                self.view = message.kind;
                self.result = value;
            }
        }
        egui::CollapsingHeader::new("Usage and reliability explorer")
            .default_open(true)
            .show(ui, |ui| {
                self.job.status_ui(ui);
                ui.add_enabled_ui(!self.job.busy, |ui| {
                    text_field(ui, "Start date", &mut self.start);
                    text_field(ui, "End date", &mut self.end);
                    text_field(ui, "User ID", &mut self.user);
                    text_field(ui, "Model", &mut self.model);
                    text_field(ui, "Service ID", &mut self.service);
                    text_field(ui, "Assistant ID", &mut self.assistant);
                    text_field(ui, "Provider", &mut self.provider);
                    select_text(
                        ui,
                        "usage_dimension",
                        &mut self.dimension,
                        &["model", "user", "assistant", "service", "provider"],
                    );
                    select_text(
                        ui,
                        "usage_granularity",
                        &mut self.granularity,
                        &["hour", "day", "week", "month"],
                    );
                    ui.add(
                        egui::DragValue::new(&mut self.limit)
                            .range(1..=1000)
                            .prefix("Limit "),
                    );
                    ui.horizontal_wrapped(|ui| {
                        for (view, label) in [
                            ("summary", "Summary"),
                            ("breakdown", "Cost breakdown"),
                            ("timeseries", "Time series"),
                            ("performance-breakdown", "Latency breakdown"),
                            ("failure-breakdown", "Failures"),
                        ] {
                            if ui.button(label).clicked() {
                                let keys: &[&str] = match view {
                                    "summary" => &[
                                        "start_date",
                                        "end_date",
                                        "user_id",
                                        "model",
                                        "service_id",
                                        "assistant_id",
                                        "provider",
                                    ],
                                    "breakdown" => {
                                        &["start_date", "end_date", "user_id", "service_id"]
                                    }
                                    _ => &[
                                        "start_date",
                                        "end_date",
                                        "user_id",
                                        "model",
                                        "service_id",
                                        "provider",
                                    ],
                                };
                                let mut r = self.filtered(view, keys);
                                if ["breakdown", "failure-breakdown"].contains(&view) {
                                    let dimension = if view == "failure-breakdown"
                                        && self.dimension == "assistant"
                                    {
                                        "service"
                                    } else {
                                        &self.dimension
                                    };
                                    r = r.query("dimension", dimension).query("limit", self.limit);
                                }
                                if ["timeseries", "performance-breakdown"].contains(&view) {
                                    r = r.query("granularity", &self.granularity);
                                }
                                self.job
                                    .request(&format!("load_{view}"), api, r, runtime, ctx);
                            }
                        }
                    });
                    egui::CollapsingHeader::new("Request traces").show(ui, |ui| {
                        select_text(
                            ui,
                            "trace_status",
                            &mut self.status,
                            &["", "success", "error"],
                        );
                        text_field(ui, "Error type", &mut self.error_type);
                        if ui.button("Search traces").clicked() {
                            self.job.request(
                                "load_traces",
                                api,
                                self.filtered(
                                    "traces",
                                    &[
                                        "start_date",
                                        "end_date",
                                        "service_id",
                                        "user_id",
                                        "provider",
                                        "model",
                                        "status",
                                        "error_type",
                                    ],
                                )
                                .query("limit", self.limit),
                                runtime,
                                ctx,
                            );
                        }
                        text_field(ui, "Request ID", &mut self.trace_id);
                        if ui
                            .add_enabled(
                                !self.trace_id.is_empty(),
                                egui::Button::new("Load request timeline"),
                            )
                            .clicked()
                        {
                            self.job.request(
                                "load_trace",
                                api,
                                ApiRequest::get(&["api", "v1", "usage", "traces", &self.trace_id]),
                                runtime,
                                ctx,
                            );
                        }
                    });
                    egui::CollapsingHeader::new("Quota overview").show(ui, |ui| {
                        select_text(
                            ui,
                            "quota_sort",
                            &mut self.sort,
                            &["daily_tokens", "monthly_cost", "status"],
                        );
                        if ui.button("Load quota overview").clicked() {
                            self.job.request(
                                "load_quota",
                                api,
                                ApiRequest::get(&["api", "v1", "quota", "users-overview"])
                                    .query("limit", self.limit)
                                    .query("sort_by", &self.sort),
                                runtime,
                                ctx,
                            );
                        }
                    });
                });
                if !self.result.is_null() {
                    ui.strong(&self.view);
                    if let Some(status) = self.result["data_status"].as_str() {
                        ui.label(format!(
                            "Data status: {status} · freshness: {} min · last ingested: {}",
                            self.result["data_freshness_minutes"], self.result["last_ingested_at"]
                        ));
                    }
                    if let Some(rate) = self.result["success_rate"].as_f64() {
                        ui.label(format!("Success rate: {rate}"));
                    } else if self.view == "load_summary" {
                        ui.label("Success rate unavailable; no synthetic zero shown.");
                    }
                    json_view(ui, &self.result);
                }
            });
    }
}
