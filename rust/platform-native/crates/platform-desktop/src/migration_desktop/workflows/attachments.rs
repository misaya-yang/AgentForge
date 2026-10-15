//! Selected-file upload, bounded image/text preview and authenticated artifact download.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use serde_json::Value;
use std::{
    io::{Cursor, Read, Write},
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
};
const MAX_BYTES: usize = 100 * 1024 * 1024;
struct PreviewData {
    bytes: Vec<u8>,
    mime: String,
    name: String,
    text: Option<String>,
    rgba: Option<([usize; 2], Vec<u8>)>,
}
enum Completion {
    Preview(u64, Result<PreviewData, ApiError>),
    Saved(u64, Result<PathBuf, ApiError>),
}
pub struct AttachmentWorkspace {
    pub expired: bool,
    pub job: RemoteJob,
    selected_path: Option<PathBuf>,
    selected_bytes: Option<Vec<u8>>,
    selected_name: String,
    selected_mime: String,
    generation: u64,
    tx: Sender<Completion>,
    rx: Receiver<Completion>,
    busy: bool,
    preview_is_selected: bool,
    status: String,
    preview: Option<PreviewData>,
    texture: Option<egui::TextureHandle>,
    save_path: String,
    save_confirm: bool,
    saved_path: Option<PathBuf>,
    upload_confirm: bool,
    compress: bool,
    files: Value,
    artifacts: Value,
    pub uploaded_path: Option<String>,
    scope: String,
}
impl Default for AttachmentWorkspace {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            expired: false,
            job: RemoteJob::default(),
            selected_path: None,
            selected_bytes: None,
            selected_name: String::new(),
            selected_mime: String::new(),
            generation: 0,
            tx,
            rx,
            busy: false,
            preview_is_selected: false,
            status: String::new(),
            preview: None,
            texture: None,
            save_path: String::new(),
            save_confirm: false,
            saved_path: None,
            upload_confirm: false,
            compress: true,
            files: Value::Null,
            artifacts: Value::Null,
            uploaded_path: None,
            scope: String::new(),
        }
    }
}
fn mime_for(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}
fn supported(name: &str) -> bool {
    [
        "pdf", "docx", "md", "txt", "csv", "xlsx", "png", "jpg", "jpeg", "gif", "webp",
    ]
    .contains(
        &name
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
    )
}
fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage, ApiError> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| ApiError::Response)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|_| ApiError::Response)?;
    if u64::from(image.width()) * u64::from(image.height()) > 32 * 1024 * 1024 {
        return Err(ApiError::Response);
    }
    Ok(image)
}
fn prepare(bytes: Vec<u8>, mime: String, name: String) -> Result<PreviewData, ApiError> {
    if bytes.len() > MAX_BYTES {
        return Err(ApiError::Response);
    }
    let rgba = if mime.starts_with("image/") {
        decode_image(&bytes).ok().map(|img| {
            let img = img.thumbnail(2048, 2048).into_rgba8();
            (
                [img.width() as usize, img.height() as usize],
                img.into_raw(),
            )
        })
    } else {
        None
    };
    let text = if mime.starts_with("text/") || mime == "application/json" {
        std::str::from_utf8(&bytes)
            .ok()
            .map(|s| s.chars().take(1_000_000).collect())
    } else {
        None
    };
    Ok(PreviewData {
        bytes,
        mime,
        name,
        text,
        rgba,
    })
}
fn read_selected(path: Option<PathBuf>, bytes: Option<Vec<u8>>) -> Result<Vec<u8>, ApiError> {
    if let Some(bytes) = bytes {
        if bytes.len() <= MAX_BYTES {
            return Ok(bytes);
        }
        return Err(ApiError::Request);
    }
    let file =
        std::fs::File::open(path.ok_or(ApiError::Request)?).map_err(|_| ApiError::Request)?;
    if !file.metadata().map_err(|_| ApiError::Request)?.is_file() {
        return Err(ApiError::Request);
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ApiError::Request)?;
    if bytes.len() > MAX_BYTES {
        return Err(ApiError::Request);
    }
    Ok(bytes)
}
/// Source compressImage falls back to original bytes on codec/compression errors.
fn compressed(bytes: Vec<u8>, mime: String, enabled: bool) -> (Vec<u8>, String) {
    if !enabled
        || bytes.len() <= 2 * 1024 * 1024
        || !["image/png", "image/jpeg", "image/webp"].contains(&mime.as_str())
    {
        return (bytes, mime);
    }
    let Ok(image) = decode_image(&bytes) else {
        return (bytes, mime);
    };
    let mut image = image.thumbnail(2048, 2048);
    for quality in [85, 75, 60, 45, 30] {
        let mut output = Vec::new();
        let result = if mime == "image/jpeg" {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, quality)
                .encode_image(&image)
        } else {
            let format = if mime == "image/webp" {
                image::ImageFormat::WebP
            } else {
                image::ImageFormat::Png
            };
            image.write_to(&mut Cursor::new(&mut output), format)
        };
        if result.is_err() {
            return (bytes, mime);
        }
        if output.len() <= 2 * 1024 * 1024 {
            return (output, mime);
        }
        image = image.thumbnail(
            (image.width() * 3 / 4).max(1),
            (image.height() * 3 / 4).max(1),
        );
    }
    (bytes, mime)
}
impl AttachmentWorkspace {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Completion::Preview(g, result) if g == self.generation => {
                    self.busy = false;
                    match result {
                        Ok(mut data) => {
                            if self.preview_is_selected {
                                self.selected_bytes = Some(data.bytes.clone());
                                self.selected_path = None;
                            }
                            self.texture = data.rgba.take().map(|(size, bytes)| {
                                ctx.load_texture(
                                    format!("attachment-preview-{g}"),
                                    egui::ColorImage::from_rgba_unmultiplied(size, &bytes),
                                    egui::TextureOptions::LINEAR,
                                )
                            });
                            self.status = "Preview loaded".into();
                            self.preview = Some(data);
                        }
                        Err(error) => {
                            self.expired |= matches!(error, ApiError::Status(401));
                            self.status = error.to_string();
                            self.preview = None;
                            self.texture = None;
                        }
                    }
                }
                Completion::Saved(g, result) if g == self.generation => {
                    self.busy = false;
                    match result {
                        Ok(path) => {
                            self.saved_path = Some(path);
                            self.status =
                                "Saved the downloaded bytes to the selected new file".into();
                        }
                        Err(error) => self.status = error.to_string(),
                    }
                }
                _ => {}
            }
        }
        if let Some(message) = self.job.poll() {
            if let Ok(value) = message.result {
                match message.kind.as_str() {
                    "upload" => {
                        self.uploaded_path = value["file_path"].as_str().map(str::to_owned);
                        self.status =
                            "Uploaded. Add the returned reference to the next turn explicitly."
                                .into();
                    }
                    "load_files" => self.files = value,
                    "load_artifacts" => self.artifacts = value,
                    _ => {}
                }
            }
        }
        self.expired |= self.job.expired;
    }
    fn load_remote(
        &mut self,
        api: &DesktopApi,
        url: String,
        name: String,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if self.busy {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let g = self.generation;
        self.preview = None;
        self.texture = None;
        self.saved_path = None;
        self.preview_is_selected = false;
        self.busy = true;
        let tx = self.tx.clone();
        let api = api.clone();
        let ctx = ctx.clone();
        runtime.spawn(async move {
            let result = async {
                let blob = api.download_blob(&url).await?;
                tokio::task::spawn_blocking(move || prepare(blob.bytes, blob.mime, name))
                    .await
                    .map_err(|_| ApiError::Response)?
            }
            .await;
            let _ = tx.send(Completion::Preview(g, result));
            ctx.request_repaint();
        });
    }
    pub fn ui_upload(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        can_attach: bool,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) -> Option<String> {
        self.poll(ctx);
        let mut attach = None;
        egui::CollapsingHeader::new("Attachments").show(ui, |ui| {
            self.job.status_ui(ui);
            ui.label(&self.status);
            ui.add_enabled_ui(!self.busy && !self.job.busy, |ui| {
                ui.label("Drop a supported file here. Selection does not upload it.");
                if !self.busy && !self.job.busy {
                    for file in ctx.input(|i| i.raw.dropped_files.clone()) {
                        self.selected_name = file
                            .path
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or(file.name);
                        self.selected_mime = if file.mime.is_empty() {
                            mime_for(&self.selected_name).into()
                        } else {
                            file.mime
                        };
                        self.selected_path = file.path;
                        self.selected_bytes = file.bytes.map(|b| b.to_vec());
                        self.upload_confirm = false;
                        self.preview = None;
                        self.texture = None;
                        self.generation = self.generation.wrapping_add(1);
                        break;
                    }
                }
                ui.label(&self.selected_name);
                if !self.selected_name.is_empty() && !supported(&self.selected_name) {
                    ui.label("Unsupported attachment extension");
                }
                if ui
                    .add_enabled(
                        supported(&self.selected_name),
                        egui::Button::new("Read selected file for local preview"),
                    )
                    .clicked()
                {
                    self.generation = self.generation.wrapping_add(1);
                    let g = self.generation;
                    self.busy = true;
                    self.preview_is_selected = true;
                    let path = self.selected_path.clone();
                    let bytes = self.selected_bytes.clone();
                    let name = self.selected_name.clone();
                    let mime = self.selected_mime.clone();
                    let tx = self.tx.clone();
                    let ctx = ctx.clone();
                    runtime.spawn(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            prepare(read_selected(path, bytes)?, mime, name)
                        })
                        .await
                        .map_err(|_| ApiError::Response)
                        .and_then(|v| v);
                        let _ = tx.send(Completion::Preview(g, result));
                        ctx.request_repaint();
                    });
                }
                ui.checkbox(
                    &mut self.compress,
                    "Compress images over 2 MB (2048 px, initial quality 0.85)",
                );
                ui.checkbox(
                    &mut self.upload_confirm,
                    "I reviewed this file and authorize its upload",
                );
                if ui
                    .add_enabled(
                        supported(&self.selected_name)
                            && self.upload_confirm
                            && !self.job.outcome_unknown,
                        egui::Button::new("Upload selected file once"),
                    )
                    .clicked()
                {
                    let api = api.clone();
                    let path = self.selected_path.clone();
                    let supplied = self.selected_bytes.clone();
                    let name = self.selected_name.clone();
                    let mime = self.selected_mime.clone();
                    let compress = self.compress;
                    self.upload_confirm = false;
                    self.job.start(
                        "upload",
                        async move {
                            let (bytes, mime) = tokio::task::spawn_blocking(move || {
                                let bytes = read_selected(path, supplied)?;
                                Ok::<_, ApiError>(compressed(bytes, mime, compress))
                            })
                            .await
                            .map_err(|_| ApiError::Request)??;
                            api.upload(
                                &["api", "v1", "files", "upload"],
                                "file",
                                &name,
                                &mime,
                                bytes,
                            )
                            .await
                        },
                        runtime,
                        ctx,
                    );
                }
                if let Some(path) = &self.uploaded_path {
                    ui.label(path);
                    if ui
                        .add_enabled(
                            can_attach,
                            egui::Button::new("Attach uploaded reference to next turn"),
                        )
                        .clicked()
                    {
                        attach = Some(path.clone());
                    }
                }
                if ui.button("Load my uploaded files").clicked() {
                    self.job.request(
                        "load_files",
                        api,
                        ApiRequest::get(&["api", "v1", "files"]),
                        runtime,
                        ctx,
                    );
                }
                let files = self.files["files"].as_array().cloned().unwrap_or_default();
                for file in files {
                    ui.horizontal(|ui| {
                        ui.label(file["filename"].as_str().unwrap_or("File"));
                        if ui
                            .add_enabled(can_attach, egui::Button::new("Attach"))
                            .clicked()
                        {
                            attach = file["file_path"].as_str().map(str::to_owned);
                        }
                        if ui.button("Preview").clicked() {
                            if let Some(url) = file["file_path"].as_str() {
                                self.load_remote(
                                    api,
                                    url.into(),
                                    file["filename"].as_str().unwrap_or("file").into(),
                                    runtime,
                                    ctx,
                                );
                            }
                        }
                    });
                }
            });
            self.preview_ui(ui, runtime, ctx);
        });
        attach
    }
    pub fn ui_artifacts(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        session: &str,
        events: &[Value],
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.poll(ctx);
        if self.scope != session && !self.busy && !self.job.busy {
            self.scope = session.into();
            self.artifacts = Value::Null;
            self.preview = None;
            self.texture = None;
        }
        egui::CollapsingHeader::new("Artifact previews and downloads").show(ui, |ui| {
            self.job.status_ui(ui);
            ui.label(&self.status);
            if ui
                .add_enabled(
                    !session.is_empty() && !self.job.busy,
                    egui::Button::new("Refresh authorized session artifacts"),
                )
                .clicked()
            {
                self.job.request(
                    "load_artifacts",
                    api,
                    ApiRequest::get(&["api", "v1", "assistant", "sessions", session, "artifacts"]),
                    runtime,
                    ctx,
                );
            }
            let mut items = events.to_vec();
            if let Some(rows) = self
                .artifacts
                .as_array()
                .or_else(|| self.artifacts["artifacts"].as_array())
            {
                items.extend(rows.clone());
            }
            let mut seen = std::collections::HashSet::new();
            for item in items {
                let Some(id) = item["artifact_id"]
                    .as_str()
                    .or_else(|| item["artifactId"].as_str())
                else {
                    continue;
                };
                if !seen.insert(id.to_owned()) {
                    continue;
                }
                let title = item["title"]
                    .as_str()
                    .or_else(|| item["filename"].as_str())
                    .unwrap_or("Artifact");
                ui.push_id(id, |ui| {
                    ui.strong(title);
                    ui.label(format!(
                        "{} · {} bytes",
                        item["mime_type"], item["size_bytes"]
                    ));
                    if let Some(content) = item["content"]
                        .as_str()
                        .or_else(|| item["preview"].as_str())
                    {
                        egui::CollapsingHeader::new("Document text").show(ui, |ui| {
                            ui.label(content);
                            if ui.button("Copy original document content").clicked() {
                                ctx.copy_text(content.into());
                            }
                        });
                    }
                    let name = item["filename"].as_str().unwrap_or("artifact").to_owned();
                    let url = item["download_url"]
                        .as_str()
                        .or_else(|| item["download_path"].as_str())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("/api/v1/assistant/artifacts/{id}/download"));
                    if ui
                        .add_enabled(
                            !self.busy,
                            egui::Button::new("Load authenticated preview / download"),
                        )
                        .clicked()
                    {
                        self.load_remote(api, url.clone(), name, runtime, ctx);
                    }
                    if let Ok(url) = reqwest::Url::parse(&url) {
                        if ["http", "https"].contains(&url.scheme())
                            && url.username().is_empty()
                            && url.password().is_none()
                        {
                            if ui
                                .button("Open external artifact link in browser")
                                .clicked()
                            {
                                ctx.open_url(egui::OpenUrl::new_tab(url.to_string()));
                            }
                        }
                    }
                });
            }
            self.preview_ui(ui, runtime, ctx);
        });
    }
    fn preview_ui(
        &mut self,
        ui: &mut egui::Ui,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        let Some(data) = &self.preview else {
            return;
        };
        ui.strong(&data.name);
        ui.label(format!("{} · {} bytes", data.mime, data.bytes.len()));
        if let Some(texture) = &self.texture {
            ui.add(egui::Image::new(texture).max_width(720.0).max_height(500.0));
        }
        if let Some(text) = &data.text {
            egui::ScrollArea::vertical()
                .id_salt("attachment-text-preview")
                .max_height(350.0)
                .show(ui, |ui| {
                    ui.label(text);
                });
            if ui.button("Copy preview text").clicked() {
                ctx.copy_text(text.clone());
            }
        }
        if self.texture.is_none() && data.text.is_none() {
            ui.label(
                "This document's binary content can be saved and opened in its native viewer.",
            );
        }
        text_field(
            ui,
            "Save as new local file (absolute path)",
            &mut self.save_path,
        );
        ui.checkbox(
            &mut self.save_confirm,
            "Save these downloaded bytes at this path; existing files are never overwritten",
        );
        if ui
            .add_enabled(
                self.save_confirm && !self.busy && PathBuf::from(&self.save_path).is_absolute(),
                egui::Button::new("Save selected copy"),
            )
            .clicked()
        {
            let path = PathBuf::from(&self.save_path);
            let bytes = data.bytes.clone();
            let tx = self.tx.clone();
            let g = self.generation;
            let ctx = ctx.clone();
            self.busy = true;
            self.save_confirm = false;
            runtime.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let mut file = std::fs::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(&path)
                        .map_err(|_| ApiError::Request)?;
                    file.write_all(&bytes).map_err(|_| ApiError::Request)?;
                    file.sync_all().map_err(|_| ApiError::Request)?;
                    Ok(path)
                })
                .await
                .map_err(|_| ApiError::Request)
                .and_then(|v| v);
                let _ = tx.send(Completion::Saved(g, result));
                ctx.request_repaint();
            });
        }
        if let Some(path) = &self.saved_path {
            if ui.button("Open saved copy in its native viewer").clicked() {
                if let Ok(url) = reqwest::Url::from_file_path(path) {
                    ctx.open_url(egui::OpenUrl::new_tab(url.to_string()));
                }
            }
        }
        if ui.button("Close preview and release bytes").clicked() {
            self.preview = None;
            self.texture = None;
            self.saved_path = None;
        }
    }
}
