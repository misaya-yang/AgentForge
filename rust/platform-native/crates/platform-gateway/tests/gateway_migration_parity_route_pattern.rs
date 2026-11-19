//! All original test_route_pattern.py path/expected fixtures, production breaker key helper.
use ai_platform_gateway::migration_compat::route_pattern::extract_route_pattern as pattern;
#[test]
fn source_uuid_integer_query_fragment_and_known_template_matrix() {
    for (path, expected) in [
        (
            "/api/v1/knowledge/datasets/abc12345-1234-1234-1234-1234567890ab/retrieve",
            "/api/v1/knowledge/datasets/{id}/retrieve",
        ),
        (
            "/api/v1/random/abc12345-1234-1234-1234-1234567890ab/foo",
            "/api/v1/random/{uuid}/foo",
        ),
        (
            "/api/v1/x/abc12345-1234-1234-1234-1234567890ab/y/deadbeef-cafe-babe-face-01234567890a/z",
            "/api/v1/x/{uuid}/y/{uuid}/z",
        ),
        (
            "/api/v1/x/ABC12345-1234-1234-1234-1234567890AB/y",
            "/api/v1/x/{uuid}/y",
        ),
        ("/users/12345/profile", "/users/{id}/profile"),
        ("/api/v1/x/123/y/4567/z", "/api/v1/x/{id}/y/{id}/z"),
        ("/items/42", "/items/{id}"),
        ("/some/v1abc/path", "/some/v1abc/path"),
        ("/health?foo=bar&baz=qux", "/health"),
        (
            "/api/v1/knowledge/datasets/abc12345-1234-1234-1234-1234567890ab/retrieve?top_k=5",
            "/api/v1/knowledge/datasets/{id}/retrieve",
        ),
        ("/health#anchor", "/health"),
        ("/health", "/health"),
        ("/health/live", "/health/live"),
        ("/health/ready", "/health/ready"),
        ("/chat", "/chat"),
        ("/chat/stream", "/chat/stream"),
        ("/sessions", "/sessions"),
        ("/api/v1/assistant/chat", "/chat"),
        ("/api/v1/assistant/chat/stream", "/chat/stream"),
        ("/api/v1/assistant/sessions", "/sessions"),
        ("/api/v1/assistant/health", "/health"),
        ("/api/v1/knowledge/datasets", "/api/v1/knowledge/datasets"),
        (
            "/api/v1/assistant/sessions/abc12345-1234-1234-1234-1234567890ab",
            "/sessions/{id}",
        ),
        (
            "/api/v1/assistant/sessions/abc12345-1234-1234-1234-1234567890ab/messages",
            "/sessions/{id}/messages",
        ),
        (
            "/api/v1/knowledge/datasets/abc12345-1234-1234-1234-1234567890ab/documents",
            "/api/v1/knowledge/datasets/{id}/documents",
        ),
        ("/some/totally/random/path", "/some/totally/random/path"),
        ("/", "/"),
        ("", ""),
        (
            "/api/v1/knowledge/datasets/{id}/retrieve",
            "/api/v1/knowledge/datasets/{id}/retrieve",
        ),
    ] {
        assert_eq!(pattern(path), expected, "{path}");
    }
}
