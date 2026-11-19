mod chat;
mod migration_desktop;
mod package_info;
use ai_platform_client::{Client, Error};
use eframe::egui;
use serde_json::Value;
use std::sync::mpsc::{self, Receiver, Sender};

enum ResultMessage {
    Login(
        u64,
        Result<(Client, bool, migration_desktop::DesktopApi), String>,
    ),
    ReleaseIdentity(
        u64,
        Result<ai_platform_client::compatibility::GatewayVersion, Error>,
    ),
    PasswordChanged(u64, Result<(), Error>),
    Data(u64, Result<Value, Error>),
    Logout(u64, Result<(), Error>),
}
struct App {
    runtime: tokio::runtime::Runtime,
    sender: Sender<ResultMessage>,
    receiver: Receiver<ResultMessage>,
    origin: String,
    email: String,
    password: String,
    client: Option<Client>,
    chat: Option<chat::Chat>,
    workspace: Option<migration_desktop::DesktopWorkspace>,
    generation: u64,
    read_task: Option<tokio::task::AbortHandle>,
    busy: bool,
    logging_out: bool,
    changing_password: bool,
    password_required: bool,
    current_password: String,
    new_password: String,
    confirm_password: String,
    status: String,
    release_status: String,
    release_generation: Option<u64>,
    data: Value,
    session: String,
}
impl Default for App {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            runtime: tokio::runtime::Runtime::new().expect("Async runtime"),
            sender,
            receiver,
            origin: "http://127.0.0.1:8000".into(),
            email: String::new(),
            password: String::new(),
            client: None,
            chat: None,
            workspace: None,
            generation: 0,
            read_task: None,
            busy: false,
            logging_out: false,
            changing_password: false,
            password_required: false,
            current_password: String::new(),
            new_password: String::new(),
            confirm_password: String::new(),
            status: "Connect to your existing Gateway".into(),
            release_status: String::new(),
            release_generation: None,
            data: Value::Null,
            session: String::new(),
        }
    }
}
impl App {
    fn clear_auth(&mut self, status: &str) {
        self.generation += 1;
        if let Some(task) = self.read_task.take() {
            task.abort();
        }
        self.busy = false;
        self.logging_out = false;
        self.changing_password = false;
        self.password_required = false;
        self.current_password.clear();
        self.new_password.clear();
        self.confirm_password.clear();
        self.client = None;
        self.chat = None;
        if let Some(mut workspace) = self.workspace.take() {
            workspace.disconnect();
        }
        self.data = Value::Null;
        self.session.clear();
        self.password.clear();
        self.status = status.into();
        self.release_status.clear();
        self.release_generation = None;
    }

