//! Console contracts from mounted native owners. The authored route/validator
//! declarations below supplement dispatcher routes; serde DTOs are extracted
//! from embedded owner source, never a published SDK/OpenAPI snapshot.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
mod rust_schema;
mod sources;
const CATALOG: &str = include_str!("catalog.json");
fn normalized(path: &str) -> String {
    static TEMPLATE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    TEMPLATE
        .get_or_init(|| regex::Regex::new(r"\{[^}]+\}").unwrap())
        .replace_all(path, "{}")
        .into_owned()
}
fn parameter_names(path: &str) -> Vec<String> {
    static PARAMETERS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PARAMETERS
        .get_or_init(|| regex::Regex::new(r"\{([^}]+)\}").unwrap())
        .captures_iter(path)
        .map(|m| m[1].to_owned())
        .collect()
}
fn dto(schema: &Value) -> Value {
    let Some(input) = schema.get("x-console-rust-schema") else {
        return schema.clone();
    };
    let source_text: Vec<_> = input["sources"]
        .as_array()
        .expect("DTO sources")
        .iter()
        .map(|name| {
            let name = name.as_str().expect("source name");
            sources::SOURCES
                .iter()
                .find(|(n, _)| *n == name)
                .unwrap_or_else(|| panic!("unembedded Console DTO source {name}"))
                .1
        })
        .collect();
    rust_schema::Registry::new(&source_text).schema(input["type"].as_str().expect("DTO type"))
}
fn opaque(schema: &Value) -> bool {
    schema.is_null()
        || schema == &Value::Bool(true)
        || schema.as_object().is_some_and(|s| {
            s.is_empty()
                || (s.get("type").is_some_and(|v| v == "object") && s.get("properties").is_none())
        })
}
pub fn extend_document(document: &mut Value) {
    let catalog: Value = serde_json::from_str(CATALOG).expect("source-owned Console catalog JSON");
    for (name, schema) in catalog["components"]["schemas"]
        .as_object()
        .expect("schemas")
    {
        document["components"]["schemas"][name] = dto(schema);
    }
    // Evaluation's JSON parser promotes presets into RetrieveRequest. Reuse
    // its actual optional wire fields; the query is synthesized server-side.
    for (name, schema) in catalog["components"]["schemas"].as_object().unwrap() {
        if let Some(base) = schema["x-console-optional-fields-from"].as_str() {
            let properties = document["components"]["schemas"][base]["properties"]
                .as_object()
                .expect("base DTO properties")
                .clone();
            let target = document["components"]["schemas"][name]["properties"]
                .as_object_mut()
                .expect("extended DTO properties");
            for (key, value) in properties {
                if let Some(existing) = target.get_mut(&key) {
                    if key != "query" && existing != &value {
                        *existing = json!({"anyOf":[existing.clone(),value]});
                    }
                } else {
                    target.insert(key, value);
                }
            }
            document["components"]["schemas"][name]
                .as_object_mut()
                .unwrap()
                .remove("x-console-optional-fields-from");
        }
    }
    let mut count = 0usize;
    for (declared, methods) in catalog["paths"].as_object().expect("paths") {
        // Existing owner contracts choose canonical parameter names. Do not add
        // ambiguous duplicate templates merely because {id} and {dataset_id}
        // were spelled differently in two actual routers.
        let canonical = document["paths"]
            .as_object()
            .and_then(|paths| {
                paths
                    .keys()
                    .find(|path| normalized(path) == normalized(declared))
            })
            .cloned()
            .unwrap_or_else(|| declared.clone());
        let from = parameter_names(declared);
        let to = parameter_names(&canonical);
        for (method, original) in methods.as_object().expect("methods") {
            let mut operation = original.clone();
            if let Some(params) = operation["parameters"].as_array_mut() {
                for parameter in params {
                    if parameter["in"] == "path" {
                        if let Some(index) = from
                            .iter()
                            .position(|name| parameter["name"] == name.as_str())
                        {
                            parameter["name"] = json!(to[index]);
                        }
                    }
                }
            }
            let current = &mut document["paths"][&canonical][method];
            if current.is_null() {
                *current = operation;
                count += 1;
                continue;
            }
            // Strong existing status/media/schema declarations and authentication
            // alternatives always remain in place. This extension fills missing
            // request models and opaque dispatcher success declarations only.
            if (current.get("requestBody").is_none()
                || current["requestBody"]["content"]
                    .as_object()
                    .is_some_and(|content| content.values().all(|m| opaque(&m["schema"]))))
                && operation.get("requestBody").is_some()
            {
                current["requestBody"] = operation["requestBody"].clone();
            }
            let parameters = current["parameters"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut merged = parameters;
            for parameter in operation["parameters"].as_array().into_iter().flatten() {
                if !merged
                    .iter()
                    .any(|p| p["name"] == parameter["name"] && p["in"] == parameter["in"])
                {
                    merged.push(parameter.clone());
                }
            }
            current["parameters"] = json!(merged);
            if current.get("description").is_none() && operation.get("description").is_some() {
                // A canonical dispatcher placeholder must still expose the
                // source owner's observation/resume semantics.
                current["description"] = operation["description"].clone();
            }
            if operation["x-native-declaration"]["handler"] == "event_response"
                || current["responses"]
                    .as_object()
                    .is_none_or(|responses| !responses.keys().any(|status| status.starts_with('2')))
                || current["responses"].as_object().is_some_and(|responses| {
                    let successes: Vec<_> = responses
                        .iter()
                        .filter(|(status, _)| status.starts_with('2'))
                        .map(|(_, response)| response)
                        .collect();
                    !successes.is_empty()
                        && successes.iter().all(|response| {
                            response["content"].as_object().is_some_and(|content| {
                                !content.is_empty()
                                    && content.values().all(|media| {
                                        opaque(&media["schema"])
                                            && media.get("itemSchema").is_none()
                                    })
                            })
                        })
                })
                || operation["responses"].as_object().is_some_and(|responses| {
                    responses.values().any(|r| {
                        r["content"].as_object().is_some_and(|media| {
                            media.values().any(|m| m["schema"]["format"] == "binary")
                        })
                    })
                })
            {
                // Binary byte handlers cannot be described as opaque JSON.
                if let Some(responses) = current["responses"].as_object_mut() {
                    responses.retain(|status, _| !status.starts_with('2'));
                }
                for (status, response) in operation["responses"].as_object().unwrap() {
                    if status != "default" {
                        current["responses"][status] = response.clone();
                    }
                }
            }
            current["x-console-owner-declaration"] = operation["x-native-declaration"].clone();
        }
    }
    // Query fields come from mounted native parser/serde owners. Enrich an
    // existing operation only: a filter declaration can never invent a route.
    for (key, declaration) in catalog["query_parameters"]
        .as_object()
        .expect("query declarations")
    {
        let (method, path) = key.split_once(' ').expect("query operation key");
        let canonical = document["paths"]
            .as_object()
            .unwrap()
            .keys()
            .find(|candidate| normalized(candidate) == normalized(path))
            .cloned()
            .unwrap_or_else(|| panic!("query owner has no mounted route: {key}"));
        let operation = &mut document["paths"][&canonical][method.to_ascii_lowercase()];
        assert!(
            operation.is_object(),
            "query owner has no mounted method: {key}"
        );
        let mut parameters = operation["parameters"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for (name, field) in declaration["parameters"].as_object().expect("query fields") {
            if let Some(existing) = parameters
                .iter_mut()
                .find(|p| p["in"] == "query" && p["name"] == name.as_str())
            {
                existing["schema"] = field["schema"].clone();
                existing["required"] = field["required"].clone();
            } else {
                parameters.push(json!({"in":"query", "name":name, "required":field["required"], "schema":field["schema"]}));
            }
        }
        operation["parameters"] = json!(parameters);
        operation["x-console-query-owner-sources"] = declaration["sources"].clone();
    }
    for (name, text) in sources::SOURCES {
        document["x-native-contract"]["common_source_sha256"][*name] =
            json!(format!("{:x}", Sha256::digest(text.as_bytes())));
    }
    document["x-native-contract"]["common_source_sha256"]["platform-openapi/src/console_contract/catalog.json"] =
        json!(format!("{:x}", Sha256::digest(CATALOG.as_bytes())));
    document["x-native-console-contract"] = json!({"schema":"native-console-contract/v1","declared_operations":catalog["paths"].as_object().unwrap().values().map(|v|v.as_object().unwrap().len()).sum::<usize>(),"added_operations":count,"request_model_source":"actual serde owner DTOs and explicit native JSON validators","published_schema_input":false});
}
#[cfg(test)]
mod tests {
    use super::*;
    fn seed_query_routes(d: &mut Value) {
        let catalog: Value = serde_json::from_str(CATALOG).unwrap();
        for key in catalog["query_parameters"].as_object().unwrap().keys() {
            let (method, path) = key.split_once(' ').unwrap();
            let canonical = d["paths"]
                .as_object()
                .unwrap()
                .keys()
                .find(|candidate| normalized(candidate) == normalized(path))
                .cloned()
                .unwrap_or_else(|| path.into());
            if d["paths"][&canonical][method.to_ascii_lowercase()].is_null() {
                d["paths"][&canonical][method.to_ascii_lowercase()] = json!({});
            }
        }
    }
    fn document() -> Value {
        // Embedded native source is immutable within this test binary. Each
        // assertion gets its own clone; avoid reparsing every owner DTO for
        // every test or multiplying that work in the Gateway embedding.
        static DOCUMENT: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
        DOCUMENT
            .get_or_init(|| {
                let mut d = json!({"paths":{},"components":{"schemas":{}}});
                seed_query_routes(&mut d);
                extend_document(&mut d);
                d
            })
            .clone()
    }
    #[test]
    fn every_console_owner_dto_resolves_without_an_unknown_request_fallback() {
        let d = document();
        assert!(d["paths"].as_object().unwrap().len() > 150);
        for (path, item) in d["paths"].as_object().unwrap() {
            for (method, op) in item.as_object().unwrap() {
                for schema in op["requestBody"]["content"]
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.values())
                    .map(|m| &m["schema"])
                {
                    assert!(
                        schema.get("type").is_some() || schema.get("$ref").is_some(),
                        "{method} {path} lacks a real body model"
                    );
                }
            }
        }
        assert_eq!(
            d["components"]["schemas"]["ConsoleRoutingPolicy"]["properties"]["cost_currency"]["type"],
            "string"
        );
        assert_eq!(
            d["components"]["schemas"]["ConsoleRetrieveRequest"]["required"],
            json!(["query"])
        );
    }
    #[test]
    fn canonical_paths_keep_existing_authority_media_and_stronger_shapes() {
        let mut d = json!({"paths":{"/api/v1/routing/policies/{policy}/versions/{v}":{"get":{"security":[{"owner":[]}],"responses":{"200":{"description":"existing","content":{"application/json":{"schema":{"const":"strong"}}}}}}}},"components":{"schemas":{}}});
        seed_query_routes(&mut d);
        extend_document(&mut d);
        assert!(
            d["paths"]
                .get("/api/v1/routing/policies/{id}/versions/{version}")
                .is_none()
        );
        let op = &d["paths"]["/api/v1/routing/policies/{policy}/versions/{v}"]["get"];
        assert_eq!(op["security"], json!([{"owner":[]}]));
        assert_eq!(
            op["responses"]["200"]["content"]["application/json"]["schema"]["const"],
            "strong"
        );
        assert_eq!(op["parameters"][0]["name"], "policy");
    }
    #[test]
    fn suffix_commands_have_real_required_path_parameters_and_resume_is_not_fabricated_success() {
        let d = document();
        let op = &d["paths"]["/api/v2/agent/threads/{thread_id}/turns/{turn_id}:interrupt"]["post"];
        assert_eq!(op["parameters"][1]["name"], "turn_id");
        assert_eq!(op["parameters"][1]["required"], true);
        let resume = &d["paths"]["/api/v1/assistant/runs/{run_id}/resume"]["post"];
        assert!(resume["responses"].get("200").is_none());
        assert!(resume["responses"].get("409").is_some());
    }
    #[test]
    fn declared_console_operations_keep_live_owner_handler_provenance() {
        let catalog: Value = serde_json::from_str(CATALOG).unwrap();
        for (path, methods) in catalog["paths"].as_object().unwrap() {
            for (method, operation) in methods.as_object().unwrap() {
                let source = operation["x-native-declaration"]["source"]
                    .as_str()
                    .unwrap();
                let handler = operation["x-native-declaration"]["handler"]
                    .as_str()
                    .unwrap();
                let (_, text) = sources::SOURCES
                    .iter()
                    .find(|(name, _)| *name == source)
                    .unwrap_or_else(|| panic!("unembedded owner for {method} {path}: {source}"));
                assert!(
                    text.contains(handler),
                    "stale owner handler for {method} {path}: {handler}"
                );
            }
        }
    }
    #[test]
    fn runtime_events_use_only_the_mounted_v2_router_and_native_envelope() {
        let d = document();
        assert!(
            d["paths"]
                .get("/api/v1/agent/threads/{thread_id}/events")
                .is_none()
        );
        let stream = &d["paths"]["/api/v2/agent/threads/{thread_id}/events"]["get"]["responses"]["200"]
            ["content"];
        assert!(stream.get("application/json").is_none());
        assert_eq!(
            stream["text/event-stream"]["itemSchema"]["$ref"],
            "#/components/schemas/ConsoleAgentV2Event"
        );
        assert_eq!(
            d["components"]["schemas"]["ConsoleAgentV2Event"]["properties"]["schema_version"]["const"],
            "agent-event/v2"
        );
    }
    #[test]
    #[should_panic(expected = "query owner has no mounted route: GET /api/v1/usage/summary")]
    fn query_declarations_cannot_invent_an_unmounted_owner_route() {
        let mut d = json!({"paths":{},"components":{"schemas":{}}});
        seed_query_routes(&mut d);
        d["paths"]
            .as_object_mut()
            .unwrap()
            .remove("/api/v1/usage/summary");
        extend_document(&mut d);
    }
    #[test]
    fn queries_are_typed_from_owner_parsers_and_keep_required_filters() {
        let d = document();
        let query = &d["paths"]["/api/v1/usage/summary"]["get"]["parameters"];
        assert!(
            query
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["name"] == "start_date" && p["schema"]["type"] == "string")
        );
        let compare = &d["paths"]["/api/v1/knowledge/{dataset_id}/documents/{document_id}/versions/compare"]
            ["get"]["parameters"];
        assert!(
            compare
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["name"] == "from_version" && p["required"] == true)
        );
        let events = &d["paths"]["/api/v1/local-nodes/{device_id}/events"]["get"]["parameters"];
        let local_query: Vec<_> = events
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["in"] == "query")
            .collect();
        assert_eq!(
            local_query.len(),
            1,
            "the JSON Local Node feed must not inherit V2 SSE paging parameters"
        );
        assert_eq!(local_query[0]["name"], "after_sequence");
        assert_eq!(local_query[0]["schema"]["minimum"], 0);
        assert!(events.as_array().unwrap().iter().all(|p| {
            !["limit", "before_sequence", "cursor", "Last-Event-ID"]
                .iter()
                .any(|name| p["name"] == *name)
        }));
    }
    #[test]
    fn v2_sse_keeps_real_limit_and_header_resume_separate_from_local_node_json_events() {
        let d = document();
        let operation = &d["paths"]["/api/v2/agent/threads/{thread_id}/events"]["get"];
        let parameters = operation["parameters"].as_array().unwrap();
        let parameter = |name: &str, location: &str| {
            parameters
                .iter()
                .find(|p| p["name"] == name && p["in"] == location)
                .unwrap()
        };
        let limit = &parameter("limit", "query")["schema"];
        assert_eq!(limit["minimum"], 1);
        assert_eq!(limit["maximum"], 1000);
        assert_eq!(limit["default"], 100);
        let validator = jsonschema::options().build(limit).unwrap();
        for value in [1, 7, 100, 1000] {
            assert!(validator.is_valid(&json!(value)));
        }
        for value in [json!(0), json!(-1), json!(1001), json!(1.5), json!("1000")] {
            assert!(!validator.is_valid(&value));
        }
        let after = &parameter("after_sequence", "query")["schema"];
        assert_eq!(after["minimum"], 0);
        assert_eq!(after["maximum"], i64::MAX);
        // Do not synthesize a zero query in generated clients: absence must
        // remain distinguishable so Last-Event-ID can select the cursor.
        assert!(after.get("default").is_none());
        let header = parameter("Last-Event-ID", "header");
        assert_eq!(header["required"], false);
        assert_eq!(header["schema"]["type"], "string");
        // Header grammar is conditional on query absence. An unconditional
        // pattern would incorrectly reject a header ignored by the native parser.
        assert!(header["schema"].get("pattern").is_none());
        assert!(
            header["description"]
                .as_str()
                .unwrap()
                .contains("query cursor takes precedence")
        );
        assert!(
            operation["description"]
                .as_str()
                .unwrap()
                .contains("never repeats a turn-creation POST")
        );
        assert!(
            operation["responses"]["200"]["content"]
                .get("text/event-stream")
                .is_some()
        );
        assert!(
            parameters
                .iter()
                .all(|p| !(p["name"] == "Last-Event-ID" && p["in"] == "query"))
        );
    }

    #[test]
    fn wire_bodies_follow_actual_gate_cases_and_multipart_owners() {
        let d = document();
        let evaluation = &d["components"]["schemas"]["ConsoleRetrievalEvaluation"];
        assert_eq!(evaluation["required"], json!(["cases"]));
        assert!(evaluation["properties"].get("test_cases").is_none());
        assert!(evaluation["properties"].get("vector_top_k").is_some());
        let gate = &d["paths"]["/api/v1/knowledge/datasets/{dataset_id}/embedding-migration/{migration_id}/gate"]
            ["post"]["requestBody"]["content"]["application/json"]["schema"];
        assert_eq!(gate["additionalProperties"], false);
        assert_eq!(gate["properties"]["sample_size"]["maximum"], 256);
        assert!(gate["properties"].get("min_mrr").is_none());
        let upload = &d["paths"]["/api/v1/public/agents/{public_id}/attachments"]["post"]["requestBody"]
            ["content"]["multipart/form-data"]["schema"];
        assert_eq!(upload["required"], json!(["file"]));
        assert!(upload["properties"].get("session_id").is_none());
        let message = &d["components"]["schemas"]["ConsoleSessionMessage"];
        assert_eq!(message["required"], json!(["role", "content"]));
    }
}
