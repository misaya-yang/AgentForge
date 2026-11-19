use crate::*;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
type Env = BTreeMap<String, String>;
fn command(v: &Value) -> Result<Vec<String>> {
    let a = v.as_array().ok_or("command must be list")?;
    require(!a.is_empty(), "command empty")?;
    a.iter()
        .map(|x| {
            let s = x.as_str().ok_or("command part must be text")?;
            require(!s.is_empty(), "empty command part")?;
            Ok(s.into())
        })
        .collect()
}
pub fn validate_spec(v: &Value) -> Result<()> {
    require(
        v["schema_version"] == "ai-platform/integration-gates/v1",
        "unsupported integration spec",
    )?;
    let gates = v["gates"].as_object().ok_or("gates must be object")?;
    let name = regex::Regex::new(r"^[A-Z][A-Z0-9_]*$")?;
    for (id, g) in gates {
        require(
            ["L2", "L3"].contains(&text(g, "tier")?),
            format!("invalid tier {id}"),
        )?;
        let env = array(g, "required_env")?;
        for e in env {
            require(
                e.as_str().is_some_and(|s| name.is_match(s)),
                "invalid environment name",
            )?;
        }
        let fresh = &g["fresh_database"];
        if !fresh.is_null() {
            require(
                g["tier"] == "L3" && fresh.as_object().is_some_and(|m| m.len() == 2),
                "invalid fresh DB contract",
            )?;
            for k in ["admin_dsn_env", "migrator_dsn_env"] {
                let s = text(fresh, k)?;
                require(
                    name.is_match(s) && env.contains(&json!(s)),
                    "fresh DSN must be required",
                )?;
            }
        }
        let external = &g["external_receipt"];
        if !external.is_null() {
            require(
                external.as_object().is_some_and(|m| m.len() == 3),
                "invalid external receipt",
            )?;
            let e = text(external, "env")?;
            require(
                name.is_match(e) && env.contains(&json!(e)),
                "external receipt env must be required",
            )?;
            text(external, "gate")?;
            let cs = array(external, "commands")?;
            require(!cs.is_empty(), "external commands empty")?;
            for c in cs {
                command(c)?;
            }
        }
        let mut ids = BTreeSet::new();
        for s in array(g, "steps")? {
            let sid = s["id"].as_str().ok_or("missing step id")?;
            require(ids.insert(sid), "duplicate step")?;
            let cmd = s.get("command").filter(|v| !v.is_null());
            let gate = s.get("gate").filter(|v| !v.is_null());
            require(cmd.is_some() != gate.is_some(), "ambiguous step")?;
            if let Some(c) = cmd {
                command(c)?;
            }
            if let Some(n) = gate {
                require(fresh.is_null(), "fresh gate cannot delegate lifecycle")?;
                require(
                    n.as_str().is_some_and(|s| gates.contains_key(s)),
                    "unknown nested gate",
                )?;
            }
        }
    }
    fn visit(
        id: &str,
        g: &serde_json::Map<String, Value>,
        stack: &mut BTreeSet<String>,
    ) -> Result<()> {
        require(stack.insert(id.into()), "integration gate cycle")?;
        for s in array(&g[id], "steps")? {
            if let Some(n) = s["gate"].as_str() {
                visit(n, g, stack)?;
            }
        }
        stack.remove(id);
        Ok(())
    }
    for id in gates.keys() {
        visit(id, gates, &mut BTreeSet::new())?;
    }
    Ok(())
}
pub fn identity(root: &Path) -> Result<(Option<String>, String)> {
    let head = compatibility::git(root, &["rev-parse", "HEAD"]).unwrap_or("unknown".into());
    let Some(raw) = std::env::var("AI_PLATFORM_COMPATIBILITY_MANIFEST_PATH")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return Ok((None, head));
    };
    let m = load(Path::new(&raw))?;
    require(
        ["release_candidate", "released"].contains(&text(&m, "status")?),
        "runtime manifest not candidate",
    )?;
    let release = text(&m, "release_id")?.to_string();
    let source = text(&m["source"], "git_sha")?.to_string();
    require(
        hex(&source, 40) && source == head,
        "checkout/source identity mismatch",
    )?;
    require(
        compatibility::git(root, &["status", "--porcelain=v1", "--untracked-files=all"])?
            .is_empty(),
        "release integration requires clean checkout",
    )?;
    Ok((Some(release), source))
}
pub fn default_receipt(root: &Path, id: &str, sha: &str) -> Result<std::path::PathBuf> {
    require(hex(sha, 40), "receipt requires candidate SHA")?;
    require(
        regex::Regex::new(r"^[a-z0-9][a-z0-9-]*$")?.is_match(id),
        "unsafe gate id",
    )?;
    Ok(root
        .join("reports/release/integration-gates")
        .join(sha)
        .join(format!("{id}.json")))
}
fn write(path: &Path, v: &Value) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&compatibility::canonical(v))?
        ),
    )?;
    Ok(())
}
fn elapsed(t: Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 1000.).round() / 1000.
}
fn record(id: &str, cmd: &[String], exit: i32, output: &str, start: Instant) -> Value {
    use sha2::{Digest, Sha256};
    let skips = regex::Regex::new(r"(?i)\b(?:SKIP(?:PED)?|NOT APPLICABLE)\b")
        .unwrap()
        .find_iter(output)
        .count();
    json!({"id":id,"command":cmd,"exit_code":exit,"duration_seconds":elapsed(start),"skip_markers":skips,"output_sha256":format!("{:x}",Sha256::digest(output.as_bytes()))})
}
fn execute(cmd: &[String], root: &Path, env: &Env) -> (i32, String) {
    match std::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .current_dir(root)
        .env_clear()
        .envs(env)
        .output()
    {
        Ok(o) => {
            let output = format!(
                "{}\n{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
            .trim()
            .to_string();
            if !output.is_empty() {
                println!("{output}");
            }
            (o.status.code().unwrap_or(2), output)
        }
        Err(_) => (2, "command unavailable: I/O error".into()),
    }
}
fn failure(r: &Value) -> Option<(&'static str, i32)> {
    if r["skip_markers"].as_u64().unwrap_or(0) > 0 {
        Some(("blocked", 1))
    } else {
        match r["exit_code"].as_i64() {
            Some(0) => None,
            Some(1) => Some(("fail", 1)),
            _ => Some(("blocked", 2)),
        }
    }
}
fn append(receipt: &mut Value, r: Value) {
    let n =
        receipt["unexpected_skips"].as_u64().unwrap_or(0) + r["skip_markers"].as_u64().unwrap_or(0);
    receipt["unexpected_skips"] = n.into();
    receipt["steps"].as_array_mut().unwrap().push(r);
}
fn finish(path: &Path, r: &mut Value, start: Instant, outcome: Option<(&str, i32)>) -> Result<i32> {
    let (status, code) = outcome.unwrap_or(("pass", 0));
    r["result"] = status.into();
    r["duration_seconds"] = elapsed(start).into();
    write(path, r)?;
    if code == 0 {
        println!("{}", serde_json::to_string_pretty(r)?);
    }
    Ok(code)
}
fn generated_name(name: &str) -> Result<()> {
    require(
        regex::Regex::new(r"^arc08_fresh_[0-9a-f]{12}_[0-9a-f]{12}$")?.is_match(name),
        "refusing nongenerated database name",
    )
}
pub fn database_dsn(dsn: &str, name: &str) -> Result<String> {
    generated_name(name)?;
    let mut u = reqwest::Url::parse(dsn).map_err(|_| "invalid database URL")?;
    require(
        ["postgres", "postgresql"].contains(&u.scheme())
            && u.host_str().is_some()
            && u.path().starts_with('/')
            && u.path().len() > 1
            && u.fragment().is_none(),
        "fresh DSN requires explicit PostgreSQL database",
    )?;
    u.set_path(&format!("/{name}"));
    Ok(u.to_string())
}
pub async fn fresh_action(action: &str, name: &str, dsn: &str) -> Result<()> {
    use sqlx::Connection;
    generated_name(name)?;
    let prefix = std::env::var("AI_GATEWAY_ROLE_PREFIX").unwrap_or("ai_gateway_".into());
    require(
        regex::Regex::new(r"^[a-z][a-z0-9_]{0,20}_$")?.is_match(&prefix),
        "unsafe role prefix",
    )?;
    let mut c = sqlx::PgConnection::connect(dsn)
        .await
        .map_err(|_| "fresh database connection failed")?;
    let current: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut c)
        .await
        .map_err(|_| "database identity lookup failed")?;
    require(current != name, "admin DSN points at target")?;
    match action {
        "create" => {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
                    .bind(name)
                    .fetch_one(&mut c)
                    .await
                    .map_err(|_| "database existence lookup failed")?;
            require(!exists, "generated database already exists")?;
            sqlx::query(&format!(
                "CREATE DATABASE \"{name}\" OWNER \"{prefix}owner\" TEMPLATE template0"
            ))
            .execute(&mut c)
            .await
            .map_err(|_| "fresh database create failed")?;
        }
        "drop" => {
            sqlx::query(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
                .execute(&mut c)
                .await
                .map_err(|_| "fresh database cleanup failed")?;
        }
        _ => return Err("unknown database action".into()),
    };
    c.close().await.map_err(|_| "database close failed")?;
    println!("fresh database {action}: {name}");
    Ok(())
}
fn control(action: &str, name: &str, root: &Path, env: &Env, admin: &str) -> (i32, String) {
    let mut env = env.clone();
    env.insert(
        "AI_PLATFORM_FRESH_DATABASE_ADMIN_DSN".into(),
        env[admin].clone(),
    );
    let exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => return (2, "cannot resolve native executable".into()),
    };
    execute(
        &[
            exe,
            "integration".into(),
            "--fresh-database-action".into(),
            action.into(),
            "--fresh-database-name".into(),
            name.into(),
        ],
        root,
        &env,
    )
}
fn external(contract: &Value, release: Option<&str>, source: &str) -> Result<String> {
    let raw = std::env::var(text(contract, "env")?)?;
    let p = Path::new(&raw);
    require(
        p.is_file() && !p.is_symlink(),
        "external receipt unavailable",
    )?;
    let r = load(p)?;
    crate::evidence::receipt(
        &r,
        Some(text(contract, "gate")?),
        release,
        Some(source),
        true,
    )?;
    let executed = array(&r, "steps")?
        .iter()
        .map(|s| serde_json::to_string(&s["command"]))
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    for c in array(contract, "commands")? {
        require(
            executed.contains(&serde_json::to_string(c)?),
            "external receipt omits quickstart command",
        )?;
    }
    digest(p)
}
pub fn run(
    id: &str,
    spec: &Value,
    root: &Path,
    dry: bool,
    path: &Path,
    identity: &(Option<String>, String),
) -> Result<i32> {
    let g = &spec["gates"][id];
    require(g.is_object(), "unknown gate")?;
    let (release, source) = identity;
    let mut r = json!({"schema_version":"ai-platform/integration-gate-receipt/v1","gate":id,"tier":g["tier"],"release_id":release,"source_git_sha":source,"result":"blocked","unexpected_skips":0,"steps":[]});
    let env: Env = std::env::vars().collect();
    let missing = array(g, "required_env")?
        .iter()
        .filter_map(|v| v.as_str())
        .filter(|n| {
            env.get(*n)
                .is_none_or(|s| s.is_empty() || (n.ends_with("_AUTHORIZED") && s != "1"))
        })
        .map(|n| format!("missing required environment name: {n}"))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        r["blockers"] = json!(missing);
        write(path, &r)?;
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(2);
    }
    if dry {
        r["result"] = "dry-run".into();
        r["steps"] = json!(
            array(g, "steps")?
                .iter()
                .map(|s| json!({"id":s["id"],"command":s["command"],"gate":s["gate"]}))
                .collect::<Vec<_>>()
        );
        if !g["fresh_database"].is_null() {
            r["fresh_environment"] = json!({"kind":"scratch-postgresql-database","database_name":"generated-at-runtime","cleanup_result":"not-run"});
        }
        write(path, &r)?;
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(0);
    }
    if !g["fresh_database"].is_null() {
        return fresh(g, &mut r, root, path, identity, &env);
    }
    let start = Instant::now();
    for s in array(g, "steps")? {
        let at = Instant::now();
        let (cmd, exit, output) = if let Some(n) = s["gate"].as_str() {
            let filename = format!(
                "{}-{n}.json",
                path.file_stem().unwrap_or_default().to_string_lossy()
            );
            let p = path.with_file_name(filename);
            let code = run(n, spec, root, false, &p, identity)?;
            (
                vec!["integration-gate".into(), n.into()],
                code,
                std::fs::read_to_string(p).unwrap_or_default(),
            )
        } else {
            let c = command(&s["command"])?;
            let (e, o) = execute(&c, root, &env);
            (c, e, o)
        };
        let step = record(text(s, "id")?, &cmd, exit, &output, at);
        let fail = failure(&step);
        append(&mut r, step);
        if fail.is_some() {
            return finish(path, &mut r, start, fail);
        }
    }
    finish(path, &mut r, start, None)
}
fn fresh(
    g: &Value,
    r: &mut Value,
    root: &Path,
    path: &Path,
    identity: &(Option<String>, String),
    env: &Env,
) -> Result<i32> {
    let start = Instant::now();
    let (release, sha) = identity;
    require(hex(sha, 40), "fresh database requires exact SHA")?;
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("arc08_fresh_{}_{}", &sha[..12], &suffix[..12]);
    let f = &g["fresh_database"];
    let admin = text(f, "admin_dsn_env")?;
    let migrator = text(f, "migrator_dsn_env")?;
    let mut env = env.clone();
    let dsn = database_dsn(env.get(migrator).ok_or("migrator DSN missing")?, &name)?;
    for k in [
        "AI_GATEWAY_DATABASE_MIGRATOR_DSN",
        "DATABASE_URL",
        "GATEWAY_DATABASE__DSN",
    ] {
        env.insert(k.into(), dsn.clone());
    }
    env.insert("AI_PLATFORM_FRESH_DATABASE_NAME".into(), name.clone());
    r["fresh_environment"] = json!({"kind":"scratch-postgresql-database","database_name":name,"cleanup_result":"pending"});
    if !g["external_receipt"].is_null() {
        let at = Instant::now();
        let (e, o) = match external(&g["external_receipt"], release.as_deref(), sha) {
            Ok(o) => (0, o),
            Err(_) => (2, "external fresh-runtime receipt validation failed".into()),
        };
        let step = record(
            "fresh-runtime-receipt",
            &[
                "verify-external-receipt".into(),
                text(&g["external_receipt"], "gate")?.into(),
            ],
            e,
            &o,
            at,
        );
        let fail = failure(&step);
        append(r, step);
        if fail.is_some() {
            return finish(path, r, start, fail);
        }
    }
    let at = Instant::now();
    let (e, o) = control("create", &name, root, &env, admin);
    let step = record(
        "fresh-database-create",
        &["fresh-database".into(), "create".into(), name.clone()],
        e,
        &o,
        at,
    );
    let mut outcome = failure(&step);
    append(r, step);
    if outcome.is_none() {
        for s in array(g, "steps")? {
            let at = Instant::now();
            let c = command(&s["command"])?;
            let (e, o) = execute(&c, root, &env);
            let step = record(text(s, "id")?, &c, e, &o, at);
            outcome = failure(&step);
            append(r, step);
            if outcome.is_some() {
                break;
            }
        }
    }
    // Cleanup only a database this invocation successfully created; never remove a colliding existing name.
    if e == 0 {
        let at = Instant::now();
        let (e, o) = control("drop", &name, root, &env, admin);
        let step = record(
            "fresh-database-drop",
            &["fresh-database".into(), "drop".into(), name.clone()],
            e,
            &o,
            at,
        );
        let failed = failure(&step).is_some();
        append(r, step);
        r["fresh_environment"]["cleanup_result"] = if failed { "blocked" } else { "pass" }.into();
        if failed {
            outcome = Some(("blocked", 2));
        }
    } else {
        r["fresh_environment"]["cleanup_result"] = "not-created".into();
    }
    finish(path, r, start, outcome)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cycles_rejected() {
        let s = json!({"schema_version":"ai-platform/integration-gates/v1","gates":{"a":{"tier":"L2","required_env":[],"steps":[{"id":"s","gate":"a"}]}}});
        assert!(validate_spec(&s).is_err());
    }
    #[test]
    fn only_generated_database_targets() {
        assert!(database_dsn("postgres://localhost/main", "main").is_err());
        let s = database_dsn(
            "postgres://localhost/main?sslmode=disable",
            "arc08_fresh_aaaaaaaaaaaa_bbbbbbbbbbbb",
        )
        .unwrap();
        assert_eq!(
            s,
            "postgres://localhost/arc08_fresh_aaaaaaaaaaaa_bbbbbbbbbbbb?sslmode=disable"
        );
    }
    #[test]
    fn skip_never_passes() {
        let r = record("x", &["test".into()], 0, "1 SKIPPED", Instant::now());
        assert_eq!(failure(&r), Some(("blocked", 1)));
    }
}
