//! Offline inventory of the source's eight explicit service→upstream mappings.
//! Repairs the historical script's call to an absent inventory_rows method.
use ai_platform_gateway_foundation::capacity::{CapacityRequest, CapacityResolver};
use serde_json::{Value, json};
const SERVICES: &[&str] = &[
    "local-2024-agent",
    "langgraph-agent",
    "agent",
    "assistant",
    "agent-runtime",
    "knowledge",
    "knowledge-service",
    "image-generation",
];
fn inventory(resolver: &CapacityResolver) -> Result<Value, String> {
    let mut rows = vec![];
    for service in SERVICES {
        let budgets = resolver
            .resolve(&CapacityRequest {
                tenant_id: String::new(),
                service_id: (*service).into(),
                request_class: "sync".into(),
                upstream_group: Value::Null,
                provider_id: Value::Null,
                is_admin_read: false,
                service_config: Value::Null,
                resource_kind: "request".into(),
            })
            .map_err(|e| e.to_string())?;
        for budget in budgets
            .into_iter()
            .filter(|b| matches!(b.scope.as_str(), "upstream" | "service"))
        {
            let mut row = serde_json::to_value(&budget).map_err(|e| e.to_string())?;
            row.as_object_mut().unwrap().remove("key");
            row["service_id"] = json!(service);
            row["budget_key"] = json!(budget.key);
            rows.push(row);
        }
    }
    let missing = rows
        .iter()
        .filter(|r| r["source_status"] == "missing")
        .count();
    Ok(
        json!({"mode":resolver.mode,"cluster_epoch":resolver.cluster_epoch,"services":rows,"missing_count":missing}),
    )
}
fn exit_code(payload: &Value, allow_missing: bool) -> u8 {
    if payload["missing_count"].as_u64().unwrap_or(0) > 0 && !allow_missing {
        2
    } else {
        0
    }
}
fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mut json_format = false;
    let mut allow_missing = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--allow-missing" => allow_missing = true,
            "--format" => {
                i += 1;
                match args.get(i).map(String::as_str) {
                    Some("json") => json_format = true,
                    Some("text") => json_format = false,
                    _ => {
                        eprintln!("--format must be json or text");
                        return std::process::ExitCode::from(2);
                    }
                }
            }
            _ => {
                eprintln!(
                    "Usage: gateway_capacity_inventory [--format json|text] [--allow-missing]"
                );
                return std::process::ExitCode::from(2);
            }
        }
        i += 1;
    }
    let payload = match inventory(&CapacityResolver::default()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return std::process::ExitCode::from(2);
        }
    };
    if json_format {
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        for row in payload["services"].as_array().unwrap() {
            println!(
                "{} -> {} enforced={} source_status={}",
                row["service_id"].as_str().unwrap(),
                row["budget_key"].as_str().unwrap(),
                if row["enforced"] == true {
                    "True"
                } else {
                    "False"
                },
                row["source_status"].as_str().unwrap()
            );
        }
    }
    std::process::ExitCode::from(exit_code(&payload, allow_missing))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_defaults_and_mappings() {
        let value = inventory(&CapacityResolver::default()).unwrap();
        assert_eq!(value["mode"], "single-node");
        assert_eq!(value["cluster_epoch"], "uat-2026-05");
        assert_eq!(value["services"].as_array().unwrap().len(), 8);
        assert_eq!(value["missing_count"], 0);
        assert_eq!(
            value["services"][0]["budget_key"],
            "upstream.langgraph_agent"
        );
        assert_eq!(value["services"][3]["budget_key"], "upstream.agent_runtime");
        assert_eq!(value["services"][7]["limit"], 2);
    }
    #[test]
    fn missing_exits_two_unless_explicitly_allowed() {
        assert_eq!(exit_code(&json!({"missing_count":1}), false), 2);
        assert_eq!(exit_code(&json!({"missing_count":1}), true), 0);
    }
}
