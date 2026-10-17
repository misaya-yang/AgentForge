//! Pure enterprise HTTP producer contract; no identity runtime dependencies.
// SPDX-License-Identifier: Apache-2.0
use serde_json::{Value, json};
pub fn extend(document: &mut Value) {
    document["components"]["securitySchemes"]["enterpriseScim"] = json!({"type":"http","scheme":"bearer","description":"Dedicated expiring SCIM credential bound to path tenant and explicit scim.users.read/write or scim.groups.read/write scopes; API keys and browser cookies are rejected"});
    let object = json!({"type":"object","additionalProperties":true});
    let session = json!({"type":"object","description":"Opaque Secure HttpOnly __Host-agentforge_session cookie plus profile and synchronizer CSRF metadata; no bearer token is returned","properties":{"session_type":{"const":"cookie"},"csrf_token":{"type":"string"},"expires_in":{"const":10800},"user":{"type":"object"}}});
    for (suffix, method, authenticated, properties, required) in [
        ("oidc/{provider}/begin", "post", false, json!({}), vec![]),
        (
            "passkeys/register/start",
            "post",
            true,
            json!({"current_password":{"type":"string","writeOnly":true}}),
            vec![],
        ),
        (
            "passkeys/register/finish",
            "post",
            true,
            json!({"flow_id":{"type":"string"},"credential":object}),
            vec!["flow_id", "credential"],
        ),
        (
            "passkeys/authenticate/start",
            "post",
            false,
            json!({"tenant_id":{"type":"string"},"user_name":{"type":"string"}}),
            vec!["tenant_id", "user_name"],
        ),
        (
            "passkeys/authenticate/finish",
            "post",
            false,
            json!({"flow_id":{"type":"string"},"credential":object}),
            vec!["flow_id", "credential"],
        ),
        ("passkeys/{id}", "delete", true, json!({}), vec![]),
        (
            "recovery",
            "post",
            false,
            json!({"tenant_id":{"type":"string"},"user_name":{"type":"string"},"current_password":{"type":"string","writeOnly":true},"recovery_code":{"type":"string","writeOnly":true}}),
            vec![
                "tenant_id",
                "user_name",
                "current_password",
                "recovery_code",
            ],
        ),
        ("recovery/rotate", "post", true, json!({}), vec![]),
        (
            "scim-credentials",
            "post",
            true,
            json!({"scopes":{"type":"array","items":{"type":"string"}},"ttl_seconds":{"type":"integer"}}),
            vec!["scopes", "ttl_seconds"],
        ),
        ("scim-credentials/{id}", "delete", true, json!({}), vec![]),
        (
            "identities",
            "post",
            true,
            json!({"provider_id":{"type":"string"},"subject":{"type":"string"},"user_id":{"type":"string"}}),
            vec!["provider_id", "subject", "user_id"],
        ),
    ] {
        let path = format!("/api/auth/enterprise/{suffix}");
        let mut op = json!({"tags":["enterprise identity"],"security":if authenticated {json!([{"browserSession":[]}])}else{json!([])},"parameters":[{"name":"Origin","in":"header","required":true,"schema":{"type":"string","format":"uri"}},{"name":"Sec-Fetch-Site","in":"header","schema":{"type":"string","enum":["same-origin","same-site"]}}],"responses":{"200":{"description":"Challenge, opaque browser session, or mutation result","content":{"application/json":{"schema":object}}},"400":{"description":"Invalid protocol request"},"401":{"description":"Expired, replayed or invalid proof"},"403":{"description":"Origin, CSRF, MFA or authority denied"},"429":{"description":"Rate limited"},"503":{"description":"Identity authority unavailable"}}});
        for name in ["provider", "id"] {
            if suffix.contains(&format!("{{{name}}}")) {
                op["parameters"].as_array_mut().unwrap().push(
                    json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}}),
                );
            }
        }
        if authenticated {
            op["parameters"].as_array_mut().unwrap().push(json!({"name":"X-CSRF-Token","in":"header","required":true,"schema":{"type":"string"}}));
        }
        if !required.is_empty() || suffix == "passkeys/register/start" {
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"type":"object","additionalProperties":false,"properties":properties,"required":required}}}});
        }
        if suffix.ends_with("authenticate/finish")
            || suffix == "recovery"
            || suffix == "passkeys/register/finish"
        {
            op["responses"]["200"]["content"]["application/json"]["schema"] = session.clone();
        }
        op["x-webauthn-object-boundary"] = json!(
            "credential objects follow WebAuthn JSON serialization; attestation/assertion are verified by webauthn-rs, never trusted as claims"
        );
        document["paths"][path][method] = op;
    }
    document["paths"]["/api/auth/enterprise/oidc/callback"]["get"] = json!({"security":[],"parameters":[{"name":"state","in":"query","required":true,"schema":{"type":"string"}},{"name":"code","in":"query","required":true,"schema":{"type":"string"}},{"name":"iss","in":"query","schema":{"type":"string","format":"uri"}}],"description":"Exact GET callback only. Requires independent __Host-agentforge_oidc_flow cookie and one-use state/nonce/PKCE; issuer, audience and signature are verified. SAML terminates at an external OIDC broker.","responses":{"303":{"description":"Opaque browser session cookie and redirect to configured approved return URL"},"401":{"description":"Invalid or replayed flow"}}});
    let error = json!({"type":"object","required":["schemas","status","detail"],"properties":{"schemas":{"type":"array","items":{"const":"urn:ietf:params:scim:api:messages:2.0:Error"}},"status":{"type":"string"},"detail":{"type":"string"}}});
    for (suffix, methods) in [
        ("Users", vec!["get", "post"]),
        ("Users/{id}", vec!["get", "put", "patch", "delete"]),
        ("Groups", vec!["get", "post"]),
        ("Groups/{id}", vec!["get", "put", "patch", "delete"]),
        ("ServiceProviderConfig", vec!["get"]),
        ("Schemas", vec!["get"]),
        ("ResourceTypes", vec!["get"]),
    ] {
        for method in methods {
            let mut op = json!({"tags":["SCIM 2"],"security":[{"enterpriseScim":[]}],"parameters":[{"name":"tenant","in":"path","required":true,"schema":{"type":"string"}}],"responses":{"200":{"description":"SCIM resource or ListResponse","content":{"application/scim+json":{"schema":object}}},"201":{"description":"Created SCIM resource"},"204":{"description":"Deleted (soft deactivated)"}}});
            if suffix.contains("{id}") {
                op["parameters"].as_array_mut().unwrap().push(
                    json!({"name":"id","in":"path","required":true,"schema":{"type":"string"}}),
                );
            }
            if matches!(method, "put" | "patch" | "delete") {
                op["parameters"].as_array_mut().unwrap().push(json!({"name":"If-Match","in":"header","schema":{"type":"string"},"description":"Optional resource ETag; updates always use version CAS"}));
            }
            if matches!(method, "post" | "put" | "patch") {
                op["requestBody"] =
                    json!({"required":true,"content":{"application/scim+json":{"schema":object}}});
            }
            for status in ["400", "401", "403", "404", "409", "412", "429", "503"] {
                op["responses"][status] = json!({"description":"SCIM error","content":{"application/scim+json":{"schema":error}}});
            }
            document["paths"][format!("/scim/v2/{{tenant}}/{suffix}")][method] = op;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_grants_are_opaque_and_scim_is_separately_scoped() {
        let mut d = json!({});
        extend(&mut d);
        assert!(!d.to_string().contains("access_token"));
        assert_eq!(
            d["paths"]["/scim/v2/{tenant}/Users"]["get"]["security"],
            json!([{"enterpriseScim":[]}])
        );
        assert_eq!(
            d["paths"]["/api/auth/enterprise/oidc/callback"]["get"]["responses"]["303"]["description"],
            "Opaque browser session cookie and redirect to configured approved return URL"
        );
    }
}
