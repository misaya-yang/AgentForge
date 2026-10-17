//! Native repository gates and source/recovery integrity tooling.
mod architecture;
mod archive;
mod build_ops;
mod buildx_resource;
mod candidates;
mod ci;
mod compose_identity;
mod docs_truth;
mod embed;
mod evidence;
mod guard;
mod harness;
mod hygiene;
mod inventory_generators;
mod live_openapi;
mod loc;
mod native_gates;
mod packaging;
mod resource_lock;
mod rollback_ops;
mod rust_gate;
mod selection;
mod smoke_ops;
mod source;
mod topology;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn require(ok: bool, message: impl Into<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into().into())
    }
}
fn digest(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut b = [0; 65536];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        hash.update(&b[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn resolve_path(input: &str) -> Result<PathBuf> {
    let expanded = if input == "~" || input.starts_with("~/") {
        PathBuf::from(std::env::var_os("HOME").ok_or("HOME missing")?)
            .join(input.strip_prefix("~/").unwrap_or(""))
    } else if let Some(rest) = input.strip_prefix('~') {
        let (user, suffix) = rest.split_once('/').unwrap_or((rest, ""));
        let passwd = std::fs::read_to_string("/etc/passwd")?;
        let home = passwd
            .lines()
            .find_map(|line| {
                let fields: Vec<_> = line.split(':').collect();
                (fields.len() >= 7 && fields[0] == user).then(|| fields[5])
            })
            .ok_or("unknown home directory")?;
        PathBuf::from(home).join(suffix)
    } else {
        PathBuf::from(input)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()?.join(expanded)
    };
    fn resolve(path: &Path, links: &mut usize) -> Result<PathBuf> {
        use std::path::Component;
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                Component::RootDir => out.push("/"),
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                Component::Normal(part) => {
                    out.push(part);
                    match std::fs::symlink_metadata(&out) {
                        Ok(metadata) if metadata.file_type().is_symlink() => {
                            *links += 1;
                            require(*links <= 40, "symlink loop or excessive depth")?;
                            let target = std::fs::read_link(&out)?;
                            out.pop();
                            out = resolve(&out.join(target), links)?;
                        }
                        Ok(_) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                Component::Prefix(_) => return Err("unsupported path prefix".into()),
            }
        }
        Ok(out)
    }
    resolve(&absolute, &mut 0)
}
fn object(path: &Path) -> Result<Value> {
    let v = strict_json(&std::fs::read(path)?)?;
    require(v.is_object(), "JSON must be an object")?;
    Ok(v)
}
fn json_input(path: &str) -> Result<Value> {
    if path != "-" {
        return strict_json(&std::fs::read(path)?);
    }
    let mut bytes = vec![];
    std::io::stdin()
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    require(bytes.len() <= 64 * 1024 * 1024, "stdin JSON exceeds 64 MiB")?;
    strict_json(&bytes)
}
fn strict_json(bytes: &[u8]) -> Result<Value> {
    struct Strict(Value);
    impl<'de> serde::Deserialize<'de> for Strict {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Strict;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("JSON without duplicate object keys")
                }
                fn visit_bool<E: serde::de::Error>(
                    self,
                    v: bool,
                ) -> std::result::Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Strict, E> {
                    serde_json::Number::from_f64(v)
                        .map(|n| Strict(Value::Number(n)))
                        .ok_or_else(|| E::custom("non-finite JSON number"))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Strict, E> {
                    Ok(Strict(Value::Null))
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Strict, A::Error> {
                    let mut values = vec![];
                    while let Some(Strict(v)) = a.next_element()? {
                        values.push(v);
                    }
                    Ok(Strict(Value::Array(values)))
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Strict, A::Error> {
                    let mut values = serde_json::Map::new();
                    while let Some((k, Strict(v))) = a.next_entry::<String, Strict>()? {
                        if values.insert(k, v).is_some() {
                            return Err(serde::de::Error::custom("duplicate JSON object key"));
                        }
                    }
                    Ok(Strict(Value::Object(values)))
                }
            }
            d.deserialize_any(Visitor)
        }
    }
    Ok(serde_json::from_slice::<Strict>(bytes)?.0)
}
fn safe(root: &Path, relative: &str) -> Result<PathBuf> {
    require(
        !relative.is_empty()
            && !relative.contains(['\\', '\0', ':'])
            && relative.split('/').all(|s| !matches!(s, "" | "." | "..")),
        "unsafe relative path",
    )?;
    let root = root.canonicalize()?;
    let mut path = root.clone();
    for part in relative.split('/') {
        path.push(part);
        if let Ok(m) = std::fs::symlink_metadata(&path) {
            require(!m.file_type().is_symlink(), "source symlink forbidden")?;
        }
    }
    require(path.starts_with(root), "escaping path")?;
    Ok(path)
}
fn walk(root: &Path, prune: &[&str], symlinks: bool) -> Result<Vec<PathBuf>> {
    fn visit(
        base: &Path,
        path: &Path,
        prune: &[&str],
        links: bool,
        out: &mut Vec<PathBuf>,
    ) -> Result<()> {
        for e in std::fs::read_dir(path)? {
            let e = e?;
            let p = e.path();
            let m = std::fs::symlink_metadata(&p)?;
            if m.file_type().is_symlink() {
                if links {
                    out.push(p.strip_prefix(base)?.into());
                }
                continue;
            }
            if m.is_dir() {
                if !prune.contains(&e.file_name().to_string_lossy().as_ref()) {
                    visit(base, &p, prune, links, out)?;
                }
            } else {
                require(m.is_file(), "nonregular filesystem entry")?;
                out.push(p.strip_prefix(base)?.into());
            }
        }
        Ok(())
    }
    let mut out = vec![];
    visit(root, root, prune, symlinks, &mut out)?;
    out.sort();
    Ok(out)
}
fn write_json(path: &Path, v: &Value) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, format!("{}\n", serde_json::to_string_pretty(v)?))?;
    std::fs::rename(tmp, path)?;
    Ok(())
}
#[derive(Default)]
struct Args {
    values: BTreeMap<String, String>,
    multiple: BTreeMap<String, Vec<String>>,
    flags: Vec<String>,
    positionals: Vec<String>,
}
impl Args {
    fn parse() -> Result<Self> {
        let mut args = Self::default();
        let mut it = std::env::args().skip(1).peekable();
        while let Some(s) = it.next() {
            if s == "--" {
                args.positionals.extend(it);
                break;
            }
            if s.starts_with("--") {
                if ![
                    "--require-zero",
                    "--require-zero-python",
                    "--require-runnable",
                    "--overlay",
                    "--check-all",
                    "--selftest",
                    "--apply",
                    "--allow-cross-host",
                    "--allow-live-owner",
                    "--require-helm",
                    "--sdk",
                    "--matrix",
                    "--build-env",
                    "--short12",
                    "--source-build",
                ]
                .contains(&s.as_str())
                    && it.peek().is_some_and(|v| !v.starts_with("--"))
                {
                    let value = it.next().unwrap();
                    args.multiple
                        .entry(s.clone())
                        .or_default()
                        .push(value.clone());
                    args.values.insert(s, value);
                } else {
                    args.flags.push(s);
                }
            } else {
                if args
                    .positionals
                    .first()
                    .is_some_and(|s| s == "resource-lock")
                    && args.positionals.get(1).is_some_and(|s| s == "run")
                {
                    args.positionals.push(s);
                    args.positionals.extend(it);
                    break;
                }
                args.positionals.push(s);
            }
        }
        Ok(args)
    }
    fn get(&self, key: &str) -> Result<&str> {
        self.values
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("missing {key}").into())
    }
    fn flag(&self, key: &str) -> bool {
        self.flags.iter().any(|s| s == key)
    }
    fn root(&self) -> PathBuf {
        self.values
            .get("--repo-root")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}
fn run() -> Result<bool> {
    let args = Args::parse()?;
    let root = args.root();
    let name = args.positionals.first().map(String::as_str).unwrap_or("");
    let value=match name{
 "inventory-generate"=>inventory_generators::run(&args)?,
 "embed-fixture-server"=>embed::serve(&args)?,
 "embed-header-check"=>embed::check(&root)?,
 "resolve-path"=>json!({"_plain":resolve_path(args.positionals.get(1).ok_or("resolve-path requires PATH")?)?.to_str().ok_or("path is not UTF-8")?}),
 "topology"=>topology::run(&args)?,
 "json-get"=>{let path=args.positionals.get(1).ok_or("json-get requires FILE dotted.path")?;let field=args.positionals.get(2).ok_or("json-get requires dotted.path")?;let value=json_input(path)?;let mut selected=&value;for part in field.split('.') {selected=if let Some(object)=selected.as_object(){object.get(part)}else if let Some(array)=selected.as_array(){part.parse::<usize>().ok().and_then(|i|array.get(i))}else{None}.ok_or("JSON field missing")?;}json!({"_plain":match selected{Value::String(s)=>s.clone(),_=>selected.to_string()}})},
 "json-pretty"=>json!({"_plain":serde_json::to_string_pretty(&json_input(args.positionals.get(1).map(String::as_str).unwrap_or("-"))?)?}),
 "uuid"=>{let id=uuid::Uuid::new_v4();json!({"_plain":if args.flag("--short12"){id.simple().to_string()[..12].to_owned()}else{id.to_string()}})},
 "free-port"=>{let listener=std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,0))?;json!({"_plain":listener.local_addr()?.port().to_string()})},
 "sha256-file"=>json!({"_plain":digest(Path::new(args.positionals.get(1).ok_or("sha256-file requires FILE")?))?}),
 "harness-check"=>harness::check(&root)?,
 "compose-image-check"=>compose_identity::run(&args)?,
 "compose-image-matrix"=>compose_identity::matrix(&root)?,
 "docs-truth-check"=>docs_truth::check(&root)?,
 "evidence"=>evidence::run(&args)?,
 "topology-inventory"=>guard::generate(&args)?,
 "single-instance-guard"=>guard::run(&args)?,
 "loc-no-growth"=>loc::run(&args)?,
 "hygiene-check"=>hygiene::run(&args)?,
 "known-candidates"=>candidates::run(&args)?,
 "resource-lock"=>resource_lock::run(&args)?,
 "cli-package-materials"=>build_ops::cli_materials(&args)?,
 "runtime-smoke-payloads"=>build_ops::runtime_smoke_payloads()?,
 "capability-worker-smoke"=>smoke_ops::run(&args)?,
 "rollback-helper"=>rollback_ops::run(&args)?,
 "native-candidate-plan"=>native_gates::plan(&args)?,
 "package-platformctl"=>packaging::run(&args,false)?,
 "package-desktop"=>packaging::run(&args,true)?,
 "gateway-performance-baseline"=>ai_platform_tools::performance::plan(),
 "live-openapi-contract"=>live_openapi::run(&args)?,
 "agent-runtime-readonly-gate"=>native_gates::readonly(&args)?,
 "agent-runtime-write-gate"=>native_gates::write(&args)?,
 "agent-runtime-single-kernel-gate"=>native_gates::single_kernel(&args)?,
 "image-initialize-probe"=>build_ops::image_probe(&args)?,
 "buildx-resource-builder"=>buildx_resource::run(&args)?,
 "affected-gates"=>selection::run(&args)?,
 "ci-gate-enforcement"=>ci::run(&args)?,
 "rust-changed-crate-gate"=>rust_gate::run(&args)?,
 "architecture-boundary"=>architecture::check(&root, args.values.get("--composed-workspace").map(Path::new))?,
 "python-inventory"=>architecture::python_inventory(&root,args.flag("--require-zero"))?,
 "source-lock"=>{match args.positionals.get(1).map(String::as_str).unwrap_or("validate"){
  "validate"=>source::validate_lock(&root,Path::new(args.get("--lock")?),args.flag("--require-runnable"),args.values.get("--require-artifact").map(String::as_str))?,
  "verify-local-images"=>source::verify_images(&args)?,
  "record-local-image"=>source::record_image(&args)?,
  "verify-overlay"=>source::verify_overlay(&args)?,
  "refresh-source-lock"=>source::refresh_source_lock(&args)?,
  "refresh-overlay"=>source::refresh_overlay(&args)?,
  "generate"=>source::generate_receipt(&args)?,
  "generate-composed"=>source::generate_composed_receipt(&args)?,
  _=>return Err("unsupported source-lock operation".into()),
 }},
 "source-candidate"=>source::candidate(&args)?,
 "recovery-archive"=>archive::verify(&args)?,
 "bundle-identity"=>source::bundle_identity(Path::new(args.get("--directory")?),args.flag("--overlay"))?,
 _=>return Err("usage: platform-tools {harness-check|architecture-boundary|python-inventory|source-lock validate|source-candidate verify|source-candidate assemble|recovery-archive verify|bundle-identity|buildx-resource-builder} [--repo-root PATH]".into())};
    if let Some(code) = value.get("_exit").and_then(Value::as_i64) {
        std::process::exit(code as i32);
    }
    if let Some(plain) = value.get("_plain").and_then(Value::as_str) {
        println!("{plain}");
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    if value["result"] == "error" {
        return Err("gate input error; see evidence".into());
    }
    if value["result"] == "blocked" {
        return Err("gate blocked; see findings".into());
    }
    Ok(value["result"] != "fail"
        && value["source_validation"] != "failed"
        && value["status"] != "fail")
}
fn main() {
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("{}", json!({"result":"fail","error":e.to_string()}));
            std::process::exit(
                if matches!(
                    std::env::args().nth(1).as_deref(),
                    Some(
                        "topology"
                            | "affected-gates"
                            | "rust-changed-crate-gate"
                            | "evidence"
                            | "single-instance-guard"
                            | "loc-no-growth"
                            | "hygiene-check"
                            | "known-candidates"
                            | "resource-lock"
                            | "agent-runtime-write-gate"
                    )
                ) {
                    2
                } else {
                    1
                },
            )
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsafe_paths_fail_before_io() {
        for s in ["../escape", "/abs", "x//y", "x/./y", "a\\b", "C:disk"] {
            assert!(safe(Path::new("."), s).is_err());
        }
    }
    #[test]
    fn duplicate_json_keys_fail_closed() {
        assert!(strict_json(br#"{"outer":{"value":1,"value":2}}"#).is_err());
        assert_eq!(
            strict_json(br#"{"a":[true,null,1,-2,3.5,"x"]}"#).unwrap(),
            json!({"a":[true,null,1,-2,3.5,"x"]})
        );
    }
    #[test]
    fn resolve_missing_suffix_and_symlinks() {
        let base =
            std::env::temp_dir().join(format!("platform-resolve-test-{}", std::process::id()));
        std::fs::create_dir_all(base.join("actual/nested")).unwrap();
        std::os::unix::fs::symlink("actual/nested", base.join("link")).unwrap();
        assert_eq!(
            resolve_path(base.join("link/../missing/../leaf").to_str().unwrap()).unwrap(),
            base.join("actual/leaf")
        );
        std::os::unix::fs::symlink("loop", base.join("loop")).unwrap();
        assert!(resolve_path(base.join("loop").to_str().unwrap()).is_err());
        assert_eq!(resolve_path("/").unwrap(), PathBuf::from("/"));
        std::fs::remove_dir_all(base).unwrap();
    }
}

/// Python json.dumps(sort_keys=True, separators=(',', ':'), ensure_ascii=False)
/// for wire fingerprints; independent of serde_json preserve_order feature unification.
fn canonical_sorted(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort();
            format!(
                "{{{}}}",
                keys.iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(*key).unwrap(),
                        canonical_sorted(&object[*key])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Array(array) => format!(
            "[{}]",
            array
                .iter()
                .map(canonical_sorted)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Number(number) if number.is_f64() => {
            let n = number.as_f64().unwrap();
            if n != 0.0 && (n.abs() < 1e-4 || n.abs() >= 1e16) {
                let scientific = format!("{n:e}");
                let (mantissa, exponent) = scientific.split_once('e').unwrap();
                let exponent: i32 = exponent.parse().unwrap();
                format!(
                    "{mantissa}e{}{:02}",
                    if exponent < 0 { "-" } else { "+" },
                    exponent.unsigned_abs()
                )
            } else {
                let text = n.to_string();
                if text.contains('.') {
                    text
                } else {
                    format!("{text}.0")
                }
            }
        }
        _ => value.to_string(),
    }
}
