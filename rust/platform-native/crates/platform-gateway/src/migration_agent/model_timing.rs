//! Source additive, process-monotonic model-plane timing. Internal evidence only.
use std::time::Instant;
pub const TIMING_SCHEMA_VERSION: &str = "ppr-timing/v1";
pub const IDENTITY_TOLERANCE_SECONDS: f64 = 1e-9;
pub const REAL_CLOCK_IDENTITY_TOLERANCE_SECONDS: f64 = 0.005;
pub const CLIENT_LOWER_BOUND_SECONDS: f64 = 0.010;
pub const CLIENT_RESIDUAL_ABS_SECONDS: f64 = 0.200;
pub const CLIENT_RESIDUAL_RELATIVE: f64 = 0.05;
#[derive(Default, Debug)]
pub struct ModelPlaneTiming {
    pub stream_started: Option<f64>,
    pub dispatched: Option<f64>,
    pub first_frame: Option<f64>,
    pub first_visible: Option<f64>,
}
impl ModelPlaneTiming {
    pub fn start(now: f64) -> Self {
        Self {
            stream_started: Some(now),
            ..Self::default()
        }
    }
    pub fn note_dispatch(&mut self, now: f64) {
        self.dispatched.get_or_insert(now);
    }
    pub fn note_first_frame(&mut self, now: f64) {
        self.first_frame.get_or_insert(now);
    }
    pub fn note_first_visible(&mut self, now: f64) {
        self.first_visible.get_or_insert(now);
    }
    pub fn components(&self) -> [(&'static str, Option<f64>); 5] {
        let delta = |a: Option<f64>, b: Option<f64>| a.zip(b).map(|(a, b)| b - a);
        let pre = delta(self.stream_started, self.dispatched);
        let projection = delta(self.first_frame, self.first_visible);
        [
            ("local_pre_provider_seconds", pre),
            (
                "provider_wait_seconds",
                delta(self.dispatched, self.first_frame),
            ),
            ("local_projection_seconds", projection),
            (
                "local_overhead_seconds",
                pre.zip(projection).map(|(a, b)| a + b),
            ),
            (
                "model_plane_ttft_seconds",
                delta(self.stream_started, self.first_visible),
            ),
        ]
    }
    pub fn identity_residual(&self) -> Option<f64> {
        let c = self.components();
        Some((c[0].1? + c[1].1? + c[2].1? - c[4].1?).abs())
    }
}
pub fn client_residual_within_tolerance(server: Option<f64>, client: Option<f64>) -> bool {
    let Some((server, client)) = server.zip(client) else {
        return false;
    };
    let residual = client - server;
    residual >= -CLIENT_LOWER_BOUND_SECONDS
        && residual <= CLIENT_RESIDUAL_ABS_SECONDS.max(CLIENT_RESIDUAL_RELATIVE * client)
}
pub(crate) struct TimingEvidence {
    started: Instant,
    pub timing: ModelPlaneTiming,
    pub wire: &'static str,
    run: String,
    call: String,
    model: String,
}
impl TimingEvidence {
    pub fn new(call: &super::model::AuthorizedCall) -> Self {
        Self {
            started: Instant::now(),
            timing: ModelPlaneTiming::start(0.0),
            wire: "chat_completions",
            run: call.run_id.clone(),
            call: call.call_id.clone(),
            model: sanitized_model(&call.model_id),
        }
    }
    pub fn dispatch(&mut self) {
        self.timing
            .note_dispatch(self.started.elapsed().as_secs_f64());
    }
    pub fn frame(&mut self) {
        self.timing
            .note_first_frame(self.started.elapsed().as_secs_f64());
    }
    pub fn projected(&mut self, frame: &[u8]) {
        let first = frame.split(|b| *b == b'\n').next().unwrap_or_default();
        if [
            b"event: response.output_text.delta".as_slice(),
            b"event: response.reasoning_text.delta",
            b"event: response.reasoning_summary_text.delta",
            b"event: response.refusal.delta",
        ]
        .contains(&first)
        {
            self.timing
                .note_first_visible(self.started.elapsed().as_secs_f64());
        }
    }
}
fn sanitized_model(model: &str) -> String {
    let mut out = String::new();
    let mut whitespace = false;
    for c in model.chars() {
        if c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c) {
            if !whitespace {
                out.push('_');
            }
            whitespace = true;
        } else {
            out.push(c);
            whitespace = false;
        }
    }
    out
}
impl Drop for TimingEvidence {
    fn drop(&mut self) {
        if self.timing.dispatched.is_none() {
            return;
        }
        let fields = self
            .timing
            .components()
            .into_iter()
            .map(|(key, value)| {
                format!(
                    "{key}={}",
                    value
                        .map(|v| format!("{v:.6}"))
                        .unwrap_or_else(|| "None".into())
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        tracing::info!(
            "Agent model-plane timing schema={} wire={} run_id={} call_id={} model={} {}",
            TIMING_SCHEMA_VERSION,
            self.wire,
            self.run,
            self.call,
            self.model,
            fields
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_write_stamps_and_additive_identity() {
        let mut t = ModelPlaneTiming::start(10.0);
        t.note_dispatch(10.1);
        t.note_dispatch(20.0);
        t.note_first_frame(10.4);
        t.note_first_frame(30.0);
        t.note_first_visible(10.5);
        t.note_first_visible(40.0);
        assert_eq!(t.dispatched, Some(10.1));
        assert!(t.identity_residual().unwrap() < IDENTITY_TOLERANCE_SECONDS);
        assert_eq!(ModelPlaneTiming::start(1.0).identity_residual(), None);
        assert!(client_residual_within_tolerance(Some(1.0), Some(1.1)));
        assert!(!client_residual_within_tolerance(Some(1.0), Some(0.5)));
    }
}
