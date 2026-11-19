use super::*;
#[test]
fn modern_request_metadata_and_http_fields_are_exact_and_stateless() {
    let mut payload = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"weather","arguments":{"city":"private"}}});
    let headers = Revision::Modern2026.decorate(&mut payload).unwrap();
    assert_eq!(headers["mcp-protocol-version"], "2026-07-28");
    assert_eq!(headers["mcp-method"], "tools/call");
    assert_eq!(headers["mcp-name"], "weather");
    assert_eq!(
        payload["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        "2026-07-28"
    );
    assert_eq!(payload["params"]["arguments"], json!({"city":"private"}));
    assert!(!headers.contains_key("mcp-session-id"));
    let mut bad = payload;
    bad["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2025-11-25");
    assert!(Revision::Modern2026.decorate(&mut bad).is_err());
}
#[test]
fn versions_and_known_modern_errors_never_downgrade_or_replay() {
    assert!(Revision::parse("2025-03-26").is_err());
    assert!(Revision::parse("latest").is_err());
    let discovery = json!({"resultType":"complete","supportedVersions":["2026-07-28","2025-11-25"],"capabilities":{"tools":{}}});
    assert!(Revision::Modern2026.validate_discovery(&discovery).is_ok());
    assert!(Revision::Modern2026.validate_discovery(&json!({"resultType":"complete","supportedVersions":["2025-11-25"],"capabilities":{}})).is_err());
    assert_eq!(
        classify_error(&json!({"error":{"code":-32022,"data":{"supported":["2025-11-25"]}}})),
        Some("MCP_PROTOCOL_DOWNGRADE_REJECTED")
    );
    assert_eq!(
        classify_error(&json!({"error":{"code":-32020}})),
        Some("MCP_HEADER_BODY_MISMATCH")
    );
    let mut session = HeaderMap::new();
    session.insert("mcp-session-id", HeaderValue::from_static("untrusted"));
    assert!(Revision::Modern2026.check_session(&session).is_err());
    assert!(Revision::Legacy2025.check_session(&session).is_ok());
    assert!(
        Revision::Modern2026
            .validate_result(&json!({"resultType":"input_required"}))
            .is_err()
    );
}
#[test]
fn legacy_contract_is_not_rewritten() {
    let mut body = json!({"method":"initialize","params":{"protocolVersion":"2025-11-25"}});
    let before = body.clone();
    let headers = Revision::Legacy2025.decorate(&mut body).unwrap();
    assert_eq!(body, before);
    assert_eq!(headers.len(), 1);
    assert_eq!(headers["mcp-protocol-version"], "2025-11-25");
}
#[test]
fn header_encoding_uses_the_official_sentinel() {
    assert_eq!(
        encode_header(" padded ").unwrap(),
        "=?base64?IHBhZGRlZCA=?="
    );
    assert_eq!(
        encode_header("line1\nline2").unwrap(),
        "=?base64?bGluZTEKbGluZTI=?="
    );
    assert_eq!(encode_header("weather").unwrap(), "weather");
    assert_ne!(
        encode_header("=?base64?literal?=").unwrap(),
        "=?base64?literal?="
    );
}
#[test]
fn tool_parameter_mirroring_rejects_dynamic_paths_collisions_and_unsafe_integers() {
    let schema = json!({"type":"object","properties":{"nested":{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}}}}});
    let headers = tool_headers(&schema, &json!({"nested":{"region":" padded "}})).unwrap();
    assert_eq!(headers["mcp-param-region"], "=?base64?IHBhZGRlZCA=?=");
    for schema in [
        json!({"allOf":[{"properties":{"region":{"type":"string","x-mcp-header":"Region"}}}]}),
        json!({"properties":{"a":{"type":"string","x-mcp-header":"Same"},"b":{"type":"string","x-mcp-header":"same"}}}),
        json!({"properties":{"a":{"type":"number","x-mcp-header":"A"}}}),
        json!({"properties":{"a":{"type":"string","x-mcp-header":"bad\nname"}}}),
    ] {
        assert!(tool_headers(&schema, &json!({})).is_err());
    }
    assert!(
        tool_headers(
            &json!({"properties":{"a":{"type":"integer","x-mcp-header":"A"}}}),
            &json!({"a":9_007_199_254_740_992i64})
        )
        .is_err()
    );
}
