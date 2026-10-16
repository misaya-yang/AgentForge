//! Native macOS AX/CGEvent helper adapter. No Python, shell, or ambient permissions.
use crate::{
    Error, Result, canonical,
    computer::{ComputerAction, ComputerDriver, Observation},
    digest,
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
pub struct NativeHelper {
    path: PathBuf,
    device: u64,
    inode: u64,
    pub timeout: Duration,
}
impl NativeHelper {
    pub fn new(path: PathBuf) -> Result<Self> {
        let m = std::fs::symlink_metadata(&path)?;
        if !path.is_absolute()
            || !m.is_file()
            || m.file_type().is_symlink()
            || m.nlink() != 1
            || m.mode() & 0o022 != 0
            || m.mode() & 0o111 == 0
        {
            return Err(Error::Denied("native helper is not trusted executable"));
        }
        Ok(Self {
            path,
            device: m.dev(),
            inode: m.ino(),
            timeout: Duration::from_secs(15),
        })
    }
    pub fn invoke(&self, request: &Value) -> Result<Value> {
        self.invoke_cancellable(request, &|| false)
    }
    pub fn invoke_cancellable(
        &self,
        request: &Value,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Value> {
        if !cfg!(target_os = "macos") {
            return Err(Error::Denied("macOS helper unsupported on this platform"));
        }
        if cancelled() {
            return Err(Error::Denied("native helper cancelled"));
        }
        let m = std::fs::symlink_metadata(&self.path)?;
        if m.dev() != self.device || m.ino() != self.inode || m.mode() & 0o022 != 0 {
            return Err(Error::Stale);
        }
        let bytes = canonical(request).into_bytes();
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(Error::Denied("native helper request budget"));
        }
        let mut child = Command::new(&self.path)
            .env_clear()
            .envs([
                ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
                ("LANG", "C"),
                ("LC_ALL", "C"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or(Error::Storage)?;
        let writer = std::thread::spawn(move || stdin.write_all(&bytes));
        let mut stdout = child.stdout.take().ok_or(Error::Storage)?;
        let reader = std::thread::spawn(move || {
            let mut kept = Vec::new();
            let mut buf = [0u8; 65536];
            let mut overflow = false;
            loop {
                let n = stdout.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                if kept.len() + n > 1024 * 1024 {
                    overflow = true;
                } else if !overflow {
                    kept.extend_from_slice(&buf[..n]);
                }
            }
            Ok::<_, std::io::Error>((kept, overflow))
        });
        let start = Instant::now();
        let status = loop {
            if cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                let _ = writer.join();
                let _ = reader.join();
                return Err(Error::Denied("native helper cancelled"));
            }
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if start.elapsed() >= self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                let _ = writer.join();
                let _ = reader.join();
                return Err(Error::Denied("native helper timeout"));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        writer.join().map_err(|_| Error::Storage)??;
        let (bytes, overflow) = reader.join().map_err(|_| Error::Storage)??;
        if !status.success() || overflow {
            return Err(Error::Denied("native helper failed"));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
        if value.get("ok") != Some(&Value::Bool(true)) {
            return Err(Error::Denied("native permission or takeover required"));
        }
        Ok(value)
    }
    pub fn prompt_and_sign(
        &self,
        payload: &[u8],
        summary: &str,
        service: &str,
        account: &str,
        timeout: Duration,
    ) -> Result<Option<String>> {
        self.prompt_and_sign_cancellable(payload, summary, service, account, timeout, &|| false)
    }
    pub fn prompt_and_sign_cancellable(
        &self,
        payload: &[u8],
        summary: &str,
        service: &str,
        account: &str,
        timeout: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<String>> {
        use base64::Engine;
        if payload.is_empty()
            || payload.len() > 1024 * 1024
            || summary.len() > 10000
            || !nonsecret(summary)
            || timeout.is_zero()
            || timeout > Duration::from_secs(120)
        {
            return Err(Error::Denied("approval prompt payload"));
        }
        let helper = NativeHelper {
            path: self.path.clone(),
            device: self.device,
            inode: self.inode,
            timeout: timeout + Duration::from_secs(5),
        };
        let response=helper.invoke_cancellable(&json!({"command":"approval_sign","title":"AI--Platfform Local Node approval","summary":summary,"timeout_seconds":timeout.as_secs_f64(),"keychain_service":service,"keychain_account":account,"approval_payload_base64":base64::engine::general_purpose::STANDARD.encode(payload)}),cancelled)?;
        if response["approved"] != true || response["timed_out"] == true {
            return Ok(None);
        }
        let signature = response["signature"]
            .as_str()
            .filter(|s| s.len() == 64)
            .ok_or(Error::Protocol)?;
        Ok(Some(signature.into()))
    }
    pub fn verify_approval(
        &self,
        payload: &[u8],
        signature: &str,
        service: &str,
        account: &str,
    ) -> bool {
        use base64::Engine;
        if payload.is_empty() || payload.len() > 1024 * 1024 || signature.len() != 64 {
            return false;
        }
        self.invoke(&json!({"command":"approval_verify","keychain_service":service,"keychain_account":account,"approval_payload_base64":base64::engine::general_purpose::STANDARD.encode(payload),"signature":signature})).is_ok_and(|v|v["verified"]==true)
    }
}
impl crate::file_dispatch::LocalApprovalPrompt for NativeHelper {
    fn sign(
        &self,
        payload: &[u8],
        summary: &str,
        device_id: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<String>> {
        self.prompt_and_sign_cancellable(
            payload,
            summary,
            "ai-platform.local-node.trusted-approval.v1",
            device_id,
            Duration::from_secs(120),
            cancelled,
        )
    }
}
pub struct MacComputer {
    pub helper: NativeHelper,
    pub allowed_apps: HashSet<String>,
    pub allowed_origins: HashSet<String>,
    last: Option<Observation>,
    details: Option<Value>,
    screenshot_dir: Option<PathBuf>,
    screen_share: bool,
}
fn origin(raw: &str) -> Result<String> {
    let u = url::Url::parse(raw).map_err(|_| Error::Denied("browser origin"))?;
    if !["http", "https"].contains(&u.scheme())
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
    {
        return Err(Error::Denied("browser origin"));
    }
    Ok(u.origin().ascii_serialization())
}
fn nonsecret(s: &str) -> bool {
    ![
        "-----begin private key-----",
        "bearer ",
        "sk-",
        "ghp_",
        "secret_canary",
        "secret-canary",
    ]
    .iter()
    .any(|m| s.to_lowercase().contains(m))
}
impl MacComputer {
    pub fn new(
        helper: NativeHelper,
        apps: HashSet<String>,
        origins: HashSet<String>,
    ) -> Result<Self> {
        if apps.is_empty() {
            return Err(Error::Denied("application scope required"));
        }
        let origins = origins
            .iter()
            .map(|s| origin(s))
            .collect::<Result<HashSet<_>>>()?;
        Ok(Self {
            helper,
            allowed_apps: apps,
            allowed_origins: origins,
            last: None,
            details: None,
            screenshot_dir: None,
            screen_share: false,
        })
    }
}
impl MacComputer {
    pub fn last_observation(&self) -> Option<&Observation> {
        self.last.as_ref()
    }
    pub fn last_details(&self) -> Option<&Value> {
        self.details.as_ref()
    }
    pub fn with_screen_capture(mut self, path: PathBuf, share: bool) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if !path.is_absolute() || path.is_symlink() {
            return Err(Error::Denied("screenshot directory"));
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)?;
        let m = std::fs::symlink_metadata(&path)?;
        if !m.is_dir() || m.mode() & 0o022 != 0 {
            return Err(Error::Denied("screenshot permissions"));
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        self.screenshot_dir = Some(path.canonicalize()?);
        self.screen_share = share;
        Ok(self)
    }
    fn screenshot(&self, window: &str) -> Result<Option<PathBuf>> {
        let Some(dir) = &self.screenshot_dir else {
            return Ok(None);
        };
        let path = dir.join(format!("screen-{}.png", uuid::Uuid::new_v4().simple()));
        let mut child = Command::new("/usr/sbin/screencapture")
            .args(["-x", "-o", "-l", window])
            .arg(&path)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("LANG", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let start = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if start.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Denied("screen capture timeout"));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if !status.success() {
            return Err(Error::Denied("screen capture failed"));
        }
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        let m = file.metadata()?;
        if !m.is_file() || m.nlink() != 1 || m.len() > 64 * 1024 * 1024 {
            return Err(Error::Denied("screen artifact"));
        }
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        if &header != b"\x89PNG\r\n\x1a\n" {
            return Err(Error::Denied("screen artifact format"));
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Some(path))
    }
}
impl ComputerDriver for MacComputer {
    fn ready(&mut self) -> bool {
        self.helper
            .invoke(&json!({"command":"doctor"}))
            .is_ok_and(|v| v["accessibility"] == true && v["screen_recording"] == true)
    }
    fn observe(&mut self, app: &str, window: Option<&str>) -> Result<Observation> {
        if !self.allowed_apps.contains(app) {
            return Err(Error::Denied("application scope"));
        }
        let mut req = json!({"command":"observe","app_id":app});
        if let Some(w) = window {
            req["window_id"] = json!(w)
        }
        let value = self.helper.invoke(&req)?;
        if value["app_id"] != app {
            return Err(Error::Stale);
        }
        let window = value["window_id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .ok_or(Error::Protocol)?;
        let width = value["width"]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0. && *n <= u32::MAX as f64)
            .ok_or(Error::Protocol)?;
        let height = value["height"]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0. && *n <= u32::MAX as f64)
            .ok_or(Error::Protocol)?;
        let origin = value["origin"].as_str().map(origin).transpose()?;
        if origin
            .as_ref()
            .is_some_and(|s| !self.allowed_origins.contains(s))
            || !self.allowed_origins.is_empty() && origin.is_none()
        {
            return Err(Error::Denied("browser origin scope"));
        }
        let flags = value["risk_flags"].as_array().ok_or(Error::Protocol)?;
        if !flags.is_empty() {
            return Err(Error::Denied("sensitive UI requires takeover"));
        }
        let lines = value["accessibility_lines"]
            .as_array()
            .ok_or(Error::Protocol)?;
        if lines.len() > 800
            || lines.iter().any(|v| {
                v.as_str()
                    .is_none_or(|s| s.chars().count() > 2000 || !nonsecret(s))
            })
        {
            return Err(Error::Denied("accessibility safety"));
        }
        let screenshot = self.screenshot(window)?;
        let o = Observation {
            observation_id: format!("obs_{}", uuid::Uuid::new_v4().simple()),
            app_id: app.into(),
            window_id: window.into(),
            origin,
            width: width as u32,
            height: height as u32,
            screenshot_ref: if self.screen_share {
                screenshot.map(|p| p.to_string_lossy().into_owned())
            } else {
                None
            },
            accessibility_digest: Some(digest(&json!(lines))),
            observed_at: crate::now(),
        };
        self.last = Some(o.clone());
        self.details = Some(value);
        Ok(o)
    }
    fn execute(&mut self, input: &ComputerAction) -> Result<()> {
        let o = self.last.as_ref().ok_or(Error::Stale)?;
        if o.observation_id != input.observation_id {
            return Err(Error::Stale);
        }
        let args = input.arguments.as_object().ok_or(Error::Protocol)?;
        let exact = |names: &[&str]| {
            args.len() == names.len() && names.iter().all(|s| args.contains_key(*s))
        };
        let valid = match input.kind.as_str() {
            "click" | "double_click" => exact(&["x", "y"]),
            "drag" => exact(&["from_x", "from_y", "to_x", "to_y"]),
            "key_press" => exact(&["key"]) && args["key"].is_string(),
            "wait" => exact(&["duration_ms"]),
            "focus_app" => args.is_empty(),
            "type_text" => {
                args.keys()
                    .all(|k| ["text", "delay_ms"].contains(&k.as_str()))
                    && args
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.len() <= 10000 && nonsecret(s))
            }
            "scroll" => {
                args.contains_key("scroll_y")
                    && args
                        .keys()
                        .all(|k| ["x", "y", "scroll_y"].contains(&k.as_str()))
            }
            "open_app" => {
                args.keys().all(|k| k == "url")
                    && args.get("url").is_none_or(|v| {
                        v.as_str()
                            .and_then(|s| origin(s).ok())
                            .is_some_and(|s| self.allowed_origins.contains(&s))
                    })
            }
            _ => false,
        };
        if !valid {
            return Err(Error::Denied("native input arguments"));
        }
        self.helper.invoke(&json!({"command":"action","app_id":o.app_id,"window_id":o.window_id,"expected_origin":o.origin,"kind":input.kind,"arguments":input.arguments}))?;
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        self.last = None;
        self.helper.invoke(&json!({"command":"stop"})).map(|_| ())
    }
}
