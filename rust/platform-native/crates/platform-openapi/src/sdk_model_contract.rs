//! Actual SDK-run model routes. Provider wire events deliberately remain opaque.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub fn extend_document(document: &mut Value) {
    let run = json!({"type":"object","additionalProperties":false,"required":["run_id"],"properties":{"run_id":{"type":"string","minLength":1}}});
    let execute = json!({"type":"object","additionalProperties":false,"required":["run_id","call_id","wire","body"],"properties":{
        "run_id":{"type":"string","minLength":1},"call_id":{"type":"string","minLength":1},
        "wire":{"type":"string","enum":["responses_v1","anthropic_messages"]},"body":{"type":"object","additionalProperties":true}
    }});
    document["components"]["securitySchemes"]["SdkRunGrant"] = json!({"type":"http","scheme":"bearer","description":"Short-lived opaque grant bound to the current SDK run, tenant, user, session, owner and fence. A provider key or browser cookie is never a substitute."});
    for (suffix, id, input, content) in [
        (
            "lease",
            "sdk_model_lease",
            run.clone(),
            json!({"application/json":{"schema":{"type":"object","required":["schema","expires_at_epoch_ms"],"properties":{"schema":{"const":"sdk-model-plane/v1"},"expires_at_epoch_ms":{"type":"integer","minimum":0}}}}}),
        ),
        (
            "execute",
            "sdk_model_execute",
            execute,
            json!({"text/event-stream":{"schema":{"type":"string"},"itemSchema":{"type":"object","properties":{"data":{"type":"string"},"event":{"type":"string"},"id":{"type":"string"},"retry":{"type":"integer","minimum":0}}}}}),
        ),
        (
            "revocations",
            "sdk_model_revocations",
            run,
            json!({"text/event-stream":{"schema":{"type":"string"},"itemSchema":{"type":"object","required":["data"],"properties":{"data":{"type":"string"}}}}}),
        ),
    ] {
        let path = format!("/internal/v1/sdk-tools/models/{suffix}");
        let mut operation = json!({
            "operationId":id,"tags":["SDK model plane"],"security":[{"SdkRunGrant":[]}],
            "x-native-owner":"platform-gateway::migration_host::knowledge_proxy::sdk_models",
            "description":"Uses existing live SDK run authority and the Gateway model registry, price book, quota and dispatch ledger. Provider credentials never leave the Gateway. Reusing an uncertain call does not dispatch a second provider request.",
            "requestBody":{"required":true,"content":{"application/json":{"schema":input}}},
            "responses":{"200":{"description":"Admitted SDK model operation","content":content},"403":{"description":"Missing, revoked or mismatched run grant"},"409":{"description":"Previously reserved or uncertain call"},"413":{"description":"Request exceeds the 4 MiB execution limit"},"422":{"description":"Invalid execution request"},"503":{"description":"Authority, quota, model or settlement unavailable"}}
        });
        if suffix == "revocations" {
            operation["x-native-sse"] = json!({"data_schema":{"type":"object","required":["type","run_id"],"properties":{"type":{"enum":["sdk.lease_heartbeat","sdk.lease_revoked"]},"run_id":{"type":"string"}},"additionalProperties":false}});
        }
        document["paths"][path]["post"] = operation;
    }
    for (name, bytes) in [
        (
            "sdk_model_contract.rs",
            include_bytes!("sdk_model_contract.rs").as_slice(),
        ),
        (
            "../../platform-gateway/src/migration_host/knowledge_proxy/sdk_models.rs",
            include_bytes!(
                "../../platform-gateway/src/migration_host/knowledge_proxy/sdk_models.rs"
            )
            .as_slice(),
        ),
    ] {
        document["x-native-contract"]["common_source_sha256"][name] =
            json!(format!("{:x}", Sha256::digest(bytes)));
    }
}
