use crate::*;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
pub fn plan(args: &Args) -> Result<Value> {
    let root = args.root();
    for rel in [
        "harness.yml",
        "rust/platform-native/Cargo.lock",
        "deploy/platform-native/Dockerfile",
        "deploy/compose/docker-compose.native-candidate.yml",
    ] {
        require(
            safe(&root, rel)?.is_file(),
            "native candidate markers missing",
        )?;
    }
    let mut compose = vec!["docker", "compose", "--env-file", "<operator-env-file>"];
    for file in [
        "docker-compose.yml",
        "deploy/compose/docker-compose.full.yml",
        "deploy/compose/docker-compose.build.yml",
        "deploy/compose/docker-compose.native-candidate.yml",
    ] {
        compose.extend(["-f", file]);
    }
    if args.flag("--sdk") {
        for file in [
            "deploy/compose/docker-compose.sdk-agent-worker.yml",
            "deploy/compose/docker-compose.sdk-agent-worker.build.yml",
        ] {
            compose.extend(["-f", file]);
        }
        compose.extend(["--profile", "sdk-agents"]);
    }
    let mut build = vec!["env", "COMPOSE_PARALLEL_LIMIT=1"];
    build.extend(compose);
    build.extend([
        "build",
        "migrate",
        "gateway",
        "frontend",
        "knowledge-service",
        "knowledge-worker",
    ]);
    if args.flag("--sdk") {
        build.push("sdk-agent-worker");
    }
    Ok(
        json!({"mode":"plan-only","cwd":root,"executed":false,"prerequisites":["Read runtime-and-secrets and verify Compose ownership before any Docker action","Set unique candidate image tags and external reviewed GatewayDocument","Controlled Runtime upstream must remain a clean verified Git checkout","Review epoch 17 and scoped role evidence before any startup"],"commands":[["bash","scripts/harness/build_agent_runtime_image.sh"],["bash","scripts/harness/build_agent_capability_worker_image.sh"],build],"excluded":["startup","migration execution","provider calls","publication","live acceptance"]}),
    )
}
pub fn readonly(args: &Args) -> Result<Value> {
    let status = Command::new("cargo")
        .args(["test", "--locked", "--manifest-path"])
        .arg(args.root().join("rust/platform-native/Cargo.toml"))
        .args([
            "-p",
            "ai-platform-gateway",
            "--test",
            "readonly_contributor_gate",
        ])
        .status()?;
    require(status.success(), "readonly contributor gate failed")?;
    Ok(json!({"_plain":"AI_PLATFORM_AGENT_RUNTIME_READONLY_GATE_OK"}))
}
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn write(args: &Args) -> Result<Value> {
    let root = args.root();
    let source = args
        .values
        .get("--fork")
        .cloned()
        .or_else(|| std::env::var("AI_PLATFORM_AGENT_RUNTIME_SOURCE").ok())
        .unwrap_or("../AI--Platfform-agent-runtime-source".into());
    let source = std::fs::canonicalize(source)?;
    require(
        source.join("justfile").is_file() && source.join("codex-rs").is_dir(),
        "controlled source checkout missing",
    )?;
    let overlay = root.join("rust/agent-runtime-overlay");
    let manifest = object(&overlay.join("manifest.json"))?;
    let sha = manifest["upstream_sha"]
        .as_str()
        .ok_or("upstream SHA missing")?;
    require(
        sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid upstream SHA",
    )?;
    let temp = Temp(std::env::temp_dir().join(format!(
        "ai-platform-agent-runtime-write-gate-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&temp.0)?;
    let mut archive = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["archive", sha])
        .stdout(Stdio::piped())
        .spawn()?;
    let extraction = Command::new("tar")
        .args(["-xf", "-", "--no-same-owner", "-C"])
        .arg(&temp.0)
        .stdin(archive.stdout.take().ok_or("archive stdout missing")?)
        .status()?;
    require(
        extraction.success() && archive.wait()?.success(),
        "source extraction failed",
    )?;
    for rel in walk(&overlay.join("kernel-rs"), &[], true)? {
        let from = safe(&overlay.join("kernel-rs"), &rel.to_string_lossy())?;
        let to = safe(&temp.0.join("codex-rs"), &rel.to_string_lossy())?;
        std::fs::create_dir_all(to.parent().ok_or("missing parent")?)?;
        std::fs::copy(&from, &to)?;
        std::fs::set_permissions(
            to,
            std::fs::Permissions::from_mode(std::fs::metadata(from)?.permissions().mode()),
        )?;
    }
    let mut command = Command::new("cargo");
    command
        .args(["test", "-p", "ai-platform-agent-runtime", "--lib"])
        .current_dir(temp.0.join("codex-rs"));
    if std::env::var_os("CARGO_TERM_COLOR").is_none() {
        command.env("CARGO_TERM_COLOR", "never");
    }
    if std::env::var_os("CARGO_TARGET_DIR").is_none() {
        command.env(
            "CARGO_TARGET_DIR",
            std::env::temp_dir().join("ai-platform-agent-runtime-write-gate-target"),
        );
    }
    require(
        command.status()?.success(),
        "tool lifecycle contract failed",
    )?;
    Ok(
        json!({"_plain":"AI_PLATFORM_AGENT_RUNTIME_WRITE_GATE_OK\ninvariants=approval,dispatch_fence,idempotency,recovery,pairing"}),
    )
}

fn calls(source: &str, function: &str) -> Result<std::collections::BTreeSet<String>> {
    use syn::visit::Visit;
    struct Calls {
        calls: std::collections::BTreeSet<String>,
    }
    impl<'a> Visit<'a> for Calls {
        fn visit_expr_call(&mut self, node: &'a syn::ExprCall) {
            if let syn::Expr::Path(p) = &*node.func {
                if let Some(segment) = p.path.segments.last() {
                    self.calls.insert(segment.ident.to_string());
                }
            }
            syn::visit::visit_expr_call(self, node);
        }
        fn visit_expr_method_call(&mut self, node: &'a syn::ExprMethodCall) {
            self.calls.insert(node.method.to_string());
            syn::visit::visit_expr_method_call(self, node);
        }
    }
    struct Find<'a> {
        name: &'a str,
        found: Vec<std::collections::BTreeSet<String>>,
    }
    impl<'ast> Visit<'ast> for Find<'_> {
        fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
            if node.sig.ident == self.name {
                let mut c = Calls {
                    calls: Default::default(),
                };
                c.visit_block(&node.block);
                self.found.push(c.calls);
            }
            syn::visit::visit_item_fn(self, node);
        }
        fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
            if node.sig.ident == self.name {
                let mut c = Calls {
                    calls: Default::default(),
                };
                c.visit_block(&node.block);
                self.found.push(c.calls);
            }
            syn::visit::visit_impl_item_fn(self, node);
        }
    }
    let tree = syn::parse_file(source)?;
    let mut find = Find {
        name: function,
        found: vec![],
    };
    find.visit_file(&tree);
    require(
        find.found.len() == 1,
        format!("single-kernel function missing/ambiguous: {function}"),
    )?;
    Ok(find.found.remove(0))
}
pub fn single_kernel(args: &Args) -> Result<Value> {
    let root = args.root();
    let base = root.join("rust/platform-native/crates/platform-gateway/src");
    let routes = std::fs::read_to_string(base.join("migration_agent/routes.rs"))?;
    let responses = std::fs::read_to_string(base.join("migration_compat/responses.rs"))?;
    let public = std::fs::read_to_string(base.join("migration_agent/public.rs"))?;
    let assignment = std::fs::read_to_string(base.join("migration_agent/threads.rs"))?;
    for (source, function, required) in [
        (&routes, "start_snapshot_turn_with_refs", "start_turn"),
        (&routes, "start_builtin_profile", "start_turn"),
        (&routes, "assistant", "start_builtin_profile"),
        (&routes, "preview_route", "start_snapshot_turn_with_refs"),
        (&routes, "v2", "start_snapshot_turn"),
        (&responses, "create_response", "start_turn"),
        (&public, "dispatch_inner", "start_snapshot_turn_with_refs"),
    ] {
        let calls = calls(source, function)?;
        require(
            calls.contains(required),
            format!("{function} must enter {required}"),
        )?;
        require(
            !calls
                .iter()
                .any(|c| c.contains("proxy") && c.contains("assistant")),
            "legacy assistant loop call forbidden",
        )?;
    }
    require(
        assignment.contains("\"runtime_owner\") != \"agent_runtime\"")
            && !assignment.contains("CANARY_PERCENT")
            && !assignment.contains("python_control")
            && !assignment.contains("codex_candidate"),
        "single runtime assignment contract changed",
    )?;
    let compose: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(root.join("docker-compose.yml"))?)?;
    let runtime = &compose["services"]["agent-runtime"];
    require(
        runtime.is_mapping()
            && runtime["container_name"].as_str() == Some("ai-gateway-agent-runtime")
            && runtime["profiles"].is_null(),
        "Agent Runtime must be unconditional singleton service",
    )?;
    let compose = serde_yaml::to_string(&compose)?;
    require(
        !compose.contains("python_control") && !compose.contains("codex_candidate"),
        "retired execution owner remains in Compose",
    )?;
    Ok(
        json!({"single_kernel":"agent_runtime","v1_projection":"passed","v2_native":"passed","python_loop_route":"absent","source_language":"rust","method":"syn AST direct call targets"}),
    )
}
