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
        1,
        "the shared byte reader must resolve authenticated headers for every dispatch"
    );
    for (start, end) in [
        (
            "export async function* sseFetch<",
            "export async function* sseFetchEvents<",
        ),
        (
            "export async function* sseFetchEvents<",
            "export async function* sseFetchAGUI(",
        ),
    ] {
        let transport = source
            .split_once(start)
            .unwrap()
            .1
            .split_once(end)
            .unwrap()
            .0;
        assert!(
            transport.contains("readSseParts(url,"),
            "{start} must use the authenticated reader"
        );
        assert!(
            !transport.contains("sendStreamRequest("),
            "{start} must not bypass the shared reader"
        );
    }
    let agui = source
        .split_once("export async function* sseFetchAGUI(")
        .unwrap()
        .1;
    assert!(agui.contains("readSseParts(url, init)"));
    assert!(!agui.contains("sendStreamRequest("));
}
#[test]
fn playground_stream_direct_fetch_attaches_current_bearer_token() {
    let source =
        fs::read_to_string(root().join("web/src/pages/playground/hooks/usePlaygroundStream.ts"))
            .unwrap();
    assert!(source.contains("const token = useAuthStore.getState().token;"));
    assert!(source.contains("Authorization: `Bearer ${token}`"));
}
