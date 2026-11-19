use serde_json::{Value, json};
use std::path::Path;

/// Synthetic rendered config for offline validator tests, never evidence of a Docker render.
pub fn rendered(root: &Path) -> Value {
    let lock: Value = serde_json::from_slice(
        &std::fs::read(root.join("deploy/agent-runtime-source/lock.json")).unwrap(),
    )
    .unwrap();
    let upstream = lock["source"]["upstream_sha"].as_str().unwrap();
    let overlay = &lock["build"]["overlay_sha256"].as_str().unwrap()[..12];
    json!({"services":{
        "gateway":{"image":"example/gateway:2.0.0"},
        "frontend":{"image":"example/web:2.0.0"},
        "knowledge-service":{"image":"example/knowledge:2.0.0"},
        "knowledge-worker":{"image":"example/knowledge:2.0.0"},
        "migrate":{"image":"example/migrate:2.0.0"},
        "agent-runtime":{"image":format!("ai-gateway-agent-runtime:local-{}-{overlay}",&upstream[..12]),
            "environment":{"AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION":format!("{upstream}+{overlay}")}},
        "agent-capability-worker":{"image":format!("ai-gateway-agent-capability-worker:local-{}-{overlay}",&upstream[..12])}
    }})
}
