use super::*;
use super::{dashboard_sql as q, summary::Param};
impl TraceRepository {
    pub async fn get_dashboard(&self, tenant_id: &str, days: i32) -> Result<Value, TraceError> {
        let mut metrics = self.get_summary(tenant_id, None, days).await?;
        let mut rows = Vec::new();
        for sql in [
            q::QUERY_0,
            q::QUERY_1,
            q::QUERY_2,
            q::QUERY_3,
            q::QUERY_4,
            q::QUERY_5,
        ] {
            let mut params = vec![Param::Text(tenant_id.into())];
            if sql.contains("$2") {
                params.push(Param::Int(days.max(1) as i64));
            }
            rows.push(
                self.query_rows(sql.into(), params)
                    .await?
                    .into_iter()
                    .next()
                    .unwrap_or(json!({})),
            );
        }
        if let Some(counts) = rows[0].as_object() {
            metrics.as_object_mut().unwrap().extend(counts.clone());
        }
        let mut runtime = rows[3].clone();
        let total = runtime["assistant_captured_traces"].as_f64().unwrap_or(0.);
        let succeeded = runtime["assistant_succeeded_traces"].as_f64().unwrap_or(0.);
        let trajectory = runtime["runtime_trajectory_traces"].as_f64().unwrap_or(0.);
        let rounded = |numerator: f64| {
            if total == 0. {
                0.
            } else {
                format!("{:.4}", numerator / total)
                    .parse::<f64>()
                    .unwrap_or(0.)
            }
        };
        runtime["pass_rate"] = json!(rounded(succeeded));
        runtime["trajectory_pass_rate"] = json!(rounded(trajectory));
        runtime["tool_safety_failures"] =
            json!(rows[4]["tool_safety_failures"].as_i64().unwrap_or(0));
        runtime["assistant_status"] = json!("enabled");
        for (family, key) in [
            ("rag", "rag_captured_traces"),
            ("langgraph", "langgraph_captured_traces"),
        ] {
            runtime[format!("{family}_status")] =
                json!(if runtime[key].as_i64().unwrap_or(0) > 0 {
                    "wired"
                } else {
                    "partial"
                });
        }
        for key in ["latest_baseline", "latest_candidate"] {
            metrics[key] = rows[5][key].clone();
        }
        for key in ["pass_rate", "trajectory_pass_rate", "tool_safety_failures"] {
            metrics[key] = runtime[key].clone();
        }
        metrics["critical_failures"] = json!(
            runtime["critical_runtime_failures"].as_i64().unwrap_or(0)
                + runtime["tool_safety_failures"].as_i64().unwrap_or(0)
        );
        Ok(
            json!({"metrics":metrics,"run_health":rows[1],"queue_health":rows[2],"runtime_health":runtime,"latest_gate_status":{"status":"not_run","source":"offline"}}),
        )
    }
}
