//! Bounded local-node event history with a device-scoped replay cursor.
use super::{super::api::DesktopApi, common::*};
use ai_platform_client::management::{NodeEventCursor, node_events_request};
use eframe::egui;
use serde_json::Value;
use std::time::{Duration, Instant};
pub struct NodeEvents {
    pub job: RemoteJob,
    device: String,
    cursor: NodeEventCursor,
    events: Vec<Value>,
    watching: bool,
    next_poll: Instant,
}
impl Default for NodeEvents {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            device: String::new(),
            cursor: NodeEventCursor::default(),
            events: Vec::new(),
            watching: false,
            next_poll: Instant::now(),
        }
    }
}
impl NodeEvents {
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        device: &str,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if device != self.device {
            self.job.cancel_observation();
            *self = Self {
                device: device.into(),
                ..Self::default()
            };
        }
        if let Some(message) = self.job.poll() {
            match message.result {
                Ok(value) => match self.cursor.accept(&self.device, &value) {
                    Ok(events) => {
                        self.events.extend(events);
                        if self.events.len() > 2000 {
                            self.events.drain(..self.events.len() - 2000);
                        }
                    }
                    Err(_) => {
                        self.job.status = "Invalid event batch; cursor unchanged".into();
                        self.watching = false;
                    }
                },
                Err(_) => self.watching = false,
            }
        }
        egui::CollapsingHeader::new("Device event history")
            .default_open(true)
            .show(ui, |ui| {
                self.job.status_ui(ui);
                let mut load = false;
                ui.add_enabled_ui(!device.is_empty() && !self.job.busy, |ui| {
                    ui.horizontal(|ui| {
                        load = ui.button("Load next events").clicked();
                        ui.checkbox(&mut self.watching, "Watch events");
                        if ui.button("Restart history").clicked() {
                            self.cursor = NodeEventCursor::default();
                            self.events.clear();
                            load = true;
                        }
                        ui.label(format!(
                            "Cursor {} · latest {} retained",
                            self.cursor.after_sequence,
                            self.events.len()
                        ));
                    });
                });
                if self.watching {
                    ctx.request_repaint_after(Duration::from_secs(1));
                }
                if !device.is_empty()
                    && !self.job.busy
                    && (load || self.watching && Instant::now() >= self.next_poll)
                {
                    if let Ok(request) =
                        node_events_request(device, self.cursor.after_sequence, 500)
                    {
                        self.job.request("load events", api, request, runtime, ctx);
                    }
                    self.next_poll = Instant::now() + Duration::from_secs(2);
                }
                egui::ScrollArea::vertical()
                    .id_salt("local_node_event_history")
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for event in &self.events {
                            let sequence = event["sequence"].as_u64().unwrap_or(0);
                            egui::CollapsingHeader::new(format!(
                                "{sequence} · {} · {}",
                                event["event"].as_str().unwrap_or("event"),
                                event["status"].as_str().unwrap_or("")
                            ))
                            .id_salt(sequence)
                            .show(ui, |ui| json_view(ui, event));
                        }
                    });
            });
    }
}
