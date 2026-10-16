//! Exact source TS authority guards from tests/contract/test_web_stream_auth_contract.py.
//! Browser code remains TypeScript; Rust replaces only its Python-hosted source test.
use std::{fs, path::PathBuf};
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_path_buf()
}
#[test]
fn shared_sse_transports_attach_current_bearer_token() {
    let source = fs::read_to_string(root().join("web/src/lib/sse.ts")).unwrap();
    assert!(source.contains("import { getAuthToken } from \"@/lib/api\""));
    assert!(source.contains("resolved.set(\"Authorization\", `Bearer ${token}`)"));
    assert_eq!(
        source
            .matches("headers: authenticatedHeaders(init.headers)")
            .count(),
        2,
        "both sseFetch and sseFetchEvents must use authenticated headers"
    );
}
#[test]
fn playground_stream_direct_fetch_attaches_current_bearer_token() {
    let source =
        fs::read_to_string(root().join("web/src/pages/playground/hooks/usePlaygroundStream.ts"))
            .unwrap();
    assert!(source.contains("const token = useAuthStore.getState().token;"));
    assert!(source.contains("Authorization: `Bearer ${token}`"));
}
