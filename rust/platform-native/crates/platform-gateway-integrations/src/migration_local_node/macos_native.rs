//! Native helper protocol port. No OS call occurs until an explicit method call.
//! The existing signed Swift helper remains the OS implementation and is not copied.
use super::{
    LocalNodeError as Error, Result,
    computer::*,
    database,
    models::*,
    processes::{signal_group, terminate},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
fn denied(message: &str) -> Error {
    Error::new("native_computer_use_denied", message)
}
fn takeover() -> Error {
    Error::new(
        "takeover_required",
        "password, MFA, permission, or security dialog requires user takeover",
    )
}
pub fn assert_nonsecret(value: &str) -> Result<()> {
    let lowered = value.to_lowercase();
    if [
        "-----begin private key-----",
        "bearer ",
        "sk-",
        "ghp_",
        "secret_canary",
        "secret-canary",
    ]
    .iter()
    .any(|p| lowered.contains(p))
    {
        return Err(denied("secret material cannot enter native Computer Use"));
    }
    Ok(())
}
pub fn canonical_origin(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value).map_err(|_| denied("browser origin invalid"))?;
    if url.scheme() == "file" {
        if url
            .host_str()
            .is_some_and(|h| !h.is_empty() && h != "localhost")
        {
            return Err(denied("file URL host outside local scope"));
        }
        return Ok("file://".into());
    }
    if !["http", "https"].contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(denied("browser origin unsupported or contains credentials"));
    }
    Ok(url.origin().ascii_serialization())
}
pub fn validate_helper(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(Error::new(
            "driver_unavailable",
            "native helper must be absolute",
        ));
    }
    let info = fs::symlink_metadata(path)?;
    if !info.is_file() || info.file_type().is_symlink() {
        return Err(Error::new(
            "driver_unavailable",
            "native helper must be a regular non-symlink executable",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if info.nlink() != 1 || info.mode() & 0o022 != 0 || info.mode() & 0o111 == 0 {
            return Err(Error::new(
                "driver_unavailable",
                "native helper is not a trusted executable",
            ));
        }
    }
    path.canonicalize().map_err(Into::into)
}
pub trait NativeHelper: Send + Sync {
    fn invoke(&self, request: Value, timeout: Duration) -> Result<Value>;
    fn stop_active(&self) -> Result<()>;
}
pub struct NativeHelperProcess {
    path: PathBuf,
    active: Mutex<Option<u32>>,
    serial: Mutex<()>,
}
impl NativeHelperProcess {
    pub fn new(path: &Path) -> Result<Self> {
        Ok(Self {
            path: validate_helper(path)?,
            active: Mutex::new(None),
            serial: Mutex::new(()),
        })
    }
}
impl NativeHelper for NativeHelperProcess {
    fn invoke(&self, request: Value, timeout: Duration) -> Result<Value> {
        if !cfg!(target_os = "macos") {
            return Err(Error::new(
                "driver_unavailable",
                "native macOS helper requires Darwin",
            ));
        }
        let _serial = self
            .serial
            .try_lock()
            .map_err(|_| denied("another native Computer Use operation active"))?;
        let encoded = canonical_json(&request).into_bytes();
        if encoded.len() > 2 * 1024 * 1024 {
            return Err(denied("native helper request exceeds budget"));
        }
        let mut command = Command::new(validate_helper(&self.path)?);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear()
            .envs([
                ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
                ("LANG", "C"),
                ("LC_ALL", "C"),
            ]);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }
        let mut child = command.spawn()?;
        *self
            .active
            .lock()
            .map_err(|_| denied("native process state unavailable"))? = Some(child.id());
        let result = (|| {
            let mut input = child
                .stdin
                .take()
                .ok_or_else(|| denied("native input unavailable"))?;
            let writer = std::thread::spawn(move || input.write_all(&encoded));
            let mut output = child
                .stdout
                .take()
                .ok_or_else(|| denied("native output unavailable"))?;
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                let fd = output.as_raw_fd();
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                if flags < 0
                    || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
                {
                    return Err(denied("native output setup failed"));
                }
            }
            let deadline = Instant::now() + timeout;
            let mut bytes = Vec::new();
            let mut eof = false;
            while !eof || child.try_wait()?.is_none() {
                if Instant::now() >= deadline {
                    terminate(&mut child)?;
                    let _ = writer.join();
                    return Err(denied("native Computer Use operation timed out"));
                }
                let mut buffer = [0u8; 16384];
                match output.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        if bytes.len() + n > 1024 * 1024 {
                            terminate(&mut child)?;
                            let _ = writer.join();
                            return Err(denied("native helper response exceeds one MiB"));
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
                    Err(e) => return Err(e.into()),
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            child.wait()?;
            writer
                .join()
                .map_err(|_| denied("native input write failed"))??;
            let value: Value = serde_json::from_slice(&bytes)?;
            if !value.is_object() {
                return Err(denied("native helper response must be object"));
            }
            if value["ok"] != true {
                return Err(
                    match value["code"].as_str().unwrap_or("native_backend_failed") {
                        "stale_window" | "stale_origin" => {
                            Error::stale("native Computer Use target changed")
                        }
                        "takeover_required" => takeover(),
                        "accessibility_denied" | "screen_recording_denied" => Error::new(
                            "driver_unavailable",
                            "required macOS permission unavailable",
                        ),
                        _ => denied("native helper operation failed"),
                    },
                );
            }
            Ok(value)
        })();
        if result.is_err() {
            let _ = terminate(&mut child);
        }
        *self
            .active
            .lock()
            .map_err(|_| denied("native process state unavailable"))? = None;
        result
    }
    fn stop_active(&self) -> Result<()> {
        if let Some(pid) = *self
            .active
            .lock()
            .map_err(|_| denied("native process state unavailable"))?
        {
            signal_group(pid, true)?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeObservationDetails {
    pub observation_id: String,
    pub session_id: String,
    pub app_id: String,
    pub window_id: String,
    pub window_title: String,
    pub origin: Option<String>,
    pub frontmost: bool,
    pub window_x: f64,
    pub window_y: f64,
    pub accessibility_lines: Vec<String>,
    pub risk_flags: Vec<String>,
    pub screenshot_path: Option<PathBuf>,
    pub screenshot_sha256: Option<String>,
}
#[derive(Clone, Debug)]
pub struct NativeApprovalIntent {
    pub action_id: String,
    pub run_id: String,
    pub device_id: String,
    pub app_id: String,
    pub window_id: String,
    pub origin: Option<String>,
    pub operation: String,
    pub arguments_digest: String,
    pub target_snapshot_digest: String,
    pub risk_reason: String,
}
impl NativeApprovalIntent {
    pub fn render_summary(&self) -> Result<String> {
        let origin = self
            .origin
            .as_deref()
            .unwrap_or("local desktop application");
        if [
            &self.action_id,
            &self.run_id,
            &self.device_id,
            &self.app_id,
            &self.window_id,
            origin,
            &self.operation,
            &self.arguments_digest,
            &self.target_snapshot_digest,
            &self.risk_reason,
        ]
        .iter()
        .any(|v| v.is_empty() || v.chars().count() > 1024)
        {
            return Err(denied("local approval intent invalid"));
        }
        Ok(format!(
            "Run: {}\nDevice: {}\nApplication: {}\nWindow: {}\nOrigin: {}\nOperation: {}\nArguments SHA-256: {}\nTarget SHA-256: {}\nRisk: {}\n\nApprove this exact action once, or take over and deny.",
            self.run_id,
            self.device_id,
            self.app_id,
            self.window_id,
            origin,
            self.operation,
            self.arguments_digest,
            self.target_snapshot_digest,
            self.risk_reason
        ))
    }
}
pub struct MacOSKeychainApprovalSigner {
    pub device_id: String,
    pub helper: Arc<dyn NativeHelper>,
    pub service: String,
    pub account: String,
}
impl MacOSKeychainApprovalSigner {
    pub fn new(device_id: String, helper: Arc<dyn NativeHelper>) -> Result<Self> {
        if !valid_identifier(&device_id, 255) {
            return Err(denied("Keychain approval device id invalid"));
        }
        Ok(Self {
            account: device_id.clone(),
            device_id,
            helper,
            service: "ai-platform.local-node.trusted-approval.v1".into(),
        })
    }
    pub fn prompt_and_sign(
        &self,
        payload: &[u8],
        intent: &NativeApprovalIntent,
        timeout: f64,
    ) -> Result<Option<String>> {
        if payload.is_empty()
            || payload.len() > 1024 * 1024
            || !timeout.is_finite()
            || !(1.0..=300.0).contains(&timeout)
        {
            return Err(denied("local approval payload or timeout invalid"));
        }
        let summary = intent.render_summary()?;
        assert_nonsecret(&summary)?;
        let response=self.helper.invoke(json!({"command":"approval_sign","title":"AI--Platfform Local Node approval","summary":summary,"timeout_seconds":timeout,"keychain_service":self.service,"keychain_account":self.account,"approval_payload_base64":STANDARD.encode(payload)}),Duration::from_secs_f64(timeout+5.0))?;
        if response["approved"] != true || response["timed_out"] == true {
            return Ok(None);
        }
        let signature = response["signature"]
            .as_str()
            .filter(|s| s.len() == 64)
            .ok_or_else(|| denied("native approval signature invalid"))?;
        Ok(Some(signature.into()))
    }
    pub fn verify(&self, payload: &[u8], signature: &str) -> bool {
        if payload.is_empty() || payload.len() > 1024 * 1024 || signature.len() != 64 {
            return false;
        }
        self.helper.invoke(json!({"command":"approval_verify","keychain_service":self.service,"keychain_account":self.account,"approval_payload_base64":STANDARD.encode(payload),"signature":signature}),Duration::from_secs(10)).is_ok_and(|v|v["verified"]==true)
    }
}
pub struct MacOSNativeComputerBackend {
    pub helper: Arc<dyn NativeHelper>,
    pub session_id: String,
    pub allowed_apps: BTreeSet<String>,
    pub allowed_origins: BTreeSet<String>,
    pub screenshot_dir: Option<PathBuf>,
    pub screen_observe: bool,
    pub screen_share: bool,
    pub helper_timeout_seconds: f64,
    last: Mutex<Option<NativeObservationDetails>>,
    stopped: AtomicBool,
}
impl MacOSNativeComputerBackend {
    pub fn new(
        helper: Arc<dyn NativeHelper>,
        session_id: String,
        allowed_apps: BTreeSet<String>,
        allowed_origins: BTreeSet<String>,
        screenshot_dir: Option<PathBuf>,
        screen_observe: bool,
        screen_share: bool,
    ) -> Result<Self> {
        if !valid_identifier(&session_id, 512)
            || allowed_apps.is_empty()
            || allowed_apps.iter().any(|s| {
                s.len() < 2
                    || s.len() > 255
                    || !s.as_bytes()[0].is_ascii_alphanumeric()
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
            })
            || screen_share && !screen_observe
            || screen_observe && screenshot_dir.is_none()
        {
            return Err(denied("native Computer Use scope invalid"));
        }
        let origins = allowed_origins
            .iter()
            .map(|o| canonical_origin(o))
            .collect::<Result<BTreeSet<_>>>()?;
        if let Some(path) = &screenshot_dir {
            if !path.is_absolute()
                || fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
            {
                return Err(denied("screenshot directory must be absolute and local"));
            }
            database::private_directory(path)?;
        }
        Ok(Self {
            helper,
            session_id,
            allowed_apps,
            allowed_origins: origins,
            screenshot_dir,
            screen_observe,
            screen_share,
            helper_timeout_seconds: 15.0,
            last: Mutex::new(None),
            stopped: AtomicBool::new(false),
        })
    }
    pub fn last_details(&self) -> Result<Option<NativeObservationDetails>> {
        Ok(self
            .last
            .lock()
            .map_err(|_| denied("native observation unavailable"))?
            .clone())
    }
    pub fn permission_health(&self) -> Result<(bool, bool)> {
        let v = self.helper.invoke(
            json!({"command":"doctor"}),
            Duration::from_secs_f64(self.helper_timeout_seconds),
        )?;
        Ok((v["accessibility"] == true, v["screen_recording"] == true))
    }
    fn screenshot(&self, window: &str) -> Result<(PathBuf, String)> {
        if !cfg!(target_os = "macos")
            || !self.screen_observe
            || window.len() > 20
            || !window.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(denied("screen observation outside native scope"));
        }
        let path = self
            .screenshot_dir
            .as_ref()
            .ok_or_else(|| denied("screenshot directory missing"))?
            .join(format!("screen-{}.png", super::identity::random_token(18)?));
        let mut child = Command::new("/usr/sbin/screencapture")
            .args(["-x", "-o", "-l", window])
            .arg(&path)
            .env_clear()
            .envs([
                ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
                ("LANG", "C"),
                ("LC_ALL", "C"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::new(
                    "driver_unavailable",
                    "native screen capture timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if !status.success() {
            return Err(Error::new(
                "driver_unavailable",
                "native screen capture failed",
            ));
        }
        let bytes = fs::read(&path)?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            let _ = fs::remove_file(&path);
            return Err(Error::new(
                "driver_unavailable",
                "native screenshot invalid",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        Ok((path, sha256(&bytes)))
    }
    fn arguments(
        &self,
        action: &ComputerAction,
        details: &NativeObservationDetails,
    ) -> Result<Value> {
        let args = action
            .arguments
            .as_object()
            .ok_or_else(|| denied("native action arguments invalid"))?;
        let keys: BTreeSet<_> = args.keys().map(String::as_str).collect();
        let exact = |expected: &[&str]| keys == expected.iter().copied().collect();
        let subset = |expected: &[&str]| keys.iter().all(|k| expected.contains(k));
        let valid = match action.kind.as_str() {
            "click" | "double_click" => exact(&["x", "y"]),
            "type_text" => {
                subset(&["text", "delay_ms"])
                    && args
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.len() <= 10000 && assert_nonsecret(s).is_ok())
            }
            "key_press" => exact(&["key"]) && args["key"].is_string(),
            "scroll" => subset(&["x", "y", "scroll_y"]) && keys.contains("scroll_y"),
            "drag" => exact(&["from_x", "from_y", "to_x", "to_y"]),
            "wait" => exact(&["duration_ms"]),
            "focus_app" => keys.is_empty(),
            "open_app" => {
                subset(&["url"])
                    && args.get("url").is_none_or(|v| {
                        v.as_str().is_some_and(|s| {
                            canonical_origin(s).is_ok_and(|o| self.allowed_origins.contains(&o))
                        })
                    })
            }
            _ => false,
        };
        if !valid {
            return Err(denied("native action argument shape invalid"));
        }
        if !details.risk_flags.is_empty() {
            return Err(takeover());
        }
        Ok(action.arguments.clone())
    }
}
impl ComputerBackend for MacOSNativeComputerBackend {
    fn name(&self) -> &str {
        "macos-native-ax-cgevent-v1"
    }
    fn observe(&self, app: &str, window: Option<&str>) -> Result<ComputerObservation> {
        if !self.allowed_apps.contains(app) {
            return Err(denied("application outside injected native scope"));
        }
        let mut request = json!({"command":"observe","app_id":app});
        if let Some(window) = window {
            request["window_id"] = json!(window);
        }
        let response = self.helper.invoke(
            request,
            Duration::from_secs_f64(self.helper_timeout_seconds),
        )?;
        if response["app_id"] != app {
            return Err(Error::stale("native helper observed another application"));
        }
        let observed_window = response["window_id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| denied("native window id invalid"))?
            .to_string();
        let number = |key: &str| {
            response[key]
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or_else(|| denied("native geometry invalid"))
        };
        let (x, y, width, height) = (
            number("x")?,
            number("y")?,
            number("width")?,
            number("height")?,
        );
        if width <= 0.0 || height <= 0.0 || width > u32::MAX as f64 || height > u32::MAX as f64 {
            return Err(denied("native geometry invalid"));
        }
        let origin = response["origin"]
            .as_str()
            .map(canonical_origin)
            .transpose()?;
        if origin
            .as_ref()
            .is_some_and(|o| !self.allowed_origins.contains(o))
            || (!self.allowed_origins.is_empty() && origin.is_none())
        {
            return Err(denied("browser origin outside injected scope or unproven"));
        }
        let strings = |key: &str, max_items: usize, max_len: usize| -> Result<Vec<String>> {
            let values = response[key]
                .as_array()
                .filter(|v| v.len() <= max_items)
                .ok_or_else(|| denied("native accessibility response invalid"))?;
            values
                .iter()
                .map(|v| {
                    v.as_str()
                        .filter(|s| s.chars().count() <= max_len)
                        .map(str::to_owned)
                        .ok_or_else(|| denied("native accessibility response invalid"))
                })
                .collect()
        };
        let lines = strings("accessibility_lines", 800, 2000)?;
        let flags = strings("risk_flags", 1000, 100)?;
        if !flags.is_empty() {
            return Err(takeover());
        }
        assert_nonsecret(&lines.join("\n"))?;
        let digest = digest_payload(&json!(lines));
        let screenshot = if self.screen_observe {
            Some(self.screenshot(&observed_window)?)
        } else {
            None
        };
        let stamp = now();
        let id = format!(
            "obs_{}",
            &digest_payload(
                &json!({"session_id":self.session_id,"app_id":app,"window_id":observed_window,"origin":origin,"width":width as u32,"height":height as u32,"accessibility_digest":digest,"screenshot_sha256":screenshot.as_ref().map(|v|&v.1),"observed_at_ns":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0u64,|v|v.as_nanos() as u64)})
            )[..32]
        );
        let details = NativeObservationDetails {
            observation_id: id.clone(),
            session_id: self.session_id.clone(),
            app_id: app.into(),
            window_id: observed_window.clone(),
            window_title: response["window_title"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(500)
                .collect(),
            origin: origin.clone(),
            frontmost: response["frontmost"] == true,
            window_x: x,
            window_y: y,
            accessibility_lines: lines,
            risk_flags: flags,
            screenshot_path: screenshot.as_ref().map(|s| s.0.clone()),
            screenshot_sha256: screenshot.as_ref().map(|s| s.1.clone()),
        };
        *self
            .last
            .lock()
            .map_err(|_| denied("native observation unavailable"))? = Some(details);
        self.stopped.store(false, Ordering::Release);
        Ok(ComputerObservation {
            observation_id: id,
            app_id: app.into(),
            window_id: observed_window,
            origin,
            width: width as u32,
            height: height as u32,
            screenshot_ref: if self.screen_share {
                screenshot.map(|s| s.0.to_string_lossy().into_owned())
            } else {
                None
            },
            accessibility_digest: Some(digest),
            observed_at: stamp,
        })
    }
    fn execute(&self, action: &ComputerAction) -> Result<()> {
        let details = self
            .last_details()?
            .filter(|d| {
                !self.stopped.load(Ordering::Acquire) && d.observation_id == action.observation_id
            })
            .ok_or_else(|| Error::stale("native action stopped or based on stale observation"))?;
        let args = self.arguments(action, &details)?;
        let duration = args["duration_ms"].as_f64().unwrap_or(0.0);
        if !duration.is_finite() || duration < 0.0 {
            return Err(denied("native wait duration invalid"));
        }
        self.helper.invoke(json!({"command":"action","app_id":details.app_id,"window_id":details.window_id,"expected_origin":details.origin,"kind":action.kind,"arguments":args}),Duration::from_secs_f64((self.helper_timeout_seconds+duration/1000.0).min(60.0)))?;
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        self.stopped.store(true, Ordering::Release);
        *self
            .last
            .lock()
            .map_err(|_| denied("native observation unavailable"))? = None;
        self.helper.stop_active()?;
        let _ = self
            .helper
            .invoke(json!({"command":"stop"}), Duration::from_secs(2));
        Ok(())
    }
}
