//! Canonical latency-window policy from src/core/gateway/admission.py.
//! This is a policy primitive, not admission middleware or a capacity lease manager.
use std::{
    collections::{BTreeMap, VecDeque},
    time::Instant,
};

#[derive(Clone, Debug, PartialEq)]
pub struct CapacityRejected {
    pub budget_key: String,
    pub code: String,
    pub status_code: u16,
    pub queue_wait_ms: f64,
    pub retry_after: i64,
    pub message: String,
    pub headers: BTreeMap<String, String>,
}
#[derive(Clone, Copy, Debug)]
pub struct LoadShedConfig {
    pub window_seconds: f64,
    pub low_threshold_ms: f64,
    pub normal_threshold_ms: f64,
    pub high_threshold_ms: f64,
    pub retry_after: i64,
}
impl Default for LoadShedConfig {
    fn default() -> Self {
        Self {
            window_seconds: 30.0,
            low_threshold_ms: 5000.0,
            normal_threshold_ms: 10000.0,
            high_threshold_ms: 20000.0,
            retry_after: 1,
        }
    }
}
pub struct AdaptiveLoadShedder {
    config: LoadShedConfig,
    epoch: Instant,
    samples: VecDeque<(f64, f64)>,
}
impl AdaptiveLoadShedder {
    /// Non-finite configuration is rejected at this typed boundary; Python's NaN sorting
    /// behavior is not a supported policy. Finite values preserve Python's clamps.
    pub fn new(mut config: LoadShedConfig) -> Result<Self, &'static str> {
        if ![
            config.window_seconds,
            config.low_threshold_ms,
            config.normal_threshold_ms,
            config.high_threshold_ms,
        ]
        .into_iter()
        .all(f64::is_finite)
        {
            return Err("nonfinite_load_shed_config");
        }
        config.window_seconds = config.window_seconds.max(1.0);
        config.low_threshold_ms = config.low_threshold_ms.max(1.0);
        config.normal_threshold_ms = config.normal_threshold_ms.max(1.0);
        config.high_threshold_ms = config.high_threshold_ms.max(1.0);
        config.retry_after = config.retry_after.max(1);
        Ok(Self {
            config,
            epoch: Instant::now(),
            samples: VecDeque::new(),
        })
    }
    pub fn record_latency(&mut self, latency_ms: f64) -> Result<(), &'static str> {
        self.record_at(self.epoch.elapsed().as_secs_f64(), latency_ms)
    }
    pub fn record_at(&mut self, now: f64, latency_ms: f64) -> Result<(), &'static str> {
        if !now.is_finite() || !latency_ms.is_finite() {
            return Err("nonfinite_load_shed_sample");
        }
        self.samples.push_back((now, latency_ms.max(0.0)));
        self.evict(now);
        Ok(())
    }
    /// Like Python, this accessor does not evict by itself.
    pub fn p99_latency_ms(&self) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }
        let mut values: Vec<_> = self.samples.iter().map(|(_, v)| *v).collect();
        values.sort_by(f64::total_cmp);
        let index = ((values.len() as f64 * 0.99).ceil() as usize)
            .saturating_sub(1)
            .min(values.len() - 1);
        Some(values[index])
    }
    pub fn maybe_reject(
        &mut self,
        service_id: &str,
        request_class: &str,
        priority: i64,
    ) -> Option<CapacityRejected> {
        self.maybe_reject_at(
            self.epoch.elapsed().as_secs_f64(),
            service_id,
            request_class,
            priority,
        )
    }
    pub fn maybe_reject_at(
        &mut self,
        now: f64,
        service_id: &str,
        request_class: &str,
        priority: i64,
    ) -> Option<CapacityRejected> {
        // Priority-zero bypass also skips eviction, exactly as the source.
        if priority <= 0 {
            return None;
        }
        self.evict(now);
        let p99 = self.p99_latency_ms()?;
        let threshold = match priority {
            1 => self.config.high_threshold_ms,
            2 => self.config.normal_threshold_ms,
            _ => self.config.low_threshold_ms,
        };
        if p99 <= threshold {
            return None;
        }
        let budget_key = format!("load_shed.{service_id}.{request_class}");
        let retry_after = self.config.retry_after;
        let headers = BTreeMap::from([
            ("X-Gateway-Capacity-Key".into(), budget_key.clone()),
            ("X-Gateway-Queue-Wait-Ms".into(), "0.0".into()),
            ("Retry-After".into(), retry_after.to_string()),
            ("X-Gateway-Load-Shed".into(), "true".into()),
            ("X-Gateway-Request-Priority".into(), priority.to_string()),
        ]);
        Some(CapacityRejected {
            budget_key,
            code: "GATEWAY_LOAD_SHED".into(),
            status_code: 503,
            queue_wait_ms: 0.0,
            retry_after,
            message: format!("Gateway load shedding active for {service_id}"),
            headers,
        })
    }
    fn evict(&mut self, now: f64) {
        let cutoff = now - self.config.window_seconds;
        while self.samples.front().is_some_and(|(t, _)| *t < cutoff) {
            self.samples.pop_front();
        }
    }
}