    fn handle_message(&mut self, message: ResultMessage, ctx: &egui::Context) {
        match message {
            ResultMessage::Login(g, result) if g == self.generation => {
                self.busy = false;
                match result {
                    Ok((c, required, api)) => {
                        self.workspace = Some(migration_desktop::DesktopWorkspace::new(api));
                        if !required {
                            self.release_generation = Some(g);
                            self.release_status = "Checking Gateway release identity…".into();
                            let sender = self.sender.clone();
                            let probe = c.clone();
                            let ctx = ctx.clone();
                            self.runtime.spawn(async move {
                                let result = probe.gateway_version().await;
                                let _ = sender.send(ResultMessage::ReleaseIdentity(g, result));
                                ctx.request_repaint();
                            });
                        } else {
                            self.release_status = "Gateway release identity not checked during required password change".into();
                        }
                        self.client = Some(c);
                        self.password_required = required;
                        self.chat = if required {
                            None
                        } else {
                            Some(chat::Chat::default())
                        };
                        self.status = if required {
                            "Change your password to continue"
                        } else {
                            "Connected"
                        }
                        .into();
                    }
                    Err(e) => self.status = e,
                }
            }
            ResultMessage::ReleaseIdentity(g, result) if self.release_generation == Some(g) => {
                self.release_status = match result {
                    Ok(version) => format!(
                        "Gateway release {} · source {}. Runtime/Worker capability compatibility is unverified.",
                        version.release_id, version.git_sha
                    ),
                    Err(error) if error.status() == Some(404) => "Gateway /version is unavailable; release compatibility is unverified.".into(),
                    Err(error) if error.status() == Some(503) => "Gateway release identity is unavailable or disagrees with its manifest; release compatibility is unverified.".into(),
                    Err(error) => format!("Gateway release identity unverified: {error}"),
                };
            }
            ResultMessage::Data(g, result) if g == self.generation => {
                self.busy = false;
                match result {
                    Ok(v) => {
                        self.data = v;
                        self.status = "Loaded".into();
                    }
                    Err(error) if error.status() == Some(401) => {
                        self.clear_auth("Session expired. Sign in again.");
                    }
                    Err(e) => self.status = e.to_string(),
                }
            }
            ResultMessage::Logout(g, result) if g == self.generation => {
                self.logging_out = false;
                self.busy = false;
                match result {
                    Ok(()) => self.clear_auth("Signed out"),
                    Err(error) if error.status() == Some(401) => self.clear_auth("Signed out"),
                    Err(e) => self.status = format!("Sign out failed: {e}"),
                }
            }
            ResultMessage::PasswordChanged(g, result) if g == self.generation => {
                self.busy = false;
                self.changing_password = false;
                match result {
                    Ok(()) => {
                        self.clear_auth("Password changed. Sign in again.");
                    }
                    Err(error) if error.status() == Some(401) => self.clear_auth("Session expired. Sign in again."),
                    Err(error) if matches!(&error, Error::Transport | Error::Response)
                        || matches!(error.status(), Some(408 | 500..=599)) => self.clear_auth(
                        "Password-change outcome unknown. Sign in with your new password to check.",
                    ),
                    Err(e) => self.status = format!("Password change failed: {e}"),
                }
            }
            _ => {}
        }
    }

