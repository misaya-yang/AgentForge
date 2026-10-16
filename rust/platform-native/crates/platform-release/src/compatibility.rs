use crate::*;
use serde_json::json;
use std::collections::BTreeSet;
#[path = "../../platform-eval/src/canonical.rs"]
mod evidence_encoding;
pub const SERVICES: &[&str] = &[
    "gateway",
    "knowledge-service",
    "frontend",
    "knowledge-worker",
    "migrator",
    "agent-runtime",
    "capability-worker",
];
pub const RECEIPTS: &[(&str, &str)] = &[
    ("platform_db", "platform-db"),
    ("agent_execution", "agent-execution"),
    ("knowledge", "knowledge"),
    ("fresh_install", "fresh-install"),
    ("rollback", "rollback"),
    ("version_agreement", "version-agreement"),
];
const SECTIONS: &[&str] = &[
    "runtime_overlay",
    "database",
    "contracts",
    "topology",
    "toolchains",
    "release_evidence",
];
fn sha(root: &Path, p: &str) -> Value {
    let p = root.join(p);
    if p.is_file() && !p.is_symlink() {
        digest(&p).map(Value::String).unwrap_or(Value::Null)
    } else {
        Value::Null
    }
}
pub fn canonical(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys = m.keys().collect::<Vec<_>>();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), canonical(&m[k])))
                    .collect(),
            )
        }
        Value::Array(a) => a.iter().map(canonical).collect(),
        _ => v.clone(),
    }
}
pub fn canonical_hash(v: &Value) -> String {
    evidence_encoding::digest(v)
}
fn compose(root: &Path, other: &str) -> Value {
    let v = json!({"docker-compose.yml":sha(root,"docker-compose.yml"),other:sha(root,other)});
    if v.as_object().unwrap().values().any(Value::is_null) {
        Value::Null
    } else {
        canonical_hash(&v).into()
    }
}
pub fn offline(root: &Path) -> Result<Value> {
    let lock = load(&root.join("deploy/agent-runtime-source/lock.json"))?;
    let b = &lock["build"];
    let rust = b["rustc"].as_str().unwrap_or("");
    let re = regex::Regex::new(r"^rustc ([0-9]+\.[0-9]+\.[0-9]+)")?;
    let rust = re.captures(rust).map(|c| c[1].to_string());
    let roles = sha(root, "database/bootstrap/roles.sql");
    let grants = sha(root, "database/baselines/2026_08_post_kb_v1/grants.sql");
    let grants_rev = if roles.is_null() || grants.is_null() {
        Value::Null
    } else {
        canonical_hash(&json!({"roles":roles,"grants":grants})).into()
    };
    let mut evidence = serde_json::Map::new();
    for (name, path) in [
        ("matrix", "deploy/release/release-rollback-matrix.json"),
        ("integration_gates", "deploy/release/integration-gates.json"),
        (
            "retirement_manifest",
            "deploy/release/historical-plan-retirement.json",
        ),
        (
            "matrix_schema",
            "deploy/release/schemas/release-rollback-matrix-v1.schema.json",
        ),
        (
            "receipt_schema",
            "deploy/release/schemas/integration-gate-receipt-v1.schema.json",
        ),
        (
            "retirement_schema",
            "deploy/release/schemas/historical-plan-retirement-v1.schema.json",
        ),
        (
            "closeout_template",
            "deploy/release/FINAL-CLOSEOUT-TEMPLATE.md",
        ),
    ] {
        evidence.insert(format!("{name}_path"), path.into());
        evidence.insert(format!("{name}_sha256"), sha(root, path));
    }
    let lexical=root.join("rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_query/lexical_v2.rs");
    let lexical = std::fs::read_to_string(lexical).unwrap_or_default();
    let bm25 = if lexical.contains("qdrant-bm25-document-v1") {
        Some("qdrant-bm25-document-v1")
    } else {
        None
    };
    Ok(
        json!({"runtime_overlay":{"lock":"deploy/agent-runtime-source/lock.json","lock_sha256":sha(root,"deploy/agent-runtime-source/lock.json"),"upstream_sha":lock["source"]["upstream_sha"],"overlay_sha256":b["overlay_sha256"],"runtime_schema_digest":b["app_server_schema_sha256"],"capability_worker_schema_digest":b["capability_worker_schema_sha256"]},"database":{"baseline_id":"2026_08_post_kb_v1","baseline_manifest_sha256":sha(root,"database/baselines/2026_08_post_kb_v1/manifest.json"),"migration_revision":sha(root,"database/migrations/2026_08_post_kb_v1/manifest.yml"),"grants_revision":grants_rev},"contracts":{"openapi_sha256":sha(root,"sdk/openapi.json"),"sse_fixture_sha256":sha(root,"sdk/fixtures/sse_inner_envelopes.json"),"capability_contract_sha256":sha(root,"rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/lib.rs"),"agent_event_schema_sha256":sha(root,"rust/platform-native/crates/platform-gateway/src/migration_host/bootstrap/usage_event_bus.rs")},"topology":{"compact_profile_revision":compose(root,"deploy/compose/docker-compose.compact.yml"),"scale_profile_revision":compose(root,"deploy/compose/docker-compose.scale.yml"),"service_topology_revision":sha(root,"src/core/data/service_topology.json"),"data_access_revision":sha(root,"docs/architecture/baselines/2026-08-post-rag/data-access-inventory.json"),"quality_baseline_revision":sha(root,"docs/architecture/baselines/2026-08-post-rag/loc-baseline.json"),"evidence_policy_revision":sha(root,"deploy/evidence/policy.json")},"toolchains":{"node_major":"26","rust":rust},"release_evidence":evidence,"vector_contract":{"bm25_revision":bm25}}),
    )
}
fn absent(v: &Value) -> bool {
    v.is_null() || v == ""
}
fn identities(v: &Value, complete: bool) -> Result<()> {
    for service in SERVICES {
        let r = &v["services"][*service];
        require(r.is_object(), "invalid service record")?;
        let i = &r["identity"];
        if i.is_null() && !complete {
            continue;
        }
        require(i.is_object(), "invalid identity")?;
        let mode = if ["gateway", "knowledge-service"].contains(service) {
            "http_version"
        } else {
            "container_image"
        };
        for (key, want) in [
            ("mode", json!(mode)),
            ("service_id", json!(service)),
            ("release_id", v["release_id"].clone()),
            ("git_sha", v["source"]["git_sha"].clone()),
            ("image_digest", r["image_digest"].clone()),
        ] {
            require(
                !(complete || !i[key].is_null()) || i[key] == want,
                format!("service identity mismatch: {service}.{key}"),
            )?;
        }
        if ["frontend", "knowledge-worker", "migrator"].contains(service) {
            if complete || !i["image_version"].is_null() {
                text(i, "image_version")?;
            }
        } else {
            require(i["image_version"].is_null(), "unexpected image version")?;
        }
    }
    Ok(())
}
pub fn git(root: &Path, args: &[&str]) -> Result<String> {
    let o = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()?;
    require(o.status.success(), format!("git {} failed", args.join(" ")))?;
    Ok(String::from_utf8(o.stdout)?.trim().into())
}
pub fn validate(root: &Path, v: &Value, level: &str) -> Result<Value> {
    require(["draft", "candidate"].contains(&level), "invalid level")?;
    require(
        v["schema_version"] == "ai-platform/compatibility-manifest/v1"
            && ["draft", "release_candidate", "released"].contains(&text(v, "status")?),
        "unsupported manifest",
    )?;
    let services = v["services"].as_object().ok_or("missing services")?;
    let receipts = v["receipts"].as_object().ok_or("missing receipts")?;
    require(
        services.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == SERVICES.iter().copied().collect()
            && receipts.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == RECEIPTS.iter().map(|(n, _)| *n).collect(),
        "service/receipt set drift",
    )?;
    identities(v, false)?;
    let offline = offline(root)?;
    let mut missing = Vec::new();
    if !v["release_id"].is_string() {
        missing.push("release_id".into());
    }
    for k in ["git_sha", "git_tree_sha"] {
        if !v["source"][k].is_string() {
            missing.push(format!("source.{k}"));
        }
    }
    for s in SERVICES {
        let r = &v["services"][*s];
        if absent(&r["image_digest"]) {
            missing.push(format!("services.{s}.image_digest"));
        }
        let i = &r["identity"];
        if !i.is_object() || i.as_object().unwrap().is_empty() {
            missing.push(format!("services.{s}.identity"));
            continue;
        }
        for k in [
            "mode",
            "service_id",
            "release_id",
            "git_sha",
            "image_digest",
        ] {
            if absent(&i[k]) {
                missing.push(format!("services.{s}.identity.{k}"));
            }
        }
        if ["frontend", "knowledge-worker", "migrator"].contains(s) && absent(&i["image_version"]) {
            missing.push(format!("services.{s}.identity.image_version"));
        }
    }
    require(v["vectors"].is_object(), "vectors missing")?;
    for k in [
        "qdrant_dataset_revision",
        "memory_namespace_revision",
        "collection_or_alias",
        "embedding_provider",
        "embedding_model",
        "embedding_dimension",
        "bm25_revision",
    ] {
        if absent(&v["vectors"][k]) {
            missing.push(format!("vectors.{k}"));
        }
    }
    for (n, _) in RECEIPTS {
        if absent(&v["receipts"][*n]) {
            missing.push(format!("receipts.{n}"));
        }
    }
    for s in SECTIONS {
        require(v[*s].is_object(), format!("missing section {s}"))?;
        for (k, current) in offline[*s].as_object().unwrap() {
            let actual = &v[*s][k];
            require(
                actual.is_null() || actual == current,
                format!("offline value drift {s}.{k}"),
            )?;
            if absent(current) || absent(actual) {
                missing.push(format!("{s}.{k}"));
            }
        }
    }
    let bm = &offline["vector_contract"]["bm25_revision"];
    require(
        v["vectors"]["bm25_revision"].is_null() || v["vectors"]["bm25_revision"] == *bm,
        "BM25 revision drift",
    )?;
    if absent(bm) && !missing.contains(&"vectors.bm25_revision".to_string()) {
        missing.push("vectors.bm25_revision".into());
    }
    let candidate = level == "candidate";
    require(
        !candidate
            || (["release_candidate", "released"].contains(&text(v, "status")?)
                && missing.is_empty()),
        format!("candidate incomplete: {missing:?}"),
    )?;
    if candidate {
        identities(v, true)?;
    }
    crate::evidence::retirement(
        root,
        &load(&root.join("deploy/release/historical-plan-retirement.json"))?,
    )?;
    crate::evidence::matrix(
        root,
        &load(&root.join("deploy/release/release-rollback-matrix.json"))?,
        level,
        if candidate {
            Some(text(v, "release_id")?)
        } else {
            None
        },
        if candidate {
            Some(text(&v["source"], "git_sha")?)
        } else {
            None
        },
    )?;
    if candidate {
        let rid = text(v, "release_id")?;
        require(
            regex::Regex::new(r"^[a-z0-9][a-z0-9._-]{7,127}$")?.is_match(rid),
            "invalid release id",
        )?;
        let source = text(&v["source"], "git_sha")?;
        let tree = text(&v["source"], "git_tree_sha")?;
        require(hex(source, 40) && hex(tree, 40), "invalid Git identity")?;
        for r in services.values() {
            let d = text(r, "image_digest")?;
            require(
                d.strip_prefix("sha256:").is_some_and(|s| hex(s, 64)),
                "image not digest-pinned",
            )?;
        }
        require(
            git(root, &["rev-parse", &format!("{source}^{{commit}}")])? == source
                && git(root, &["rev-parse", &format!("{source}^{{tree}}")])? == tree,
            "source Git identity mismatch",
        )?;
        for (n, g) in RECEIPTS {
            let raw = text(&v["receipts"], n)?;
            require(
                raw.starts_with("reports/") || raw.starts_with("tmp/gate-evidence/"),
                "receipt outside evidence roots",
            )?;
            let p = safe(root, raw)?;
            require(
                p.is_file() && !p.is_symlink(),
                "receipt missing or symlinked",
            )?;
            crate::evidence::receipt(&load(&p)?, Some(g), Some(rid), Some(source), true)?;
        }
    }
    Ok(
        json!({"result":if missing.is_empty(){"pass"}else{"draft"},"level":level,"status":v["status"],"missing":missing,"manifest_sha256":canonical_hash(v)}),
    )
}
pub fn generate(root: &Path, rev: &str, inputs: &Value) -> Result<Value> {
    require(
        hex(rev, 40)
            && git(root, &["rev-parse", "HEAD"])? == rev
            && git(root, &["status", "--porcelain=v1", "--untracked-files=all"])?.is_empty(),
        "candidate generation requires explicit clean HEAD",
    )?;
    for k in ["services", "vectors", "receipts"] {
        require(inputs[k].is_object(), format!("inputs missing {k}"))?;
    }
    let mut v = offline(root)?;
    v.as_object_mut().unwrap().remove("vector_contract");
    v["schema_version"] = "ai-platform/compatibility-manifest/v1".into();
    v["status"] = "release_candidate".into();
    v["release_id"] = inputs["release_id"].clone();
    v["source"] =
        json!({"git_sha":rev,"git_tree_sha":git(root,&["rev-parse",&format!("{rev}^{{tree}}")])?});
    for k in ["services", "vectors", "receipts"] {
        v[k] = inputs[k].clone();
    }
    v["vectors"]["bm25_revision"] = offline(root)?["vector_contract"]["bm25_revision"].clone();
    validate(root, &v, "candidate")?;
    Ok(v)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_order() {
        assert_eq!(
            canonical_hash(&json!({"z":1,"a":{"y":2,"b":3}})),
            canonical_hash(&json!({"a":{"b":3,"y":2},"z":1}))
        );
    }
    #[test]
    fn evidence_float_encoding_matches_source_contract() {
        assert_eq!(
            evidence_encoding::canonical(&json!({"value": 1e-7, "large": 1e20, "zero": -0.0})),
            "{\"large\":1e+20,\"value\":1e-07,\"zero\":-0.0}"
        );
    }
    #[test]
    fn absent_not_zero() {
        assert!(!absent(&json!(0)));
        assert!(absent(&Value::Null));
    }
}
