use super::*;
fn fixture() -> Value {
    json!({
        "openapi":"3.2.0","info":{"title":"SDK codegen fixture","version":"1.0.0"},
        "paths":{
            "/items/{id}":{"$ref":"#/components/pathItems/Item"},
            "/events":{"get":{"responses":{"200":{"description":"SSE","content":{"text/event-stream":{"itemSchema":{"type":"object","required":["event"],"additionalProperties":false,"properties":{"event":{"type":"string"},"data":{"$ref":"#/components/schemas/Extended"}}}}}}}}},
            "/empty":{"delete":{"responses":{"204":{"description":"Deleted"}}}}
        },
        "components":{
            "schemas":{
                "Base":{"type":"object","required":["id"],"properties":{"id":{"type":"string"}}},
                "Extended":{"allOf":[{"$ref":"#/components/schemas/Base"},{"type":"object","required":["active"],"properties":{"active":{"type":"boolean"},"label":{"type":"string","nullable":true}}}]},
                "Tree":{"type":"object","required":["children"],"additionalProperties":false,"properties":{"children":{"type":"array","items":{"$ref":"#/components/schemas/Tree"}}}},
                "A-B":{"type":"string"},"A_B":{"type":"integer"},"Never":false,
                "User.Profile":{"type":"object","additionalProperties":false,"properties":{"type":{"type":"string"},"A-B":{"type":"string"},"A_B":{"type":"integer"},"quote\"\n\u{0001}":{"enum":["x\n\u{0001}"]}}}
            },
            "parameters":{"Id":{"name":"id","in":"path","required":true,"schema":{"type":"string"}}},
            "requestBodies":{"Update":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Extended"}}}}},
            "responses":{"Updated":{"description":"Updated item","content":{"application/json":{"schema":{"$ref":"#/components/schemas/Extended"}}}}},
            "pathItems":{"Item":{"parameters":[{"$ref":"#/components/parameters/Id"},{"name":"filter","in":"query","required":true,"schema":{"type":"string"}}],"put":{"parameters":[{"name":"filter","in":"query","required":false,"schema":{"type":"integer"}}],"requestBody":{"$ref":"#/components/requestBodies/Update"},"responses":{"200":{"$ref":"#/components/responses/Updated"},"400":{"description":"Problem","content":{"application/json":{"schema":{"type":"object","required":["message"],"properties":{"message":{"type":"string"}},"additionalProperties":false}}}}}}}}
        }
    })
}
fn scratch(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "agentforge-codegen-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}
#[test]
fn deterministic_complete_models_resolve_allof_and_keep_stream_items() {
    let document = fixture();
    let files = generate(&document).unwrap();
    assert_eq!(files, generate(&document).unwrap());
    let ts = &files["typescript/generated.ts"];
    let py = &files["python/generated_models.py"];
    let rust = &files["rust/generated_models.rs"];
    assert!(ts.contains("\"PUT /items/{id}\""));
    assert!(ts.contains("\"id\": string"));
    assert!(ts.contains("\"filter\"?: number"));
    assert!(ts.contains("\"label\"?: (string | null)"));
    assert!(ts.contains("\"event\": string"));
    assert!(ts.contains("stream: true"));
    assert!(ts.contains("response: ApiDeleteApiEmptyResponse"));
    assert!(ts.contains("= void;"));
    for body in [ts, py, rust] {
        assert!(!body.contains("https://"));
        assert!(!body.contains("fetch("));
        assert!(!body.contains("Self.Models"));
    }
    assert!(py.contains("\"id\": Required[str]"));
    assert!(py.contains("\"active\": Required[bool]"));
    assert!(py.contains("\"label\": NotRequired[Union[str, None]]"));
    assert!(py.contains("\"message\": Required[str]"));
    assert!(py.contains("AsyncIterator["));
    assert!(py.contains("self._transport.request"));
    assert!(rust.contains("pub field_message: String"));
    assert!(rust.contains("pub field_children: Vec<Box<ApiTree>>"));
    assert!(rust.contains("pub enum ApiNever {}"));
    assert!(!rust.contains("Infallible"));
    let manifest: Value = serde_json::from_str(&files["generated-manifest.json"]).unwrap();
    assert_eq!(manifest["operations"], 3);
    assert_eq!(manifest["stream_operations"], json!(["GET /events"]));
}
#[test]
fn generated_rust_syntax_and_identifier_uniqueness_are_valid() {
    let files = generate(&fixture()).unwrap();
    let syntax = syn::parse_file(&files["rust/generated_models.rs"]).unwrap();
    let mut ids = BTreeSet::new();
    for item in syntax.items {
        let id = match item {
            syn::Item::Struct(v) => v.ident.to_string(),
            syn::Item::Enum(v) => v.ident.to_string(),
            syn::Item::Type(v) => v.ident.to_string(),
            _ => continue,
        };
        assert!(ids.insert(id.clone()), "duplicate Rust item {id}");
    }
    assert!(ids.contains("ApiAB"));
    assert!(ids.iter().any(|id| id.starts_with("ApiAB_")));
    assert!(ids.iter().any(|id| id.starts_with("ApiNever_")));
}
#[test]
fn schema_reference_pointer_escaping_and_nesting_fail_closed() {
    let mut document = fixture();
    document["x-models"] = json!({"User/Profile":{"type":"string"}});
    let mut types = Types::new(&document);
    assert!(
        types
            .reference(&json!({"$ref":"#/x-models/User~1Profile"}))
            .unwrap()
            .is_some()
    );
    for reference in [
        "https://example.org/schema",
        "#/components/schemas/Absent",
        "#/components/schemas/Bad~2Name",
        "#Anchor",
    ] {
        assert!(types.reference(&json!({"$ref":reference})).is_err());
    }
    assert!(types.ty(&json!({}), "nested", "Nested", 0, 65).is_err());
    assert!(
        types
            .ty(&json!({"enum":[]}), "empty", "Empty", 0, 0)
            .is_err()
    );
    assert!(
        types
            .ty(&json!({"allOf":[]}), "empty", "Empty", 0, 0)
            .is_err()
    );
    assert_eq!(
        fragment("#/components/schemas/User%7E1Profile").unwrap(),
        "/components/schemas/User~1Profile"
    );
}
#[test]
fn allof_ref_siblings_and_conflicts_are_not_silently_discarded() {
    let document = fixture();
    let schema=merge_objects(&document,&[json!({"$ref":"#/components/schemas/Base","properties":{"extra":{"type":"integer"}},"required":["extra"]})],&mut BTreeSet::new(),0).unwrap();
    assert!(schema["properties"].get("id").is_some());
    assert!(schema["properties"].get("extra").is_some());
    assert_eq!(schema["required"], json!(["extra", "id"]));
    assert!(
        merge_objects(
            &document,
            &[
                json!({"properties":{"id":{"type":"string"}}}),
                json!({"properties":{"id":{"type":"integer"}}})
            ],
            &mut BTreeSet::new(),
            0
        )
        .is_err()
    );
    let mut cyclic = document.clone();
    cyclic["components"]["schemas"]["Loop"] =
        json!({"allOf":[{"$ref":"#/components/schemas/Loop"}]});
    assert!(generate(&cyclic).is_err());
}
#[test]
fn check_detects_modified_and_missing_artifacts_without_writing() {
    let root = scratch("check");
    let files = generate(&fixture()).unwrap();
    assert!(write_or_check(&root, &files, true).is_err());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    write_or_check(&root, &files, false).unwrap();
    write_or_check(&root, &files, true).unwrap();
    let target = root.join("python/generated_models.py");
    fs::write(&target, "stale\n").unwrap();
    assert!(
        write_or_check(&root, &files, true)
            .unwrap_err()
            .contains("python/generated_models.py")
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "stale\n");
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn offline_relative_references_preserve_lexical_scope_and_block_escape() {
    let root = scratch("refs");
    let entry = root.join("candidate.json");
    let mut document = fixture();
    document["components"]["schemas"]["External"] = json!({"$ref":"models.json#/User"});
    fs::write(&entry, document.to_string()).unwrap();
    fs::write(root.join("models.json"),json!({"User":{"type":"object","properties":{"children":{"type":"array","items":{"$ref":"#/User"}}}}}).to_string()).unwrap();
    let bundled = read_candidate(&entry).unwrap();
    let reference = &bundled["components"]["schemas"]["External"];
    let user = resolve(&bundled, reference).unwrap();
    let child = &user["properties"]["children"]["items"];
    assert_eq!(resolve(&bundled, child).unwrap(), user);
    generate(&bundled).unwrap();
    for raw in [
        "../outside.json#/User",
        "https://example.org/model.json#/User",
        "/tmp/model.json#/User",
        "models%2ejson#/User",
    ] {
        document["components"]["schemas"]["External"] = json!({"$ref":raw});
        fs::write(&entry, document.to_string()).unwrap();
        assert!(read_candidate(&entry).is_err());
    }
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn cli_options_are_strict_and_candidate_check_works_offline() {
    let args = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert!(arguments(&args(&["--out", "sdk", "--out", "other"])).is_err());
    assert!(arguments(&args(&["--out"])).is_err());
    assert!(arguments(&args(&["--out", "sdk", "--unknown"])).is_err());
    let root = scratch("cli");
    let candidate = root.join("fixture.json");
    fs::write(&candidate, fixture().to_string()).unwrap();
    let output = root.join("sdk");
    let args = vec![
        "--out".into(),
        output.to_string_lossy().into_owned(),
        "--candidate".into(),
        candidate.to_string_lossy().into_owned(),
    ];
    assert_eq!(run(&args, None), 0);
    let mut check = args.clone();
    check.push("--check".into());
    assert_eq!(run(&check, None), 0);
    fs::write(output.join("typescript/generated.ts"), "changed").unwrap();
    assert_eq!(run(&check, None), 2);
    fs::remove_dir_all(root).unwrap();
}
/// Root may run this after the serialized native-container build. It compiles
/// actual generated fixture code with already-installed host tools only.
#[test]
#[ignore = "requires installed python3 and TypeScript tsc; no downloads"]
fn generated_python_and_typescript_execute_with_the_caller_transport() {
    let root = scratch("languages");
    let files = generate(&fixture()).unwrap();
    write_or_check(&root, &files, false).unwrap();
    let python = root.join("python/generated_models.py");
    let test = root.join("python/check.py");
    fs::write(&test,"import generated_models as sdk\nimport typing\nassert typing.get_type_hints(sdk.ApiExtended)['id'] is str\nassert typing.get_type_hints(sdk.ApiExtended)['active'] is bool\nclass Transport:\n    async def request(self, operation, request):\n        assert operation == 'DELETE /empty'\n        return None\n    def stream(self, operation, request):\n        async def events():\n            yield {'event': 'done'}\n        return events()\nassert sdk.Client(Transport())\n").unwrap();
    let result = std::process::Command::new("python3")
        .arg(&test)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(python.exists());
    let tsc = std::env::var("AGENTFORGE_TSC").unwrap_or_else(|_| "tsc".into());
    let result = std::process::Command::new(tsc)
        .args([
            "--noEmit",
            "--strict",
            "--target",
            "ES2022",
            "--module",
            "ESNext",
            "--lib",
            "ES2022,DOM",
            "--skipLibCheck",
            "--erasableSyntaxOnly",
        ])
        .arg(root.join("typescript/generated.ts"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn oas32_query_custom_methods_and_media_discriminants_are_generated() {
    let mut document = fixture();
    document["paths"]["/search"] = json!({"query":{"parameters":[{"name":"search","in":"querystring","content":{"application/json":{"schema":{"type":"object","properties":{"q":{"type":"string"}}}}}}],"responses":{"200":{"description":"OK"}}},"additionalOperations":{"REPORT":{"responses":{"200":{"description":"OK","content":{"application/json":{"schema":{"type":"string"}}}}}}}});
    document["paths"]["/input"] = json!({"post":{"requestBody":{"required":true,"content":{"application/json":{"schema":{"type":"integer"}},"text/plain":{"schema":{"type":"string"}}}},"responses":{"204":{"description":"OK"}}}});
    let files = generate(&document).unwrap();
    let ts = &files["typescript/generated.ts"];
    assert!(ts.contains("\"QUERY /search\""));
    assert!(ts.contains("\"REPORT /search\""));
    assert!(ts.contains("\"querystring\"?:"));
    assert!(ts.contains("\"content_type\": \"application/json\""));
    assert!(ts.contains("\"content_type\": \"text/plain\""));
    assert!(ts.contains("\"body\": number"));
    assert!(ts.contains("\"body\": string"));
}

#[test]
fn binary_media_and_multipart_use_native_binary_types_and_compact_runtime_metadata() {
    let mut d = fixture();
    d["paths"]["/files"]["post"] = json!({"requestBody":{"required":true,"content":{"multipart/form-data":{"schema":{"type":"object","required":["file"],"properties":{"file":{"type":"string","format":"binary"}}}}}},"responses":{"200":{"description":"Uploaded"}}});
    d["paths"]["/files/{id}"]["get"] = json!({"parameters":[{"in":"path","name":"id","required":true,"schema":{"type":"string"}}],"responses":{"200":{"description":"Bytes","content":{"application/octet-stream":{"schema":{"type":"string","format":"binary"}}}}}});
    let files = generate(&d).unwrap();
    assert!(files["typescript/generated.ts"].contains("\"file\": Blob"));
    assert!(files["python/generated_models.py"].contains("\"file\": Required[bytes]"));
    assert!(files["rust/generated_models.rs"].contains("pub field_file: Vec<u8>"));
    let ts = &files["typescript/generated.ts"];
    let compact = ts
        .split("export const binaryOperations = ")
        .nth(1)
        .unwrap()
        .split(" as const;")
        .next()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(compact).unwrap(),
        json!(["GET /files/{id}"])
    );
    let compact = ts
        .split("export const nonJsonRequestMediaTypes = ")
        .nth(1)
        .unwrap()
        .split(" as const;")
        .next()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(compact).unwrap(),
        json!({"POST /files":["multipart/form-data"]})
    );
}

#[test]
fn sdk_emits_the_fetch_transport_and_operation_typed_query_factories() {
    let output = generate(&fixture()).unwrap();
    let ts = &output["typescript/generated.ts"];
    assert!(ts.contains("export function createFetchTransport<Ops"));
    assert!(ts.contains("export const operationKeys = ["));
    assert!(ts.contains("export function queryOptions<K extends Extract<UnaryOperation"));
    assert!(ts.contains("client.request(operation, input, signal)"));
    assert!(!ts.contains("localStorage"));
    assert!(!ts.contains("Authorization:"));
}

#[test]
fn console_contract_event_requests_generate_in_all_three_sdks() {
    // Project two actual authored operations for generator coverage. The shared
    // live/offline extension tests separately verify mounted query enrichment.
    let catalog: Value =
        serde_json::from_str(include_str!("console_contract/catalog.json")).unwrap();
    let mut d = catalog.clone();
    for key in [
        "GET /api/v1/local-nodes/{device_id}/events",
        "GET /api/v2/agent/threads/{thread_id}/events",
    ] {
        let (_, path) = key.split_once(' ').unwrap();
        for (name, field) in catalog["query_parameters"][key]["parameters"]
            .as_object()
            .unwrap()
        {
            let parameters = d["paths"][path]["get"]["parameters"]
                .as_array_mut()
                .unwrap();
            if let Some(parameter) = parameters
                .iter_mut()
                .find(|p| p["name"] == name.as_str() && p["in"] == "query")
            {
                parameter["schema"] = field["schema"].clone();
                parameter["required"] = field["required"].clone();
            } else {
                parameters.push(json!({"name":name,"in":"query","required":field["required"],"schema":field["schema"]}));
            }
        }
    }
    let local = "/api/v1/local-nodes/{device_id}/events";
    let v2 = "/api/v2/agent/threads/{thread_id}/events";
    let mut input = json!({
        "openapi":"3.2.0", "info":{"title":"Native event owner projection", "version":"1.0.0"},
        "paths":{local:d["paths"][local], v2:d["paths"][v2]},
        "components":{"schemas":{"ConsoleAgentV2Event":d["components"]["schemas"]["ConsoleAgentV2Event"]},
            "securitySchemes":{"browserSession":{"type":"apiKey","in":"cookie","name":"__Host-agentforge_session"},
                "NativeBearerAuth":{"type":"http","scheme":"bearer"}}}
    });
    crate::stream_schema::upgrade(&mut input);
    let generated = crate::codegen::generate(&input).unwrap();
    let ts = &generated["typescript/generated.ts"];
    let request_for = |path: &str| {
        let operation = ts
            .lines()
            .find(|line| line.starts_with(&format!("  \"GET {path}\": {{ request: ")))
            .unwrap();
        let model = operation
            .split_once("request: ")
            .unwrap()
            .1
            .split(';')
            .next()
            .unwrap();
        ts.lines()
            .find(|line| line.starts_with(&format!("export type {model} =")))
            .unwrap()
    };
    let local_request = request_for(local);
    assert!(local_request.contains("\"after_sequence\"?: number"));
    for ignored in ["limit", "before_sequence", "cursor", "Last-Event-ID"] {
        assert!(!local_request.contains(&format!("\"{ignored}\"")));
    }
    let v2_request = request_for(v2);
    assert!(v2_request.contains("\"limit\"?: number"));
    assert!(v2_request.contains("\"Last-Event-ID\"?: string"));
    assert!(
        generated["python/generated_models.py"].contains("\"Last-Event-ID\": NotRequired[str]")
    );
    assert!(generated["rust/generated_models.rs"].contains("rename = \"Last-Event-ID\""));
    assert!(generated["rust/generated_models.rs"].contains("field_last_event_id"));
}
