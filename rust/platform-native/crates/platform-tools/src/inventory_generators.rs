//! Versioned native source inventories. Historical Python baselines are immutable.
use crate::*;
use std::{collections::BTreeSet, process::Command};
mod contracts;
mod core;
mod data;
mod facts;
mod immutable_export;
const ID: &str = "native-rust-v2";
const KINDS: &[(&str, &str)] = &[
    ("service-topology", "service-topology.json"),
    ("data-access-inventory", "data-access-inventory.json"),
    ("loc-baseline", "loc-baseline.json"),
    ("dependency-baseline", "dependency-baseline.json"),
    ("skip-baseline", "skip-baseline.json"),
    ("contract-freeze", "contract-freeze.json"),
];
const INPUTS: &[&str] = &[
    "src",
    "apps",
    "packages",
    "scripts",
    "database",
    "web",
    "sdk",
    "tests",
    "rust",
    "reports/performance",
    "docker-compose.yml",
    "deploy/compose/docker-compose.compact.yml",
    "deploy/compose/docker-compose.full.yml",
    "deploy/compose/docker-compose.scale.yml",
    "deploy/compose/docker-compose.dev.yml",
    "deploy/compose/docker-compose.build.yml",
    "deploy/compose/docker-compose.kbms.yml",
    "deploy/compose/docker-compose.capability.yml",
];
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    require(
        out.status.success(),
        format!(
            "Git source operation failed: {}",
            args.first().unwrap_or(&"")
        ),
    )?;
    Ok(out.stdout)
}
fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git(root, args)?)?)
}
fn valid_sha(sha: &str) -> bool {
    sha.len() == 40
        && sha
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn clean_head(root: &Path) -> Result<String> {
    let sha = git_text(root, &["rev-parse", "--verify", "HEAD"])?
        .trim()
        .to_owned();
    require(valid_sha(&sha), "invalid Git HEAD")?;
    require(
        git(root, &["status", "--porcelain=v1", "--untracked-files=all"])?.is_empty(),
        "baseline generation requires clean tracked and untracked working tree",
    )?;
    Ok(sha)
}
fn require_tree(root: &Path, sha: &str) -> Result<()> {
    require(valid_sha(sha), "source revision must be full lowercase SHA")?;
    git(root, &["merge-base", "--is-ancestor", sha, "HEAD"])?;
    let mut args = vec!["diff", "--quiet", sha, "--"];
    args.extend_from_slice(INPUTS);
    git(root, &args)?;
    Ok(())
}
struct Snapshot {
    sha: String,
    tree: String,
    files: BTreeMap<String, Vec<u8>>,
}
impl Snapshot {
    fn read(root: &Path, sha: &str) -> Result<Self> {
        require(valid_sha(sha), "invalid immutable source revision")?;
        require(
            git_text(
                root,
                &["rev-parse", "--verify", &format!("{sha}^{{commit}}")],
            )?
            .trim()
                == sha,
            "source commit mismatch",
        )?;
        let tree = git_text(root, &["rev-parse", &format!("{sha}^{{tree}}")])?
            .trim()
            .to_owned();
        let listing = git(root, &["ls-tree", "-r", "-z", sha])?;
        let mut files = BTreeMap::new();
        for row in listing.split(|b| *b == 0).filter(|b| !b.is_empty()) {
            let row = std::str::from_utf8(row)?;
            let (meta, path) = row.split_once('\t').ok_or("malformed Git tree")?;
            if !meta.starts_with("100")
                || path.split('/').any(|s| crate::harness::PRUNED.contains(&s))
            {
                continue;
            }
            if !INPUTS
                .iter()
                .any(|p| path == *p || path.starts_with(&format!("{p}/")))
            {
                continue;
            }
            let oid = meta.split_whitespace().nth(2).ok_or("blob id missing")?;
            files.insert(path.into(), git(root, &["cat-file", "blob", oid])?);
        }
        Ok(Self {
            sha: sha.into(),
            tree,
            files,
        })
    }
    fn text(&self, path: &str) -> Result<&str> {
        Ok(std::str::from_utf8(
            self.files
                .get(path)
                .ok_or_else(|| format!("source file missing: {path}"))?,
        )?)
    }
    fn envelope(&self, kind: &str) -> Value {
        json!({"schema":format!("ai-gateway/baseline/{kind}/v2"),"baseline_id":ID,"base_git_sha":self.sha,"generator":"platform-tools inventory-generate","source_language":"rust","provenance":{"source_kind":"git-commit","source_tree":self.tree,"schema_version":"ai-platform/native-source-provenance/v1"}})
    }
}
fn serialize(value: &Value) -> Result<Vec<u8>> {
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, v)| (k.clone(), sorted(v)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
            _ => v.clone(),
        }
    }
    let mut bytes = serde_json::to_vec_pretty(&sorted(value))?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn canonical(v: &Value) -> Result<String> {
    let bytes = serialize(v)?;
    let sorted: Value = serde_json::from_slice(&bytes)?;
    Ok(hash(&serde_json::to_vec(&sorted)?))
}
fn unit(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("rust/platform-native/crates/") {
        return rest.split('/').next().unwrap_or("other").to_owned();
    }
    if let Some(rest) = path.strip_prefix("rust/agent-runtime-overlay/kernel-rs/") {
        return rest.split('/').next().unwrap_or("runtime").to_owned();
    }
    path.split('/').next().unwrap_or("other").into()
}
fn build(s: &Snapshot, kind: &str) -> Result<Value> {
    match kind {
        "loc-baseline" => facts::loc(s),
        "dependency-baseline" => facts::dependencies(s),
        "skip-baseline" => facts::skips(s),
        "data-access-inventory" => data::build(s),
        "service-topology" => contracts::topology(s),
        "contract-freeze" => contracts::build(s),
        "core-consumption" => core::consumption(s),
        "persistence-sql" => core::persistence(s),
        "domain-ownership" => core::domains(s),
        _ => Err("unknown inventory kind".into()),
    }
}
pub fn run(args: &Args) -> Result<Value> {
    if let Some(operation) = args.values.get("--operation") {
        require(
            operation == "immutable-loc-export",
            "unknown inventory operation",
        )?;
        return immutable_export::run(args);
    }
    let root = args.root();
    let kind = args
        .values
        .get("--kind")
        .map(String::as_str)
        .unwrap_or("all");
    let write = args.flag("--write");
    require(
        !(write && args.flag("--verify")),
        "--write and --verify are mutually exclusive",
    )?;
    let output = args
        .values
        .get("--output-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("docs/architecture/baselines").join(ID));
    require(
        !output.to_string_lossy().contains("2026-08-post-rag"),
        "historical baseline destination is immutable",
    )?;
    let selected: Vec<(String, String)> = if kind == "all" {
        KINDS
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    } else {
        vec![(kind.to_owned(), format!("{kind}.json"))]
    };
    let head = clean_head(&root)?;
    let source = if write {
        let sha = args.get("--source-rev")?;
        require(sha == head, "--source-rev must equal clean HEAD")?;
        sha.to_owned()
    } else {
        require(
            !args.values.contains_key("--source-rev"),
            "--source-rev requires --write",
        )?;
        let mut revisions = BTreeSet::new();
        for (_, file) in &selected {
            let payload = object(&output.join(file))?;
            require(
                payload["baseline_id"] == ID,
                "historical or foreign baseline cannot be refreshed implicitly",
            )?;
            revisions.insert(
                payload["base_git_sha"]
                    .as_str()
                    .ok_or("baseline revision absent")?
                    .to_owned(),
            );
        }
        require(revisions.len() == 1, "inconsistent source revisions")?;
        revisions.into_iter().next().unwrap()
    };
    require_tree(&root, &source)?;
    let snapshot = Snapshot::read(&root, &source)?;
    let mut generated = vec![];
    for (k, file) in selected {
        generated.push((file, serialize(&build(&snapshot, &k)?)?));
    }
    require(clean_head(&root)? == head, "HEAD changed during generation")?;
    require_tree(&root, &source)?;
    let mut outcomes = vec![];
    for (file, bytes) in generated {
        let path = output.join(&file);
        if write {
            std::fs::create_dir_all(&output)?;
            std::fs::write(&path, bytes)?;
            outcomes.push(json!({"file":file,"result":"written"}));
        } else {
            let equal = std::fs::read(&path).is_ok_and(|old| old == bytes);
            outcomes.push(json!({"file":file,"result":if equal{"pass"}else{"drift"}}));
        }
    }
    Ok(
        json!({"result":if outcomes.iter().any(|r|r["result"]=="drift"){"fail"}else{"pass"},"source_revision":source,"outputs":outcomes,"writes":write}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_order() {
        assert_eq!(
            serialize(&json!({"z":1,"a":{"y":2,"b":3}})).unwrap(),
            b"{\n  \"a\": {\n    \"b\": 3,\n    \"y\": 2\n  },\n  \"z\": 1\n}\n"
        );
    }
    #[test]
    fn exact_sha() {
        assert!(!valid_sha("HEAD"));
        assert!(!valid_sha(&"A".repeat(40)));
        assert!(valid_sha(&"a".repeat(40)));
    }
    #[test]
    fn native_inventory_fixture_covers_all_producers() {
        let root = "rust/platform-native/crates/platform-fixture";
        let s = Snapshot {
            sha: "a".repeat(40), tree: "b".repeat(40),
            files: BTreeMap::from([
                ("src/core/data/service_topology.json".into(), include_bytes!("inventory_generators/topology_fixture.json").to_vec()),
                (format!("{root}/Cargo.toml"), b"[package]\nname='ai-platform-fixture'\nversion='0.1.0'\n[dependencies]\nsqlx='0.8'\n".to_vec()),
                (format!("{root}/src/lib.rs"), b"pub fn lookup(){ let query = include_str!(\"query.sql\"); }\n#[test]\n#[ignore = \"live database\"]\nfn integration() {}\n".to_vec()),
                (format!("{root}/src/query.sql"), b"SELECT * FROM users".to_vec()),
                ("database/schema.sql".into(), b"CREATE TABLE users (\n id BIGSERIAL,\n email TEXT\n);\nCREATE FUNCTION write_user() RETURNS void AS $$ INSERT INTO users(email) VALUES('x'); $$ LANGUAGE sql;".to_vec()),
                ("web/e2e/sample.spec.ts".into(), b"test.skip('live', () => {});".to_vec()),
                ("docker-compose.yml".into(), b"services:\n  gateway:\n    image: local-${REV}\n".to_vec()),
            ]),
        };
        for kind in KINDS.iter().map(|(k, _)| *k).chain([
            "core-consumption",
            "persistence-sql",
            "domain-ownership",
        ]) {
            let value = build(&s, kind).unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert_eq!(value["base_git_sha"], s.sha);
            assert_eq!(
                serialize(&value).unwrap(),
                serialize(&build(&s, kind).unwrap()).unwrap()
            );
        }
        let inventory = data::build(&s).unwrap();
        assert_eq!(
            inventory["postgresql"]["object_counts"]["sequences_implicit_serial"],
            1
        );
        assert_eq!(
            inventory["postgresql"]["tables"][0]["static_readers"],
            json!(["platform-fixture"])
        );
        let skip = facts::skips(&s).unwrap();
        assert_eq!(skip["rust"]["marker_count"], 1);
        assert_eq!(skip["typescript"]["marker_count"], 1);
    }
}