    fn load(&mut self, kind: u8, ctx: egui::Context) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.busy || self.password_required {
            return;
        }
        if let Some(task) = self.read_task.take() {
            task.abort();
        }
        self.data = Value::Null;
        self.status = match kind {
            0 => "Loading models",
            1 => "Loading knowledge",
            2 => "Loading sessions",
            _ => "Loading history",
        }
        .into();
        self.generation += 1;
        let generation = self.generation;
        self.busy = true;
        let sender = self.sender.clone();
        let id = self.session.clone();
        let task = self.runtime.spawn(async move {
            let result = match kind {
                0 => client.models().await,
                1 => client.datasets().await,
                2 => client.sessions().await,
                _ => client.history(&id).await,
            };
            let _ = sender.send(ResultMessage::Data(generation, result));
            ctx.request_repaint();
        });
        self.read_task = Some(task.abort_handle());
    }
}
impl App {
    fn process_messages(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.receiver.try_recv() {
            self.handle_message(message, ctx);
        }
    }
    fn render_ui(&mut self, root_ui: &mut egui::Ui) {
        let context = root_ui.ctx().clone();
        let ctx = &context;
        self.process_messages(ctx);
        egui::CentralPanel::default().show_inside(root_ui, |ui| {
            ui.heading("AI Platform");
            ui.label("Native Rust desktop · reconstruction preview");
            ui.label(&self.status);
            if !self.release_status.is_empty() {
                ui.label(&self.release_status);
            }
            if self.client.is_none() {
                ui.add_enabled_ui(!self.busy, |ui| {
                    ui.label("Gateway origin");
                    ui.text_edit_singleline(&mut self.origin);
                    ui.label("Email");
                    ui.text_edit_singleline(&mut self.email);
                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                    if ui.button("Sign in").clicked() {
                        match Client::new(&self.origin) {
                            Err(e) => self.status = e.to_string(),
                            Ok(client) => {
                                self.generation += 1;
                                let generation = self.generation;
                                let email = self.email.clone();
                                let origin = self.origin.clone();
                                let password = std::mem::take(&mut self.password);
                                let sender = self.sender.clone();
                                let ctx = ctx.clone();
                                self.busy = true;
                                self.runtime.spawn(async move {
                                    let result = client
                                        .login(email, password)
                                        .await
                                        .map_err(|e| e.to_string())
                                        .and_then(|login| {
                                            let api = migration_desktop::DesktopApi::new(&origin, login.access_token.clone()).map_err(|e| e.to_string())?;
                                            Ok((
                                                client.authenticated(login.access_token),
                                                login.force_password_change,
                                                api,
                                            ))
                                        });
                                    let _ = sender.send(ResultMessage::Login(generation, result));
                                    ctx.request_repaint();
                                });
                            }
                        }
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    if ui.button("Models").clicked() {
                        self.load(0, ctx.clone())
                    }
                    if ui.button("Knowledge").clicked() {
                        self.load(1, ctx.clone())
                    }
                    if ui.button("Sessions").clicked() {
                        self.load(2, ctx.clone())
                    }
                    if ui.button("Sign out").clicked()
                        && !self.busy
                        && let Some(client) = self.client.clone()
                    {
                        self.generation += 1;
                        let g = self.generation;
                        self.busy = true;
                        self.logging_out = true;
                        let sender = self.sender.clone();
                        let ctx = ctx.clone();
                        self.runtime.spawn(async move {
                            let _ = sender.send(ResultMessage::Logout(g, client.logout().await));
                            ctx.request_repaint();
                        });
                    }
                    if ui.button("Disconnect").clicked() {
                        let status = if self.changing_password { "Disconnected. Password-change outcome may be unknown; check with your new password." } else { "Disconnected locally" };
                        self.clear_auth(status);
                    }
                });
                if self.password_required {
                    ui.label(
                        "Use at least 8 characters with a letter, number and special character.",
                    );
                    ui.add_enabled_ui(!self.busy, |ui| {
                        ui.label("Current password");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.current_password).password(true),
                        );
                        ui.label("New password");
                        ui.add(egui::TextEdit::singleline(&mut self.new_password).password(true));
                        ui.label("Confirm new password");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.confirm_password).password(true),
                        );
                        if ui.button("Change password").clicked() {
                            if self.current_password.is_empty()
                                || self.new_password != self.confirm_password
                                || self.new_password.chars().count() < 8
                            {
                                self.status =
                                    "Check the current password, minimum length and confirmation"
                                        .into();
                            } else if let Some(client) = self.client.clone() {
                                self.generation += 1;
                                let g = self.generation;
                                self.busy = true;
                                self.changing_password = true;
                                let current = std::mem::take(&mut self.current_password);
                                let new = std::mem::take(&mut self.new_password);
                                let confirmation = std::mem::take(&mut self.confirm_password);
                                let sender = self.sender.clone();
                                let ctx = ctx.clone();
                                self.runtime.spawn(async move {
                                    let result =
                                        client.change_password(current, new, confirmation).await;
                                    let _ = sender.send(ResultMessage::PasswordChanged(g, result));
                                    ctx.request_repaint();
                                });
                            }
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.label("Session ID");
                    ui.text_edit_singleline(&mut self.session);
                    if ui.button("Read history").clicked() {
                        self.load(3, ctx.clone())
                    }
                });
                if let (Some(chat), Some(client)) = (&mut self.chat, &self.client) {
                    ui.add_enabled_ui(!self.logging_out, |ui| {
                        chat.ui(ui, client, &self.runtime, ctx)
                    });
                }
                if !self.password_required {
                    if let Some(workspace) = &mut self.workspace {
                        ui.add_enabled_ui(!self.logging_out, |ui| {
                            workspace.ui(ui, &self.runtime, ctx);
                        });
                    }
                }
                if self.workspace.as_ref().is_some_and(|workspace| workspace.expired) {
                    self.clear_auth("Session expired");
                }
                if self.chat.as_ref().is_some_and(|c| c.expired) {
                    self.clear_auth("Session expired");
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.monospace(serde_json::to_string_pretty(&self.data).unwrap_or_default());
                });
            }
            if self.busy {
                ui.spinner();
                if !self.logging_out
                    && !self.changing_password
                    && ui.button("Cancel pending display").clicked()
                {
                    self.generation += 1;
                    self.busy = false;
                    self.status = "Pending result discarded".into();
                    if let Some(task) = self.read_task.take() {
                        task.abort();
                    }
                }
            }
        });
    }
}
impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // eframe also calls logic for a hidden window after a requested repaint.
        // Session/result fencing must not depend on drawing a visible frame.
        self.process_messages(ctx);
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render_ui(ui);
    }
}

