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
    ReleaseIdentity(u64, Result<ai_platform_client::compatibility::GatewayVersion, Error>),
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
                    Err(Error::Status(404)) => "Gateway /version is unavailable; release compatibility is unverified.".into(),
                    Err(Error::Status(503)) => "Gateway release identity is unavailable or disagrees with its manifest; release compatibility is unverified.".into(),
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
                    Err(Error::Status(401)) => {
                        self.clear_auth("Session expired. Sign in again.");
                    }
                    Err(e) => self.status = e.to_string(),
                }
            }
            ResultMessage::Logout(g, result) if g == self.generation => {
                self.logging_out = false;
                self.busy = false;
                match result {
                    Ok(()) | Err(Error::Status(401)) => {
                        self.clear_auth("Signed out");
                    }
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
                    Err(Error::Status(401)) => self.clear_auth("Session expired. Sign in again."),
                    Err(Error::Status(408 | 500..=599) | Error::Transport | Error::Response) => self.clear_auth(
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
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(message) = self.receiver.try_recv() {
            self.handle_message(message, ctx);
        }
        egui::CentralPanel::default().show(ctx, |ui| {
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
