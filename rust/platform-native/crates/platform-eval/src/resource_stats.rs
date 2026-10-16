//! Deterministic PPR-00 resource aggregation, retaining source units and windows.
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub const CONTAINERS: &[&str] = &[
    "ai-gateway-pg",
    "ai-gateway-redis",
    "ai-gateway-qdrant",
    "ai-gateway-backend",
    "ai-gateway-frontend",
    "ai-gateway-knowledge-service",
    "ai-gateway-knowledge-worker",
    "ai-gateway-agent-capability-worker",
    "ai-gateway-agent-runtime",
];
pub const DERIVATION_SERVICES: &[&str] = &[
    "ai-gateway-backend",
    "ai-gateway-agent-runtime",
    "ai-gateway-pg",
    "ai-gateway-redis",
    "ai-gateway-qdrant",
];
pub const CLASSES: &[&str] = &[
    "success",
    "http_429_or_admission_reject",
    "http_5xx",
    "stream_error",
    "timeout",
    "infrastructure_error",
];
pub const OWNERSHIP: &str = "com.docker.compose.project.working_dir";
pub const CAVEATS: &[&str] = &[
    "docker stats reports the memory working set (cgroup memory.current minus inactive file cache), not strict process RSS.",
    "docker stats gives container granularity only.",
    "delta/K mixes true per-stream state with shared warm-up growth, caches and allocator retention.",
    "post-level cooldown RSS is also recorded (retained vs in-flight split).",
];
pub fn round(v: f64, n: i32) -> f64 {
    let p = 10f64.powi(n);
    (v * p).round() / p
}
pub fn finite(v: &Value) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite())
}
pub fn mean(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        None
    } else {
        Some(round(v.iter().sum::<f64>() / v.len() as f64, 6))
    }
}
pub fn percentile(v: &[f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut a = v.to_vec();
    a.sort_by(f64::total_cmp);
    let i = (a.len() - 1) as f64 * p;
    let low = i.floor() as usize;
    let high = i.ceil() as usize;
    Some(round(a[low] + (a[high] - a[low]) * (i - low as f64), 6))
}
pub fn human_mib(v: &str) -> Option<f64> {
    let re = regex::Regex::new(r"(?i)^\s*([0-9]+(?:\.[0-9]+)?)\s*([KMGT])?i?B\s*$").ok()?;
    let c = re.captures(v)?;
    let number = c[1].parse::<f64>().ok()?;
    let power = match c.get(2).map(|x| x.as_str().to_uppercase()).as_deref() {
        Some("K") => 1,
        Some("M") => 2,
        Some("G") => 3,
        Some("T") => 4,
        _ => 0,
    };
    Some(round(number * 1024f64.powi(power) / 1048576., 6))
}
pub fn percent(v: &Value) -> Option<f64> {
    v.as_str()?
        .trim()
        .trim_end_matches('%')
        .parse::<f64>()
        .ok()
        .filter(|x| x.is_finite())
}
pub fn classify(reason: &str) -> &'static str {
    let s = reason.to_lowercase();
    if s.contains("timed out") || s.contains("timeout") {
        "timeout"
    } else if [
        "429",
        "rate_limit",
        "rate limit",
        "concurrency",
        "admission",
    ]
    .iter()
    .any(|t| s.contains(t))
    {
        "http_429_or_admission_reject"
    } else if s.contains("http_5") {
        "http_5xx"
    } else {
        "stream_error"
    }
}
pub fn slope(points: &[(f64, f64)]) -> Option<f64> {
    if points.len() < 2 {
        return None;
    }
    let mx = points.iter().map(|p| p.0).sum::<f64>() / points.len() as f64;
    let my = points.iter().map(|p| p.1).sum::<f64>() / points.len() as f64;
    let den = points.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>();
    if den == 0. {
        None
    } else {
        Some(round(
            points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / den,
            6,
        ))
    }
}
pub fn window(samples: &[Value], phase: &str, name: &str, limit: Option<f64>) -> Value {
    let rows = samples
        .iter()
        .filter(|s| s["phase"] == phase && s["containers"].get(name).is_some())
        .map(|s| &s["containers"][name])
        .collect::<Vec<_>>();
    let values = |k: &str| {
        rows.iter()
            .filter_map(|r| finite(&r[k]))
            .collect::<Vec<_>>()
    };
    let mem = values("mem_used_mib");
    let cpu = values("cpu_pct");
    let times = samples
        .iter()
        .filter(|s| s["phase"] == phase)
        .filter_map(|s| finite(&s["elapsed_seconds"]))
        .collect::<Vec<_>>();
    let mean = mean(&mem);
    let peak = mem.iter().copied().reduce(f64::max);
    let span = times
        .iter()
        .copied()
        .reduce(f64::max)
        .zip(times.iter().copied().reduce(f64::min))
        .map(|(a, b)| round(a - b, 3));
    json!({"samples":rows.len(),"window_seconds":span,"mean_mib":mean,"p50_mib":percentile(&mem,0.5),"p95_mib":percentile(&mem,0.95),"peak_mib":peak,"pct_of_mem_limit_mean":mean.zip(limit.filter(|x|*x!=0.)).map(|(a,b)|round(a/b*100.,2)),"pct_of_mem_limit_peak":peak.zip(limit.filter(|x|*x!=0.)).map(|(a,b)|round(a/b*100.,2)),"peak_cpu_pct":cpu.into_iter().reduce(f64::max),"net_in_mib_cumulative_mean":self::mean(&values("net_in_mib")),"net_out_mib_cumulative_mean":self::mean(&values("net_out_mib"))})
}
pub fn derive(warmed: &Value, steady: &BTreeMap<usize, Value>) -> Value {
    let mut services = serde_json::Map::new();
    for name in DERIVATION_SERVICES {
        let base = finite(&warmed[*name]["mean_mib"]);
        let mut deltas = serde_json::Map::new();
        let mut per = serde_json::Map::new();
        let mut points = vec![];
        for (k, s) in steady {
            let delta = base.zip(finite(&s[*name]["mean_mib"])).map(|(a, b)| b - a);
            deltas.insert(k.to_string(), json!(delta.map(|v| round(v, 6))));
            per.insert(k.to_string(), json!(delta.map(|v| round(v / *k as f64, 6))));
            if let Some(v) = delta {
                points.push((*k as f64, v));
            }
        }
        services.insert(name.to_string(),json!({"warmed_idle_rss_mean_mib":base,"delta_mib":deltas,"per_stream_mib":per,"least_squares_slope_mib_per_stream":slope(&points)}));
    }
    json!({"policy":"delta_K = mean_RSS(steady window, K) - warmed_idle_RSS; per_stream_K = delta_K / K; slope = least-squares line through (K, delta_K) over K > 0","services":services,"caveats":CAVEATS})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn units_and_slopes() {
        assert_eq!(human_mib("1GiB"), Some(1024.));
        assert_eq!(human_mib("512KiB"), Some(0.5));
        assert_eq!(slope(&[(1., 2.), (5., 10.)]), Some(2.));
        assert_eq!(human_mib("bad"), None);
    }
    #[test]
    fn phase_windows() {
        let s = vec![
            json!({"phase":"idle","elapsed_seconds":1.,"containers":{"a":{"mem_used_mib":10.,"cpu_pct":5.}}}),
            json!({"phase":"busy","elapsed_seconds":2.,"containers":{"a":{"mem_used_mib":100.}}}),
        ];
        let v = window(&s, "idle", "a", Some(100.));
        assert_eq!(v["mean_mib"], 10.);
        assert_eq!(v["pct_of_mem_limit_peak"], 10.);
        assert_eq!(v["samples"], 1);
    }
}
