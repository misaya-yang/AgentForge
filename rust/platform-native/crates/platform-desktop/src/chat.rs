//! Chat has its own turn identity; catalog navigation does not cancel durable turns.
use ai_platform_client::{Client, Error, events::Event};
use eframe::egui;
use tokio::sync::{mpsc, watch};
enum Message {
    Started(String, String, String, String),
    Event(Event),
    Failed(Error),
    Stop(String, Result<(), Error>),
}
pub struct Chat {
    prompt: String,
    model: String,
    text: String,
    status: String,
    active: bool,
    stop_pending: bool,
    redacted: bool,
    pub expired: bool,
    thread: Option<String>,
    turn: Option<String>,
    session: String,
    events_url: Option<String>,
    observing: bool,
    cursor: u64,
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
    cancel: Option<watch::Sender<bool>>,
}
impl Default for Chat {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel(32);
        Self {
            prompt: String::new(),
            model: String::new(),
            text: String::new(),
            status: String::new(),
            active: false,
            stop_pending: false,
            redacted: false,
            expired: false,
            thread: None,
            turn: None,
            session: String::new(),
            events_url: None,
            observing: false,
            cursor: 0,
            tx,
            rx,
            cancel: None,
        }
    }
}
impl Drop for Chat {
    fn drop(&mut self) {
        if let Some(c) = &self.cancel {
            let _ = c.send(true);
        }
    }
}
impl Chat {
    fn handle_message(&mut self, message: Message) {
        match message {
            Message::Started(thread, turn, session, url) => {
                self.events_url = Some(url);
                self.observing = true;
                self.thread = Some(thread);
                self.turn = Some(turn);
                self.session = session;
                self.status = "Streaming".into();
            }
            Message::Event(event) => {
                if event.sequence <= self.cursor {
                    return;
                }
                self.cursor = event.sequence;
                let kind = event.payload["event_type"].as_str().unwrap_or_default();
                if kind == "source_access_revoked"
                    || event.payload["data"]["source_access_revoked"] == true
                {
                    self.text.clear();
                    self.redacted = true;
                    self.status = "Source access revoked; content hidden".into();
                }
                if kind == "text_delta" && !self.redacted {
                    let content = event.payload["data"]
                        .as_str()
                        .or_else(|| event.payload["data"]["content"].as_str())
                        .unwrap_or("");
                    if self.text.len().saturating_add(content.len()) <= 1_048_576 {
                        self.text.push_str(content);
                    } else {
                        self.status =
                            "Display limit reached; full result remains on the server".into();
                    }
                }
                if kind == "approval_required" {
                    self.status =
                        "Approval required. Review it in the web console, or Stop.".into();
                }
                if kind == "approval_result" {
                    self.status = "Approval resolved; awaiting next event".into();
                }
                if event.terminal {
                    self.active = false;
                    self.observing = false;
                    self.stop_pending = false;
                    let outcome = if event.payload["data"]["status"] == "cancelled"
                        || event.payload["data"]["terminal_envelope"]["status"] == "cancelled"
                    {
                        "cancelled"
                    } else {
                        kind
                    };
                    self.status = format!("Turn ended: {outcome}");
                }
            }
            Message::Failed(e) => {
                self.observing = false;
                self.expired = matches!(e, Error::Status(401));
                self.status = format!("{e}. No turn was automatically resubmitted.");
                self.active = self.turn.is_some() && !self.expired;
                if self.active {
                    self.status
                        .push_str(" Remote outcome unresolved; Stop remains available.");
                }
            }
            Message::Stop(turn, result) => {
                if !self.active || self.turn.as_deref() != Some(turn.as_str()) {
                    return;
                }
                self.stop_pending = false;
                self.expired = matches!(&result, Err(Error::Status(401)));
                self.status = match result {
                    Ok(()) => "Stop requested; awaiting terminal event".into(),
                    Err(e) => format!("Stop request failed: {e}"),
                };
            }
        }
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        client: &Client,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        while let Ok(message) = self.rx.try_recv() {
            self.handle_message(message);
        }
        ui.separator();
        ui.heading("Chat");
        if ui
            .add_enabled(!self.active, egui::Button::new("New conversation"))
            .clicked()
        {
            *self = Self::default();
        }
        ui.label(&self.status);
        ui.label(format!("Session: {}", self.session));
        ui.add_enabled_ui(!self.active, |ui| {
            ui.label("Model ID (blank uses server default)");
            ui.text_edit_singleline(&mut self.model);
            ui.add(egui::TextEdit::multiline(&mut self.prompt).desired_rows(3));
            if ui.button("Send").clicked() && !self.prompt.trim().is_empty() {
                self.active = true;
                self.observing = true;
                self.cursor = 0;
                self.events_url = None;
                self.stop_pending = false;
                self.redacted = false;
                self.turn = None;
                self.status = "Submitting once".into();
                self.text.clear();
                let client = client.clone();
                let tx = self.tx.clone();
                let ctx = ctx.clone();
                let prompt = std::mem::take(&mut self.prompt);
                let model = self.model.clone();
                let existing = self.thread.clone();
                let session = self.session.clone();
                let (cancel, stop) = watch::channel(false);
                self.cancel = Some(cancel);
                runtime.spawn(async move {
                    let result: Result<(), Error> = async {
                        let model = (!model.is_empty()).then_some(model.as_str());
                        let (thread, session) = if let Some(thread) = existing {
                            (thread, session)
                        } else {
                            let result = client.create_thread(None, model).await?;
                            let thread = result["thread"]["thread_id"]
                                .as_str()
                                .ok_or(Error::Response)?
                                .to_owned();
                            let session = result["thread"]["session_id"]
                                .as_str()
                                .ok_or(Error::Response)?
                                .to_owned();
                            (thread, session)
                        };
                        let result = client.create_turn(&thread, &prompt, model).await?;
                        let turn = result["turn"]["id"]
                            .as_str()
                            .ok_or(Error::Response)?
                            .to_owned();
                        let url = result["turn"]["events_url"]
                            .as_str()
                            .ok_or(Error::Response)?
                            .to_owned();
                        tx.send(Message::Started(
                            thread.clone(),
                            turn.clone(),
                            session,
                            url.clone(),
                        ))
                        .await
                        .map_err(|_| Error::Transport)?;
                        ctx.request_repaint();
                        observe(&client, &thread, &turn, &url, &tx, &ctx, stop).await
                    }
                    .await;
                    if let Err(e) = result {
                        let _ = tx.send(Message::Failed(e)).await;
                        ctx.request_repaint();
                    }
                });
            }
        });
        if self.active
            && !self.observing
            && ui.button("Reconnect observation").clicked()
            && let (Some(thread), Some(turn), Some(url)) = (
                self.thread.clone(),
                self.turn.clone(),
                self.events_url.clone(),
            )
        {
            self.observing = true;
            let client = client.clone();
            let tx = self.tx.clone();
            let ctx = ctx.clone();
            let (cancel, stop) = watch::channel(false);
            self.cancel = Some(cancel);
            runtime.spawn(async move {
                if let Err(e) = observe(&client, &thread, &turn, &url, &tx, &ctx, stop).await {
                    let _ = tx.send(Message::Failed(e)).await;
                    ctx.request_repaint();
                }
            });
        }
        if self.active
            && ui
                .add_enabled(
                    !self.stop_pending && self.turn.is_some(),
                    egui::Button::new("Stop turn"),
                )
                .clicked()
            && let (Some(thread), Some(turn)) = (self.thread.clone(), self.turn.clone())
        {
            self.stop_pending = true;
            let client = client.clone();
            let tx = self.tx.clone();
            let ctx = ctx.clone();
            runtime.spawn(async move {
                let result = client.interrupt(&thread, &turn).await.map(|_| ());
                let _ = tx.send(Message::Stop(turn, result)).await;
                ctx.request_repaint();
            });
        }
        egui::ScrollArea::vertical()
            .max_height(300.0)
            .show(ui, |ui| {
                ui.label(&self.text);
            });
    }
}

async fn observe(
    client: &Client,
    thread: &str,
    turn: &str,
    url: &str,
    tx: &mpsc::Sender<Message>,
    ctx: &egui::Context,
    stop: watch::Receiver<bool>,
) -> Result<(), Error> {
    let (events, mut receiver) = mpsc::channel(16);
    let stream = client.watch_turn(thread, turn, url, events, stop);
    tokio::pin!(stream);
    loop {
        tokio::select! {
            result=&mut stream=>{
                // Drain terminal events already queued before stream completion.
                while let Ok(event)=receiver.try_recv(){
                    tx.send(Message::Event(event?)).await.map_err(|_|Error::Transport)?;
                }
                ctx.request_repaint();return result;
            },
            event=receiver.recv()=>if let Some(event)=event{
                tx.send(Message::Event(event?)).await.map_err(|_|Error::Transport)?;ctx.request_repaint();
            }
        }
    }
}

