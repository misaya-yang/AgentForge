//! Additive BFF/JWKS OpenAPI contract; the legacy bearer document is frozen.
// SPDX-License-Identifier: Apache-2.0
use serde_json::{Value, json};
pub fn extend_document(document: &mut Value) {
    let mut legacy = json!({});
    super::contract::extend_document(&mut legacy);
    let old_login = &legacy["paths"][super::contract::LOGIN]["post"]["responses"]["200"]["content"]
        ["application/json"]["schema"];
    let old_me = &legacy["paths"][super::contract::ME]["get"]["responses"]["200"]["content"]["application/json"]
        ["schema"];
    document["components"]["securitySchemes"]["browserSession"] = json!({"type":"apiKey","in":"cookie","name":"__Host-agentforge_session","description":"Opaque HttpOnly Secure SameSite=Lax cookie; HTTPS only"});
    let error = json!({"description":"Authentication, origin, CSRF, current authority or dependency failure"});
    for (path, method, id, session, csrf) in [
        (
            "/api/auth/session",
            "post",
            "browser_session_login",
            false,
            false,
        ),
        (
            "/api/auth/session",
            "get",
            "browser_session_current",
            true,
            false,
        ),
        (
            "/api/auth/session",
            "delete",
            "browser_session_logout",
            true,
            true,
        ),
        (
            "/api/auth/session/password",
            "post",
            "browser_session_password",
            true,
            true,
        ),
        (
            "/.well-known/jwks.json",
            "get",
            "api_access_public_keys",
            false,
            false,
        ),
    ] {
        let mut operation = json!({"operationId":id,"x-native-owner":"platform-auth::browser","security":if session{json!([{"browserSession":[]}])}else{json!([])},"responses":{"200":{"description":"BFF session metadata (never a bearer token), mutation confirmation, or public Ed25519 JWKS"},"401":error,"403":error,"503":error}});
        let properties = if path == "/.well-known/jwks.json" {
            json!({"keys":{"type":"array","items":{"type":"object","properties":{"kty":{"const":"OKP"},"crv":{"const":"Ed25519"},"use":{"const":"sig"},"alg":{"const":"EdDSA"},"kid":{"type":"string"},"x":{"type":"string"}},"required":["kty","crv","use","alg","kid","x"]}}})
        } else if path == "/api/auth/session" && method == "post" {
            json!({"user":old_login["properties"]["user"],"force_password_change":{"type":"boolean"},"expires_in":{"type":"integer","const":10800},"session_type":{"const":"cookie"},"csrf_token":{"type":"string","minLength":64,"maxLength":64}})
        } else if method == "get" {
            json!({"user":old_me,"csrf_token":{"type":"string","minLength":64,"maxLength":64},"expires_at":{"type":"integer"}})
        } else {
            json!({"message":{"type":"string"}})
        };
        let required = properties
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        operation["responses"]["200"]["content"] = json!({"application/json":{"schema":{"type":"object","properties":properties,"required":required}}});
        if path.starts_with("/api/auth/") {
            operation["parameters"] = json!([{"name":"Sec-Fetch-Site","in":"header","required":true,"schema":{"type":"string","enum":["same-origin","same-site"]}},{"name":"Origin","in":"header","required":method!="get","schema":{"type":"string","format":"uri"}}]);
            operation["x-session-absolute-ttl-seconds"] = json!(10800);
            operation["x-session-idle-ttl-seconds"] = json!(10800);
        }
        if csrf {
            operation["parameters"].as_array_mut().unwrap().push(json!({"name":"X-CSRF-Token","in":"header","required":true,"schema":{"type":"string","minLength":64,"maxLength":64}}));
        }
        if method == "post" {
            let properties = if path.ends_with("password") {
                json!({"current_password":{"type":"string"},"new_password":{"type":"string"},"confirm_password":{"type":"string"}})
            } else {
                json!({"email":{"type":"string"},"password":{"type":"string"}})
            };
            let required = properties
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            operation["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"type":"object","properties":properties,"required":required}}}});
        }
        document["paths"][path][method] = operation;
    }
    document["paths"]["/api/auth/session/step-up"]["get"] = json!({
        "operationId":"browser_session_step_up_status","security":[{"browserSession":[]}],
        "x-native-owner":"platform-auth::browser",
        "parameters":[{"name":"Sec-Fetch-Site","in":"header","required":true,"schema":{"type":"string","enum":["same-origin","same-site"]}},{"name":"Origin","in":"header","schema":{"type":"string","format":"uri"}}],
        "description":"Current server policy and session-bound recent passkey assurance. Feature-disabled is explicit; dependency errors never imply disabled.",
        "responses":{"200":{"description":"Current policy","content":{"application/json":{"schema":{"type":"object","required":["enabled","verified","passkey_enrolled"],"properties":{"enabled":{"type":"boolean"},"verified":{"type":"boolean"},"passkey_enrolled":{"type":"boolean"}}}}}},"401":error,"403":error,"503":error}
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bff_never_advertises_a_bearer_response_and_mutations_require_csrf() {
        let mut doc = json!({});
        extend_document(&mut doc);
        assert_eq!(
            doc["components"]["securitySchemes"]["browserSession"]["name"],
            "__Host-agentforge_session"
        );
        for (path, method) in [
            ("/api/auth/session", "delete"),
            ("/api/auth/session/password", "post"),
        ] {
            assert!(
                doc["paths"][path][method]["parameters"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p["name"] == "X-CSRF-Token" && p["required"] == true)
            );
        }
        assert!(!doc.to_string().contains("access_token"));
        assert_eq!(
            doc["paths"]["/api/auth/session/step-up"]["get"]["security"],
            json!([{"browserSession":[]}])
        );
    }
}
