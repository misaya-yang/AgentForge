use super::*;
struct Wire {
    seen: Mutex<Vec<(HeaderMap, Value)>>,
    mode: u8,
}
impl McpTransport for Wire {
    fn post(&self, request: McpHttpRequest) -> McpFuture<'_, McpHttpResponse> {
        Box::pin(async move {
            let method = request.payload["method"].as_str().unwrap().to_owned();
            let id = request.payload["id"].clone();
            self.seen
                .lock()
                .unwrap()
                .push((request.headers, request.payload));
            let (status, body) = if method == "notifications/initialized" {
                (202, vec![])
            } else {
                let result = match method.as_str() {
                    "server/discover" => {
                        json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}}})
                    }
                    "initialize" => json!({"protocolVersion":"2025-11-25","serverInfo":{}}),
                    "tools/list" => {
                        json!({"resultType":"complete","tools":[{"name":"weather","inputSchema":{"type":"object"}}]})
                    }
                    _ => panic!("unexpected RPC"),
                };
                let value = if self.mode == 1 || self.mode == 3 {
                    json!({"jsonrpc":"2.0","id":if self.mode==3{Value::String("foreign-request".into())}else{id},"error":{"code":-32022,"data":{"supported":["2025-11-25"]}}})
                } else {
                    json!({"jsonrpc":"2.0","id":id,"result":result})
                };
                (
                    if self.mode == 1 || self.mode == 3 {
                        400
                    } else {
                        200
                    },
                    serde_json::to_vec(&value).unwrap(),
                )
            };
            let mut headers = HeaderMap::new();
            headers.insert("content-type", HeaderValue::from_static("application/json"));
            if self.mode == 2 {
                headers.insert("mcp-session-id", HeaderValue::from_static("must-not-pin"));
            }
            Ok(McpHttpResponse {
                status,
                headers,
                body,
            })
        })
    }
}
fn session(version: &str, mode: u8) -> (Arc<McpSession>, Arc<Wire>) {
    let config = McpClientConfig::from_record(
        &json!({"base_url":"https://mcp.example","protocol_version":version}),
        None,
    )
    .unwrap();
    let wire = Arc::new(Wire {
        seen: Mutex::new(vec![]),
        mode,
    });
    let session = Arc::new(McpSession {
        config,
        wire: Mutex::new(Some(wire.clone())),
        session: Arc::new(Mutex::new(None)),
    });
    (session, wire)
}
#[tokio::test]
async fn both_revisions_run_their_actual_production_state_machine() {
    for version in ["2025-11-25", "2026-07-28"] {
        let (session, wire) = session(version, 0);
        let initialized = session.initialize().await.unwrap();
        assert_eq!(initialized.protocol_version.as_deref(), Some(version));
        let tools = initialized.client.list_tools().await.unwrap();
        assert_eq!(tools[0]["name"], "weather");
        let seen = wire.seen.lock().unwrap();
        let methods = seen
            .iter()
            .map(|(_, p)| p["method"].as_str().unwrap())
            .collect::<Vec<_>>();
        if version == "2026-07-28" {
            assert_eq!(methods, vec!["server/discover", "tools/list"]);
            for (h, p) in seen.iter() {
                assert_eq!(h["mcp-protocol-version"], version);
                assert_eq!(h["mcp-method"], p["method"].as_str().unwrap());
                assert!(!h.contains_key("mcp-session-id"));
                assert_eq!(
                    p["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
                    version
                );
            }
        } else {
            assert_eq!(
                methods,
                vec!["initialize", "notifications/initialized", "tools/list"]
            );
        }
    }
}
#[tokio::test]
async fn recognized_modern_error_and_session_confusion_never_trigger_legacy_or_tool_replay() {
    for mode in [1, 2, 3] {
        let (session, wire) = session("2026-07-28", mode);
        assert!(session.initialize().await.is_err());
        let seen = wire.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1["method"], "server/discover");
    }
}
#[test]
fn protocol_revision_and_metadata_resource_have_separate_cache_identity() {
    let item = json!({"connection_id":"connection","base_url":"https://mcp.example","auth_method":"none","protocol_version":"2025-11-25"});
    let a = crate::mcp_gateway_broker::mcp_client_cache_key(&item, "tenant").unwrap();
    let mut modern = item.clone();
    modern["protocol_version"] = json!("2026-07-28");
    let b = crate::mcp_gateway_broker::mcp_client_cache_key(&modern, "tenant").unwrap();
    assert_ne!(a, b);
    modern["oauth_metadata_url"] = json!("https://mcp.example/metadata");
    let c = crate::mcp_gateway_broker::mcp_client_cache_key(&modern, "tenant").unwrap();
    assert_ne!(b, c);
    modern["protocol_version"] = json!("1900-01-01");
    assert!(crate::mcp_gateway_broker::mcp_client_cache_key(&modern, "tenant").is_err());
}
#[test]
fn gateway_stdio_cannot_spawn_without_sandbox_control() {
    assert!(
        McpClientConfig::from_record(
            &json!({"base_url":"https://mcp.example","transport":"stdio"}),
            None
        )
        .is_err()
    );
}
