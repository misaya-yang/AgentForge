use super::*;
use serde_json::json;
#[test]
fn protected_resource_is_exact_and_issuers_are_not_implicitly_trusted_for_fetch() {
    let document = json!({"resource":"https://mcp.example/mcp","authorization_servers":["https://issuer.example"],"bearer_methods_supported":["header"]});
    assert!(validate(&document, "https://mcp.example/mcp").is_ok());
    assert!(validate(&document, "https://mcp.example/other").is_err());
    assert!(
        validate(
            &json!({"resource":"https://mcp.example/mcp","authorization_servers":[]}),
            "https://mcp.example/mcp"
        )
        .is_err()
    );
    assert_eq!(
        resource_indicator("https://mcp.example/mcp").unwrap(),
        ("resource".into(), "https://mcp.example/mcp".into())
    );
}
#[test]
fn path_and_root_well_known_follow_rfc9728_without_cross_origin_credentials() {
    let endpoint = Url::parse("https://mcp.example/public/mcp").unwrap();
    let paths = candidates(&endpoint, None).unwrap();
    assert_eq!(
        paths[0].as_str(),
        "https://mcp.example/.well-known/oauth-protected-resource/public/mcp"
    );
    assert_eq!(
        paths[1].as_str(),
        "https://mcp.example/.well-known/oauth-protected-resource"
    );
    assert!(candidates(&endpoint, Some("https://evil.example/metadata")).is_err());
    assert!(candidates(&endpoint, Some("https://token:secret@mcp.example/metadata")).is_err());
}
#[test]
fn challenge_metadata_is_bounded_and_duplicate_or_insecure_urls_fail() {
    assert_eq!(
        challenge_url(
            "Bearer resource_metadata=\"https://mcp.example/metadata\", scope=\"read,a\""
        )
        .unwrap(),
        Some("https://mcp.example/metadata".into())
    );
    for header in [
        "Bearer resource_metadata=\"http://mcp.example/x\"",
        "Bearer resource_metadata=\"https://mcp.example/x\", resource_metadata=\"https://mcp.example/y\"",
        "Bearer resource_metadata=\"https://mcp.example/x\n\"",
    ] {
        assert!(challenge_url(header).is_err());
    }
}