fn main() -> eframe::Result {
    if let Some(code) = package_info::handle_args() {
        std::process::exit(code);
    }
    eframe::run_native(
        "AI Platform",
        eframe::NativeOptions::default(),
        Box::new(|_| Ok(Box::<App>::default())),
    )
}

#[cfg(test)]
mod problem_session_tests {
    use super::*;
    fn problem(status: u16) -> Error {
        let bytes=serde_json::to_vec(&serde_json::json!({"type":"about:blank","title":"private-marker","status":status,"detail":{"password":"private-marker"},"instance":"https://private-marker.invalid"})).unwrap();
        Error::Problem(Box::new(
            ai_platform_client::problem::RemoteProblem::decode(status, &bytes).unwrap(),
        ))
    }
    fn authenticated() -> App {
        let mut app = App::default();
        app.generation = 12;
        app.client = Some(
            Client::new("https://gateway.example")
                .unwrap()
                .authenticated("synthetic-token".into()),
        );
        app.chat = Some(chat::Chat::default());
        app.session = "session-sensitive".into();
        app.password = "password-sensitive".into();
        app.current_password = "old-sensitive".into();
        app.new_password = "new-sensitive".into();
        app.confirm_password = "new-sensitive".into();
        app.data = serde_json::json!({"private":"cached"});
        app.busy = true;
        app
    }
    fn assert_fenced(app: &App) {
        assert_eq!(app.generation, 13);
        assert!(app.client.is_none() && app.chat.is_none() && app.workspace.is_none());
        assert!(
            app.session.is_empty()
                && app.password.is_empty()
                && app.current_password.is_empty()
                && app.new_password.is_empty()
                && app.confirm_password.is_empty()
        );
        assert_eq!(app.data, Value::Null);
        assert!(!app.busy);
        assert!(!app.status.contains("private-marker"));
    }
    #[test]
    fn problem_unauthorized_fences_data_logout_and_password_and_stale_messages() {
        let ctx = egui::Context::default();
        for kind in 0..3 {
            let mut app = authenticated();
            let message = match kind {
                0 => ResultMessage::Data(12, Err(problem(401))),
                1 => ResultMessage::Logout(12, Err(problem(401))),
                _ => ResultMessage::PasswordChanged(12, Err(problem(401))),
            };
            app.handle_message(message, &ctx);
            assert_fenced(&app);
            app.handle_message(
                ResultMessage::Data(12, Ok(serde_json::json!({"stale":"must-not-return"}))),
                &ctx,
            );
            assert_fenced(&app);
        }
    }
    #[test]
    fn problem_password_timeout_or_server_failure_fences_unknown_outcome() {
        let ctx = egui::Context::default();
        for status in [408, 500, 502, 503, 504, 599] {
            let mut app = authenticated();
            app.handle_message(
                ResultMessage::PasswordChanged(12, Err(problem(status))),
                &ctx,
            );
            assert_fenced(&app);
            assert!(app.status.contains("outcome unknown"));
        }
        for status in [403, 404, 409, 422, 429] {
            let mut app = authenticated();
            app.handle_message(
                ResultMessage::PasswordChanged(12, Err(problem(status))),
                &ctx,
            );
            assert!(app.client.is_some());
            assert_eq!(app.generation, 12);
            assert!(!app.status.contains("private-marker"));
        }
    }
    #[test]
    fn problem_release_absence_and_unavailability_keep_compatibility_unknown() {
        let ctx = egui::Context::default();
        for status in [404, 503] {
            let mut app = authenticated();
            app.release_generation = Some(12);
            app.handle_message(
                ResultMessage::ReleaseIdentity(12, Err(problem(status))),
                &ctx,
            );
            assert!(app.release_status.contains("unverified"));
            assert!(!app.release_status.contains("private-marker"));
            assert!(app.client.is_some());
        }
    }
}

#[cfg(test)]
#[path = "font_rendering_tests.rs"]
mod maintained_font_rendering_tests;
