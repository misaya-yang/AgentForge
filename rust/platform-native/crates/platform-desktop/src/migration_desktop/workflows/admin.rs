//! Native user/role access editors and gateway settings; permission checks remain server-authoritative.
use super::super::{
    api::{ApiRequest, DesktopApi},
    catalog::Section,
};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
pub struct AdminEditor {
    pub job: RemoteJob,
    review: ReviewedAction,
    result: Value,
    user_id: String,
    loaded: String,
    email: String,
    name: String,
    department: String,
    roles: String,
    extra: String,
    status: String,
    tier: String,
    mode: String,
    allow: String,
    deny: String,
    role_catalog: Value,
    permission_catalog: Value,
    service_catalog: Value,
    service_loaded: bool,
    search: String,
    page: u32,
    page_size: u32,
    filter_status: String,
    key_id: String,
    key_name: String,
    key_description: String,
    key_expiry: String,
    key_read: bool,
    key_write: bool,
    include_inactive: bool,
    issued: IssuedSecret,
    jwt: bool,
    jwt_secret: String,
    jwt_algorithms: String,
    jwt_issuer: String,
    key_auth: bool,
    key_header: String,
    auth_loaded: bool,
    lb: String,
    scope: String,
    scope_id: String,
    requests: u32,
    window: u32,
    burst: u32,
}
impl Default for AdminEditor {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            review: ReviewedAction::default(),
            result: Value::Null,
            user_id: String::new(),
            loaded: String::new(),
            email: String::new(),
            name: String::new(),
            department: String::new(),
            roles: "user".into(),
            extra: String::new(),
            status: "active".into(),
            tier: "normal".into(),
            mode: "all".into(),
            allow: String::new(),
            deny: String::new(),
            role_catalog: Value::Null,
            permission_catalog: Value::Null,
            service_catalog: Value::Null,
            service_loaded: false,
            search: String::new(),
            page: 1,
            page_size: 20,
            filter_status: String::new(),
            key_id: String::new(),
            key_name: String::new(),
            key_description: String::new(),
            key_expiry: String::new(),
            key_read: true,
            key_write: true,
            include_inactive: false,
            issued: IssuedSecret::default(),
            jwt: false,
            jwt_secret: String::new(),
            jwt_algorithms: "HS256".into(),
            jwt_issuer: String::new(),
            key_auth: false,
            key_header: "X-API-Key".into(),
            auth_loaded: false,
            lb: "round_robin".into(),
            scope: "global".into(),
            scope_id: String::new(),
            requests: 100,
            window: 60,
            burst: 10,
        }
    }
}
fn choices(
    ui: &mut egui::Ui,
    rows: &Value,
    key: &str,
    label: &str,
    selected: &mut String,
    locked: &[String],
) {
    let mut values = lines(selected);
    if let Some(rows) = rows.as_array() {
        for row in rows {
            let Some(id) = row[key].as_str() else {
                continue;
            };
            let inherited = locked.iter().any(|s| s == id);
            let mut checked = inherited || values.iter().any(|s| s == id);
            if ui
                .add_enabled(
                    !inherited,
                    egui::Checkbox::new(
                        &mut checked,
                        format!(
                            "{}{}",
                            row[label].as_str().unwrap_or(id),
                            if inherited { " (from role)" } else { "" }
                        ),
                    ),
                )
                .changed()
            {
                values.retain(|s| s != id);
                if checked {
                    values.push(id.into());
                }
            }
        }
    }
    *selected = values.join("\n");
}
impl AdminEditor {
    fn accept_user(&mut self, v: &Value) {
        self.loaded = v["user_id"].as_str().unwrap_or("").into();
        self.user_id = self.loaded.clone();
        self.email = v["email"].as_str().unwrap_or("").into();
        self.name = v["display_name"].as_str().unwrap_or("").into();
        self.department = v["department"].as_str().unwrap_or("").into();
        self.roles = string_list(&v["roles"]);
        self.extra = string_list(&v["extra_permissions"]);
        self.mode = v["service_access_mode"].as_str().unwrap_or("all").into();
        self.allow = string_list(&v["allowed_services"]);
        self.deny = string_list(&v["denied_services"]);
        self.status = v["status"].as_str().unwrap_or("active").into();
        self.tier = v["tier"].as_str().unwrap_or("normal").into();
        self.review.clear();
    }
    fn poll(&mut self) {
        if let Some(message) = self.job.poll() {
            if let Ok(mut value) = message.result {
                match message.kind.as_str() {
                    "load_user" | "save_user" | "create_user" => self.accept_user(&value),
                    "load_roles" => self.role_catalog = value.clone(),
                    "load_permissions" => self.permission_catalog = value.clone(),
                    "load_services" => {
                        self.service_catalog = value.clone();
                        self.service_loaded = true;
                    }
                    "issue_key" => {
                        self.issued
                            .set(value["api_key"].as_str().unwrap_or("").into());
                        if let Some(obj) = value.as_object_mut() {
                            obj.remove("api_key");
                        }
                    }
                    "load_auth" => {
                        let v = value.get("runtime").unwrap_or(&value);
                        self.jwt = v["jwt_enabled"].as_bool().unwrap_or(false);
                        self.key_auth = v["api_key_enabled"].as_bool().unwrap_or(false);
                        self.jwt_issuer = v["jwt_issuer"].as_str().unwrap_or("").into();
                        self.jwt_algorithms = string_list(&v["jwt_algorithms"]);
                        self.key_header =
                            v["api_key_header"].as_str().unwrap_or("X-API-Key").into();
                        self.jwt_secret.clear();
                        self.auth_loaded = true;
                    }
                    "load_lb" => {
                        self.lb = value["runtime"]["strategy"]
                            .as_str()
                            .or_else(|| value["strategy"].as_str())
                            .unwrap_or("round_robin")
                            .into()
                    }
                    _ => {}
                }
                self.result = value;
            }
        }
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        section: Section,
        selected: &str,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.poll();
        egui::CollapsingHeader::new("Administration")
            .default_open(true)
            .show(ui, |ui| {
                self.job.status_ui(ui);
                ui.add_enabled_ui(!self.job.busy, |ui| match section {
                    Section::Users => self.users(ui, api, selected, runtime, ctx),
                    Section::Roles => self.roles_ui(ui, api, runtime, ctx),
                    Section::ApiKeys => self.keys(ui, api, selected, runtime, ctx),
                    Section::Settings => self.settings(ui, api, runtime, ctx),
                    _ => {}
                });
                self.review.ui(ui, &mut self.job, api, runtime, ctx);
                self.issued.ui(ui);
                if !self.result.is_null() {
                    json_view(ui, &self.result);
                }
            });
    }
    fn users(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        selected: &str,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        text_field(ui, "Search users", &mut self.search);
        select_text(
            ui,
            "user_filter",
            &mut self.filter_status,
            &["", "active", "disabled", "pending"],
        );
        ui.add(
            egui::DragValue::new(&mut self.page)
                .range(1..=100000)
                .prefix("Page "),
        );
        ui.add(
            egui::DragValue::new(&mut self.page_size)
                .range(1..=100)
                .prefix("Page size "),
        );
        if ui.button("Search / reload users").clicked() {
            let mut r = ApiRequest::get(&["api", "v1", "users"])
                .query("page", self.page)
                .query("page_size", self.page_size);
            if !self.search.is_empty() {
                r = r.query("search", &self.search);
            }
            if !self.filter_status.is_empty() {
                r = r.query("status", &self.filter_status);
            }
            self.job.request("load_users", api, r, runtime, ctx);
        }
        text_field(ui, "User ID", &mut self.user_id);
        ui.horizontal(|ui| {
            if ui.button("Use selected").clicked() {
                self.user_id = selected.into();
                self.review.clear();
            }
            if ui
                .add_enabled(!self.user_id.is_empty(), egui::Button::new("Load user"))
                .clicked()
            {
                self.job.request(
                    "load_user",
                    api,
                    ApiRequest::get(&["api", "v1", "users", &self.user_id]),
                    runtime,
                    ctx,
                );
            }
            if ui.button("New user").clicked() {
                self.loaded.clear();
                self.user_id.clear();
                self.email.clear();
                self.name.clear();
                self.department.clear();
                self.roles = "user".into();
                self.extra.clear();
                self.review.clear();
            }
        });
        if self.loaded.is_empty() {
            text_field(ui, "Email", &mut self.email);
        } else {
            ui.label(format!("Email: {}", self.email));
        }
        text_field(ui, "Display name", &mut self.name);
        text_field(ui, "Department", &mut self.department);
        self.roles_ui(ui, api, runtime, ctx);
        ui.label("Role names, one per line");
        ui.text_edit_multiline(&mut self.roles);
        choices(
            ui,
            &self.role_catalog["roles"],
            "role_name",
            "role_name",
            &mut self.roles,
            &[],
        );
        if !self.loaded.is_empty() {
            select_text(
                ui,
                "user_status",
                &mut self.status,
                &["active", "disabled", "pending"],
            );
            text_field(ui, "Tier", &mut self.tier);
            let mut inherited = Vec::new();
            let selected_roles = lines(&self.roles);
            if let Some(roles) = self.role_catalog["roles"].as_array() {
                for role in roles {
                    if role["role_name"]
                        .as_str()
                        .is_some_and(|name| selected_roles.iter().any(|s| s == name))
                    {
                        inherited.extend(lines(&string_list(&role["permissions"])));
                    }
                }
            }
            ui.label("Additional direct permissions");
            ui.text_edit_multiline(&mut self.extra);
            egui::CollapsingHeader::new("Permission picker").show(ui, |ui| {
                choices(
                    ui,
                    &self.permission_catalog["permissions"],
                    "permission_code",
                    "name",
                    &mut self.extra,
                    &inherited,
                )
            });
            egui::CollapsingHeader::new("Service access policy").show(ui, |ui| {
                if ui.button("Load service catalog").clicked() {
                    self.job.request(
                        "load_services",
                        api,
                        ApiRequest::get(&["api", "v1", "services"]),
                        runtime,
                        ctx,
                    );
                }
                ui.label("Service policy is included in saves only after this catalog is loaded.");
                ui.add_enabled_ui(self.service_loaded, |ui| {
                    select_text(ui, "service_mode", &mut self.mode, &["all", "allowlist"]);
                    ui.label("Allowed service IDs, one per line");
                    ui.text_edit_multiline(&mut self.allow);
                    ui.label("Denied service IDs, one per line (denial takes precedence)");
                    ui.text_edit_multiline(&mut self.deny);
                    if !self.service_catalog.is_null() {
                        json_view(ui, &self.service_catalog);
                    }
                });
            });
        }
        if ui
            .add_enabled(
                !self.name.is_empty() && (self.loaded.is_empty() || self.loaded == self.user_id),
                egui::Button::new("Save user…"),
            )
            .clicked()
        {
            let mut body = json!({"display_name":self.name,"roles":lines(&self.roles)});
            if !self.department.is_empty() {
                body["department"] = json!(self.department);
            }
            if self.loaded.is_empty() {
                body["email"] = json!(self.email);
                self.review.prepare(
                    "create_user",
                    "Create user and assign these roles",
                    ApiRequest::mutation(Method::POST, &["api", "v1", "users"], Some(body)),
                );
            } else {
                body["extra_permissions"] = json!(lines(&self.extra));
                body["status"] = json!(self.status);
                body["tier"] = json!(self.tier);
                if self.service_loaded {
                    body["service_access_mode"] = json!(self.mode);
                    body["allowed_services"] = json!(lines(&self.allow));
                    body["denied_services"] = json!(lines(&self.deny));
                }
                self.review.prepare(
                    "save_user",
                    "Update this user's access and profile",
                    ApiRequest::mutation(
                        Method::PUT,
                        &["api", "v1", "users", &self.loaded],
                        Some(body),
                    ),
                );
            }
        }
        if !self.loaded.is_empty() && self.user_id == self.loaded {
            ui.horizontal_wrapped(|ui| {
                for (op, label) in [
                    ("enable", "Enable user…"),
                    ("disable", "Disable user…"),
                    ("reset-password", "Reset password to default…"),
                ] {
                    if ui.button(label).clicked() {
                        self.review.prepare(
                            op,
                            label,
                            ApiRequest::mutation(
                                Method::POST,
                                &["api", "v1", "users", &self.loaded, op],
                                None,
                            ),
                        );
                    }
                }
                if ui.button("Delete user…").clicked() {
                    self.review.prepare(
                        "delete_user",
                        "Delete this user account",
                        ApiRequest::mutation(
                            Method::DELETE,
                            &["api", "v1", "users", &self.loaded],
                            None,
                        ),
                    );
                }
            });
        }
    }
    fn roles_ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        ui.horizontal(|ui| {
            if ui.button("Load roles").clicked() {
                self.job.request(
                    "load_roles",
                    api,
                    ApiRequest::get(&["api", "v1", "roles"]),
                    runtime,
                    ctx,
                );
            }
            if ui.button("Load permissions").clicked() {
                self.job.request(
                    "load_permissions",
                    api,
                    ApiRequest::get(&["api", "v1", "roles", "permissions"]),
                    runtime,
                    ctx,
                );
            }
        });
    }
    fn keys(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        selected: &str,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        ui.checkbox(&mut self.include_inactive, "Include inactive keys");
        if ui.button("Reload my API keys").clicked() {
            self.job.request(
                "load_keys",
                api,
                ApiRequest::get(&["api", "v1", "api-keys"])
                    .query("include_inactive", self.include_inactive),
                runtime,
                ctx,
            );
        }
        text_field(ui, "Key name", &mut self.key_name);
        text_field(ui, "Description", &mut self.key_description);
        text_field(
            ui,
            "Expires at (RFC3339, blank means no expiry)",
            &mut self.key_expiry,
        );
        ui.checkbox(&mut self.key_read, "knowledge:read");
        ui.checkbox(&mut self.key_write, "knowledge:write");
        if ui
            .add_enabled(
                !self.key_name.trim().is_empty(),
                egui::Button::new("Create scoped API key…"),
            )
            .clicked()
        {
            let mut scopes = Vec::new();
            if self.key_read {
                scopes.push("knowledge:read");
            }
            if self.key_write {
                scopes.push("knowledge:write");
            }
            self.issued.clear();
            self.review.prepare("issue_key","Create a key with the shown scopes; it inherits your tier",ApiRequest::mutation(Method::POST,&["api","v1","api-keys"],Some(json!({"name":self.key_name.trim(),"description":optional_text(&self.key_description),"expires_at":optional_text(&self.key_expiry),"scopes":scopes}))));
        }
        text_field(ui, "Key ID", &mut self.key_id);
        if ui.button("Use selected key").clicked() {
            self.key_id = selected.into();
            self.review.clear();
        }
        if !self.key_id.is_empty() {
            ui.horizontal(|ui| {
                if ui.button("Load key metadata").clicked() {
                    self.job.request(
                        "load_key",
                        api,
                        ApiRequest::get(&["api", "v1", "api-keys", &self.key_id]),
                        runtime,
                        ctx,
                    );
                }
                if ui.button("Revoke key…").clicked() {
                    self.review.prepare(
                        "revoke_key",
                        "Immediately revoke key while preserving its record",
                        ApiRequest::mutation(
                            Method::POST,
                            &["api", "v1", "api-keys", &self.key_id, "revoke"],
                            None,
                        ),
                    );
                }
                if ui.button("Delete key record…").clicked() {
                    self.review.prepare(
                        "delete_key",
                        "Delete this key record",
                        ApiRequest::mutation(
                            Method::DELETE,
                            &["api", "v1", "api-keys", &self.key_id],
                            None,
                        ),
                    );
                }
            });
        }
    }
    fn settings(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        ui.horizontal(|ui| {
            for (kind, path, label) in [
                ("load_status", "status", "System status"),
                ("load_auth", "auth", "Load authentication"),
                ("load_rates", "rate-limits", "Load rate limits"),
                ("load_lb", "load-balancer", "Load balancing"),
            ] {
                if ui.button(label).clicked() {
                    self.job.request(
                        kind,
                        api,
                        ApiRequest::get(&["api", "v1", "config", path]),
                        runtime,
                        ctx,
                    );
                }
            }
        });
        egui::CollapsingHeader::new("Authentication configuration").show(ui,|ui|{ui.add_enabled_ui(self.auth_loaded,|ui|{ui.checkbox(&mut self.jwt,"JWT enabled");ui.checkbox(&mut self.key_auth,"API key authentication enabled");text_field(ui,"JWT issuer",&mut self.jwt_issuer);text_field(ui,"JWT algorithms, one per line",&mut self.jwt_algorithms);text_field(ui,"API key header",&mut self.key_header);ui.label("JWT secret (write-only; blank leaves unchanged)");ui.add(egui::TextEdit::singleline(&mut self.jwt_secret).password(true));if ui.button("Apply authentication settings…").clicked(){let mut body=json!({"jwt_enabled":self.jwt,"jwt_algorithms":lines(&self.jwt_algorithms),"jwt_issuer":optional_text(&self.jwt_issuer),"api_key_enabled":self.key_auth,"api_key_header":self.key_header});if !self.jwt_secret.is_empty(){body["jwt_secret"]=json!(std::mem::take(&mut self.jwt_secret));}self.review.prepare_sensitive("save_auth","Change gateway authentication; this can affect access",ApiRequest::mutation(Method::PUT,&["api","v1","config","auth"],Some(body)),true);}});});
        egui::CollapsingHeader::new("Rate limits").show(ui,|ui|{select_text(ui,"rate_scope",&mut self.scope,&["global","ip","user","tenant","service"]);text_field(ui,"Scope ID",&mut self.scope_id);ui.add(egui::DragValue::new(&mut self.requests).range(1..=10000000).prefix("Requests "));ui.add(egui::DragValue::new(&mut self.window).range(1..=86400).prefix("Window seconds "));ui.add(egui::DragValue::new(&mut self.burst).range(0..=10000000).prefix("Burst "));if ui.button("Create rate limit…").clicked(){self.review.prepare("save_rate","Apply this gateway rate limit",ApiRequest::mutation(Method::POST,&["api","v1","config","rate-limits"],Some(json!({"scope":self.scope,"scope_id":self.scope_id,"requests":self.requests,"window":self.window,"burst":self.burst,"enabled":true}))));}});
        egui::CollapsingHeader::new("Load balancing").show(ui, |ui| {
            select_text(
                ui,
                "lb_strategy",
                &mut self.lb,
                &["round_robin", "random", "weighted", "consistent_hash"],
            );
            if ui.button("Apply load-balancing strategy…").clicked() {
                self.review.prepare(
                    "save_lb",
                    "Change live gateway load-balancing strategy",
                    ApiRequest::mutation(
                        Method::PUT,
                        &["api", "v1", "config", "load-balancer"],
                        Some(json!({"strategy":self.lb})),
                    ),
                );
            }
        });
    }
}
