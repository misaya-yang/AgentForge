//! Offline exporter: no Gateway initialization, environment configuration or baseline read.
#[allow(dead_code)]
#[path = "../../platform-gateway-foundation/src/native_contract.rs"]
mod native_contract;
#[allow(dead_code)]
#[path = "../../platform-gateway/src/migration_host/knowledge_proxy/contract.rs"]
mod knowledge_proxy_contract;
use sha2::{Digest, Sha256};
fn main() -> std::process::ExitCode {
    if std::env::args().skip(1).collect::<Vec<_>>() != ["--native-only"] {
        eprintln!(
            "Usage: ai-platform-openapi --native-only (bounded, incomplete Gateway contract)"
        );
        return std::process::ExitCode::from(2);
    }
    let mut document = native_contract::document();
    knowledge_proxy_contract::extend_document(&mut document);
    document["x-native-contract"]["source_sha256"] = serde_json::json!(format!(
        "{:x}",
        Sha256::digest(include_bytes!(
            "../../platform-gateway-foundation/src/native_contract.rs"
        ))
    ));
    document["x-native-contract"]["response_source_sha256"] = serde_json::json!(format!(
        "{:x}",
        Sha256::digest(include_bytes!(
            "../../platform-gateway-foundation/src/native_responses.rs"
        ))
    ));
    document["x-native-contract"]["common_source_sha256"] = serde_json::json!({

        "native_invocation_task_contract.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/native_invocation_task_contract.rs"))),
        "invocation_http.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/invocation_http.rs"))),
        "task_http.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/task_http.rs"))),
        "task_manager.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/task_manager.rs"))),
        "task_actor.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/task_actor.rs"))),
        "task_durable_worker.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/task_durable_worker.rs"))),
        "gateway_dispatcher.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/gateway_dispatcher.rs"))),
        "../../platform-auth/src/contract.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-auth/src/contract.rs"))),
        "../../platform-auth/src/http.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-auth/src/http.rs"))),
        "../../platform-auth/src/lib.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-auth/src/lib.rs"))),
        "../../platform-auth/src/accounts.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-auth/src/accounts.rs"))),
        "../../platform-auth/src/sessions.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-auth/src/sessions.rs"))),
        "../../platform-gateway/src/migration_host/foundation.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-gateway/src/migration_host/foundation.rs"))),
        "session_storage.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/session_storage.rs"))),
        "../../platform-auth/src/api_keys.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-auth/src/api_keys.rs"))),
        "../../platform-auth/src/service.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-auth/src/service.rs"))),
        "../../platform-gateway/src/migration_host/bootstrap/graph.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-gateway/src/migration_host/bootstrap/graph.rs"))),
        "../../platform-gateway/src/migration_host/bootstrap/foundation_backends.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-gateway/src/migration_host/bootstrap/foundation_backends.rs"))),
        "../../platform-gateway/src/migration_agent/control.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-gateway/src/migration_agent/control.rs"))),
        "../../platform-gateway-integrations/src/migration_adapters/sdk_session_cleanup.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-gateway-integrations/src/migration_adapters/sdk_session_cleanup.rs"))),
        "sessions_api.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/sessions_api.rs"))),
        "../../platform-gateway/src/session_history.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-gateway/src/session_history.rs"))),
        "../../platform-gateway/src/lib.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/../../platform-gateway/src/lib.rs"))),
        "native_sessions_contract.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/native_sessions_contract.rs"))),
        "native_roles_contract.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/native_roles_contract.rs"))),
        "roles_api.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/roles_api.rs"))),
        "native_users_contract.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/native_users_contract.rs"))),
        "users_api.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/users_api.rs"))),
        "users_store.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/users_store.rs"))),
        "users_store/guard.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/users_store/guard.rs"))),
        "rate_config_api.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/rate_config_api.rs"))),
        "usage_parser.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/usage_parser.rs"))),
        "native_services_contract.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/native_services_contract.rs"))),
        "services_api.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/services_api.rs"))),
        "protocol.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/protocol.rs"))),
        "request_validation.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/request_validation.rs"))),
        "service_registry.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/service_registry.rs"))),
        "service_config_api.rs": format!("{:x}", Sha256::digest(include_bytes!("../../platform-gateway-foundation/src/service_config_api.rs"))),
    });
    // Keep this bounded owner group outside the existing json! map expansion.
    let connector_sources: [(&str, &[u8]); 7] = [
        ("native_connectors_contract.rs", include_bytes!("../../platform-gateway-foundation/src/native_connectors_contract.rs")),
        ("connector_user_api.rs", include_bytes!("../../platform-gateway-foundation/src/connector_user_api.rs")),
        ("connector_user_service.rs", include_bytes!("../../platform-gateway-foundation/src/connector_user_service.rs")),
        ("connector_user_store.rs", include_bytes!("../../platform-gateway-foundation/src/connector_user_store.rs")),
        ("connector_user_types.rs", include_bytes!("../../platform-gateway-foundation/src/connector_user_types.rs")),
        ("../../platform-auth/src/admission.rs", include_bytes!("../../platform-gateway-foundation/src/../../platform-auth/src/admission.rs")),
        ("../../platform-gateway/src/migration_host/bootstrap/foundation.rs", include_bytes!("../../platform-gateway-foundation/src/../../platform-gateway/src/migration_host/bootstrap/foundation.rs")),
    ];
    for (name, source) in connector_sources {
        document["x-native-contract"]["common_source_sha256"][name] =
            serde_json::json!(format!("{:x}", Sha256::digest(source)));
    }
    let api_key_sources: [(&str, &[u8]); 6] = [
        ("native_api_keys_contract.rs", include_bytes!("../../platform-gateway-foundation/src/native_api_keys_contract.rs")),
        ("self_service_api_keys_api.rs", include_bytes!("../../platform-gateway-foundation/src/self_service_api_keys_api.rs")),
        ("self_service_api_keys.rs", include_bytes!("../../platform-gateway-foundation/src/self_service_api_keys.rs")),
        ("self_service_api_keys_store.rs", include_bytes!("../../platform-gateway-foundation/src/self_service_api_keys_store.rs")),
        ("http_content_type.rs", include_bytes!("../../platform-gateway-foundation/src/http_content_type.rs")),
        ("service_access_resolver.rs", include_bytes!("../../platform-gateway-foundation/src/service_access_resolver.rs")),
    ];
    for (name, source) in api_key_sources {
        document["x-native-contract"]["common_source_sha256"][name] =
            serde_json::json!(format!("{:x}", Sha256::digest(source)));
    }
    println!("{}", serde_json::to_string_pretty(&document).unwrap());
    std::process::ExitCode::SUCCESS
}
