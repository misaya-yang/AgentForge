//! Device authorization producer contract, kept pure for platform-openapi reuse.
use serde_json::{Value, json};
pub fn extend(document: &mut Value) {
    let string = json!({"type":"string"});
    let secret = json!({"type":"string","writeOnly":true});
    let authorize = json!({"type":"object","additionalProperties":false,"required":["client_id","code_challenge","code_challenge_method"],"properties":{"client_id":{"const":"agentforge-cli"},"scope":{"type":"string","enum":["","platform"],"default":"platform"},"code_challenge":{"type":"string","minLength":43,"maxLength":43},"code_challenge_method":{"const":"S256"}}});
    let device = json!({"type":"object","additionalProperties":false,"required":["grant_type","client_id","device_code","code_verifier"],"properties":{"grant_type":{"const":"urn:ietf:params:oauth:grant-type:device_code"},"client_id":{"const":"agentforge-cli"},"device_code":secret,"code_verifier":{"type":"string","minLength":43,"maxLength":128,"writeOnly":true}}});
    let refresh = json!({"type":"object","additionalProperties":false,"required":["grant_type","client_id","refresh_token"],"properties":{"grant_type":{"const":"refresh_token"},"client_id":{"const":"agentforge-cli"},"refresh_token":secret}});
    let grant = json!({"type":"object","required":["access_token","token_type","expires_in","refresh_token","tenant_id","user_id","scope"],"properties":{"access_token":string,"token_type":{"const":"Bearer"},"expires_in":{"type":"integer","minimum":1,"maximum":300},"refresh_token":string,"tenant_id":string,"user_id":string,"scope":{"const":"platform"}}});
    let authorization = json!({"type":"object","required":["device_code","user_code","verification_uri","verification_uri_complete","expires_in","interval"],"properties":{"device_code":string,"user_code":string,"verification_uri":{"type":"string","format":"uri"},"verification_uri_complete":{"type":"string","format":"uri"},"expires_in":{"const":600},"interval":{"const":5}}});
    let error = json!({"description":"OAuth error; never cache credentials or errors","content":{"application/json":{"schema":{"type":"object","required":["error"],"properties":{"error":{"type":"string","enum":["invalid_request","invalid_client","invalid_grant","authorization_pending","slow_down","access_denied","expired_token","temporarily_unavailable","step_up_required"]}}}}}});
    for (path, id, input, output) in [
        (
            "/api/v1/auth/device/authorize",
            "device_authorize",
            authorize,
            authorization,
        ),
        (
            "/api/v1/auth/device/token",
            "device_token",
            json!({"oneOf":[device,refresh]}),
            grant,
        ),
        (
            "/api/v1/auth/device/revoke",
            "device_revoke",
            json!({"type":"object","additionalProperties":false,"required":["refresh_token"],"properties":{"refresh_token":secret}}),
            json!({"type":"null"}),
        ),
    ] {
        let mut op = json!({"operationId":id,"tags":["device authorization"],"x-native-owner":"platform-auth::device_flow","security":[],"description":"Public native client only; Cookie, Origin, Fetch Metadata and Authorization headers rejected. JSON transport. Refresh rotates once (reused rotated credentials revoke the family), retains no plaintext secrets, and cannot extend the approving browser session (maximum 10800 seconds). Revocation using any issued family refresh token revokes current access immediately. User and tenant derive exclusively from the approving current BFF session.","requestBody":{"required":true,"content":{"application/json":{"schema":input}}},"responses":{"200":{"description":"Device grant","content":{"application/json":{"schema":output}}},"400":error,"401":error,"403":error,"429":error,"503":error}});
        if path.ends_with("revoke") {
            op["responses"].as_object_mut().unwrap().remove("200");
            op["responses"]["204"] = json!({"description":"Revoked or unknown token (idempotent)"});
        }
        document["paths"][path]["post"] = op;
    }
    document["paths"]["/api/auth/device"]["get"] = json!({"operationId":"device_approval_page","security":[],"description":"Static same-origin approval shell. No identity or CSRF metadata is embedded. Explicit review and click required; authenticated fetches use the existing BFF session.","responses":{"200":{"description":"CSP-protected approval page","content":{"text/html":{"schema":string}}}}});
    for (path, method, id) in [
        ("/api/auth/device/request", "get", "device_approval_review"),
        ("/api/auth/device", "post", "device_approval_decide"),
    ] {
        let mut op = json!({"operationId":id,"x-native-owner":"platform-auth::device_flow","security":[{"browserSession":[]}],"parameters":[{"name":"Origin","in":"header","required":method=="post","schema":string},{"name":"Sec-Fetch-Site","in":"header","required":true,"schema":{"enum":["same-origin","same-site"]}}],"responses":{"200":{"description":"Review current tenant/user/client/scope or approval decision; approval requires recent passkey step-up when enterprise policy is enabled"},"400":error,"401":error,"403":error,"429":error,"503":error}});
        if method == "post" {
            op["parameters"].as_array_mut().unwrap().push(json!({"name":"X-CSRF-Token","in":"header","required":true,"schema":{"type":"string","minLength":64,"maxLength":64}}));
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"type":"object","additionalProperties":false,"required":["user_code","approve"],"properties":{"user_code":string,"approve":{"type":"boolean"}}}}}});
        } else {
            op["parameters"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":"user_code","in":"query","required":true,"schema":string}));
        }
        document["paths"][path][method] = op;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_contract_keeps_native_and_browser_authority_distinct() {
        let mut d = json!({});
        extend(&mut d);
        let paths = &d["paths"];
        assert_eq!(
            paths["/api/v1/auth/device/token"]["post"]["security"],
            json!([])
        );
        let schemas = &paths["/api/v1/auth/device/token"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"]["oneOf"];
        assert_eq!(schemas.as_array().unwrap().len(), 2);
        for schema in schemas.as_array().unwrap() {
            assert!(schema["properties"].get("tenant_id").is_none());
            assert!(schema["properties"].get("user_id").is_none());
        }
        let op = &paths["/api/auth/device"]["post"];
        assert_eq!(op["security"], json!([{"browserSession":[]}]));
        assert!(
            op["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["name"] == "X-CSRF-Token" && p["required"] == true)
        );
        assert!(
            paths["/api/v1/auth/device/revoke"]["post"]["responses"]
                .get("204")
                .is_some()
        );
    }
}
