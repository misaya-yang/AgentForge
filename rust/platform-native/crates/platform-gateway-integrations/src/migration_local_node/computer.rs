use super::{LocalNodeError as Error, Result, ledger::ActionLedger, models::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ready,
    Denied,
    NeedsAction,
    Unsupported,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DriverHealth {
    pub status: HealthStatus,
    pub platform: String,
    pub driver: String,
    pub accessibility: HealthStatus,
    pub screen_recording: HealthStatus,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComputerObservation {
    pub observation_id: String,
    pub app_id: String,
    pub window_id: String,
    pub origin: Option<String>,
    pub width: u32,
    pub height: u32,
    pub screenshot_ref: Option<String>,
    pub accessibility_digest: Option<String>,
    pub observed_at: f64,
}
impl ComputerObservation {
    pub fn digest(&self) -> Result<String> {
        Ok(digest_payload(&serde_json::to_value(self)?))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComputerAction {
    pub kind: String,
    pub observation_id: String,
    pub arguments: Value,
}
pub trait ComputerBackend: Send + Sync {
    fn name(&self) -> &str;
    fn observe(&self, app: &str, window: Option<&str>) -> Result<ComputerObservation>;
    fn execute(&self, action: &ComputerAction) -> Result<()>;
    fn stop(&self) -> Result<()>;
}
pub trait ComputerDriver: Send + Sync {
    fn doctor(&self) -> DriverHealth;
    fn observe(&self, app: &str, window: Option<&str>) -> Result<ComputerObservation>;
    fn execute(&self, action: &ComputerAction) -> Result<()>;
    fn stop(&self) -> Result<()>;
}
pub struct MacOSComputerDriver {
    pub backend: Option<Arc<dyn ComputerBackend>>,
    pub accessibility_probe: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub screen_recording_probe: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub platform_name: String,
}
impl MacOSComputerDriver {
    fn require_ready(&self) -> Result<&dyn ComputerBackend> {
        let health = self.doctor();
        if health.status != HealthStatus::Ready {
            return Err(Error::new(
                "driver_unavailable",
                health
                    .reason
                    .unwrap_or_else(|| "computer driver unavailable".into()),
            ));
        }
        self.backend
            .as_deref()
            .ok_or_else(|| Error::new("driver_unavailable", "computer backend unavailable"))
    }
}
impl ComputerDriver for MacOSComputerDriver {
    fn doctor(&self) -> DriverHealth {
        if self.platform_name != "Darwin" {
            return DriverHealth {
                status: HealthStatus::Unsupported,
                platform: self.platform_name.clone(),
                driver: "unavailable".into(),
                accessibility: HealthStatus::Unsupported,
                screen_recording: HealthStatus::Unsupported,
                reason: Some("macOS Computer Use driver is only enabled on Darwin".into()),
            };
        }
        let probe = |p: &Option<Arc<dyn Fn() -> bool + Send + Sync>>| match p {
            None => HealthStatus::NeedsAction,
            Some(p) => {
                if p() {
                    HealthStatus::Ready
                } else {
                    HealthStatus::Denied
                }
            }
        };
        let accessibility = probe(&self.accessibility_probe);
        let screen = probe(&self.screen_recording_probe);
        let (status, reason) = if self.backend.is_none() {
            (
                HealthStatus::NeedsAction,
                Some("no trusted CUA/Accessibility backend configured".into()),
            )
        } else if accessibility == HealthStatus::Ready && screen == HealthStatus::Ready {
            (HealthStatus::Ready, None)
        } else {
            (
                if accessibility == HealthStatus::Denied || screen == HealthStatus::Denied {
                    HealthStatus::Denied
                } else {
                    HealthStatus::NeedsAction
                },
                Some("required macOS permissions are not ready".into()),
            )
        };
        DriverHealth {
            status,
            reason,
            platform: self.platform_name.clone(),
            driver: self
                .backend
                .as_ref()
                .map_or("unavailable", |b| b.name())
                .into(),
            accessibility,
            screen_recording: screen,
        }
    }
    fn observe(&self, app: &str, window: Option<&str>) -> Result<ComputerObservation> {
        self.require_ready()?.observe(app, window)
    }
    fn execute(&self, action: &ComputerAction) -> Result<()> {
        self.require_ready()?.execute(action)
    }
    fn stop(&self) -> Result<()> {
        if let Some(b) = &self.backend {
            b.stop()?;
        }
        Ok(())
    }
}
#[derive(Clone, Default)]
pub struct ComputerScope {
    pub allowed_apps: BTreeSet<String>,
    pub allowed_origins: BTreeSet<String>,
}
#[derive(Clone)]
pub struct ComputerLease {
    pub lease_id: String,
    pub session_id: String,
    pub app_id: String,
    pub window_id: Option<String>,
    pub expires_at: f64,
    token: String,
}
impl ComputerLease {
    pub fn token(&self) -> &str {
        &self.token
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComputerActionResult {
    pub action_id: String,
    pub status: ActionStatus,
    pub before_observation_id: String,
    pub after_observation_id: String,
    pub after_digest: String,
}
#[derive(Default)]
struct State {
    lease: Option<ComputerLease>,
    observation: Option<ComputerObservation>,
}
pub struct ComputerController {
    pub driver: Arc<dyn ComputerDriver>,
    pub scope: ComputerScope,
    pub ledger: Arc<ActionLedger>,
    state: Mutex<State>,
    operation: Mutex<()>,
}
impl ComputerController {
    pub fn new(
        driver: Arc<dyn ComputerDriver>,
        scope: ComputerScope,
        ledger: Arc<ActionLedger>,
    ) -> Self {
        Self {
            driver,
            scope,
            ledger,
            state: Mutex::new(State::default()),
            operation: Mutex::new(()),
        }
    }
    pub fn acquire(
        &self,
        app: &str,
        session: &str,
        window: Option<String>,
        ttl: u64,
    ) -> Result<ComputerLease> {
        if self.driver.doctor().status != HealthStatus::Ready {
            return Err(Error::new(
                "driver_unavailable",
                "computer driver or permissions not ready",
            ));
        }
        if !self.scope.allowed_apps.contains(app)
            || !valid_identifier(session, 512)
            || !(1..=3600).contains(&ttl)
        {
            return Err(Error::denied("Computer Use scope or lifetime invalid"));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::denied("computer state unavailable"))?;
        if state.lease.as_ref().is_some_and(|v| now() < v.expires_at) {
            return Err(Error::denied("another Computer Use lease active"));
        }
        let lease = ComputerLease {
            lease_id: format!("lease_{}", uuid::Uuid::new_v4().simple()),
            session_id: session.into(),
            app_id: app.into(),
            window_id: window,
            expires_at: now() + ttl as f64,
            token: super::identity::random_token(32)?,
        };
        state.lease = Some(lease.clone());
        state.observation = None;
        Ok(lease)
    }
    fn require_lease(&self, token: &str) -> Result<ComputerLease> {
        self.state
            .lock()
            .map_err(|_| Error::denied("computer state unavailable"))?
            .lease
            .clone()
            .filter(|l| {
                now() < l.expires_at && constant_time_equal(token.as_bytes(), l.token.as_bytes())
            })
            .ok_or_else(|| Error::denied("Computer Use lease absent, expired or token invalid"))
    }
    pub fn observe(&self, token: &str) -> Result<ComputerObservation> {
        let lease = self.require_lease(token)?;
        let observation = self
            .driver
            .observe(&lease.app_id, lease.window_id.as_deref())?;
        if observation.app_id != lease.app_id {
            self.stop(Some(token))?;
            return Err(Error::stale("focused application changed"));
        }
        if observation.origin.as_ref().is_some_and(|o| {
            !self.scope.allowed_origins.is_empty() && !self.scope.allowed_origins.contains(o)
        }) {
            self.stop(Some(token))?;
            return Err(Error::denied("browser origin left allowed scope"));
        }
        self.require_lease(token)?;
        self.state
            .lock()
            .map_err(|_| Error::denied("computer state unavailable"))?
            .observation = Some(observation.clone());
        Ok(observation)
    }
    pub fn execute(
        &self,
        token: &str,
        computer_action: &ComputerAction,
        action: &ActionContext,
    ) -> Result<ComputerActionResult> {
        let _operation = self
            .operation
            .lock()
            .map_err(|_| Error::denied("computer operation unavailable"))?;
        let lease = self.require_lease(token)?;
        if ![
            "click",
            "double_click",
            "type_text",
            "key_press",
            "scroll",
            "drag",
            "wait",
            "open_app",
            "focus_app",
        ]
        .contains(&computer_action.kind.as_str())
            || action.capability != "app.control"
            || action.operation != "app.control"
            || action.session_id != lease.session_id
        {
            return Err(Error::denied("Computer Use signed action scope mismatch"));
        }
        let before = self
            .state
            .lock()
            .map_err(|_| Error::denied("computer state unavailable"))?
            .observation
            .clone()
            .filter(|o| {
                o.observation_id == computer_action.observation_id && o.app_id == lease.app_id
            })
            .ok_or_else(|| Error::stale("Computer Use action uses stale observation"))?;
        let args = json!({"lease_id":lease.lease_id,"kind":computer_action.kind,"observation_id":computer_action.observation_id,"arguments":computer_action.arguments});
        action.validate_payload(
            &args,
            self.ledger.platform_signature_verifier.as_deref(),
            Some(&lease.lease_id),
            Some(&[
                lease.lease_id.clone(),
                before.app_id.clone(),
                before.window_id.clone(),
            ]),
        )?;
        let begin = self.ledger.begin(action)?;
        if !begin.created {
            return serde_json::from_value(
                begin
                    .record
                    .result
                    .ok_or_else(|| Error::denied("matching Computer Use action running"))?,
            )
            .map_err(Into::into);
        }
        let mut dispatched = false;
        let result: Result<ComputerActionResult> = (|| {
            self.ledger.mark_awaiting_approval(&action.action_id)?;
            action.require_approval(
                &before.digest()?,
                self.ledger.trusted_local_approval_verifier.as_deref(),
            )?;
            self.require_lease(token)?;
            self.ledger.mark_dispatched(&action.action_id)?;
            self.ledger.mark_running(&action.action_id)?;
            dispatched = true;
            self.driver.execute(computer_action)?;
            let after = self.observe(token)?;
            self.ledger.mark_observed(&action.action_id)?;
            let result = ComputerActionResult {
                action_id: action.action_id.clone(),
                status: ActionStatus::Succeeded,
                before_observation_id: before.observation_id,
                after_observation_id: after.observation_id.clone(),
                after_digest: after.digest()?,
            };
            self.ledger.finish(
                &action.action_id,
                ActionStatus::Succeeded,
                serde_json::to_value(&result)?,
            )?;
            Ok(result)
        })();
        if let Err(e) = &result {
            self.ledger.finish(
                &action.action_id,
                if dispatched {
                    ActionStatus::Unknown
                } else {
                    ActionStatus::Failed
                },
                json!({"error_code":e.code,"read_back_required":dispatched,"replay_allowed":false}),
            )?;
        }
        result
    }
    pub fn stop(&self, token: Option<&str>) -> Result<bool> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::denied("computer state unavailable"))?;
        let Some(lease) = state.lease.as_ref() else {
            return Ok(false);
        };
        if token.is_some_and(|t| !constant_time_equal(t.as_bytes(), lease.token.as_bytes())) {
            return Err(Error::denied("Computer Use lease token invalid"));
        }
        state.lease = None;
        state.observation = None;
        drop(state);
        self.driver.stop()?;
        Ok(true)
    }
    pub fn emergency_stop(&self) -> Result<bool> {
        self.stop(None)
    }
}
