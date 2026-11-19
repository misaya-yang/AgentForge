use ai_platform_release::*;
use serde_json::json;
async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args
        .first()
        .ok_or("usage: platform-release evidence|compatibility [options]")?;
    if command == "version-agreement" {
        require(
            args.len() == 1,
            "version-agreement takes environment configuration",
        )?;
        println!(
            "{}",
            serde_json::to_string_pretty(&version::from_environment().await?)?
        );
        return Ok(());
    }
    if command == "integration" {
        return integration_cli(&args[1..]).await;
    }
    require(
        ["evidence", "compatibility"].contains(&command.as_str()),
        "unknown command",
    )?;
    let mut opts = std::collections::BTreeMap::new();
    let mut write = false;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--write" {
            write = true;
            i += 1;
            continue;
        }
        require(
            [
                "--repo-root",
                "--level",
                "--matrix",
                "--retirement",
                "--manifest",
                "--inputs",
                "--source-rev",
            ]
            .contains(&args[i].as_str()),
            "unknown option",
        )?;
        let value = args.get(i + 1).ok_or("missing option value")?;
        require(
            opts.insert(args[i].clone(), value.clone()).is_none(),
            "duplicate option",
        )?;
        i += 2;
    }
    let root = opts
        .get("--repo-root")
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?;
    let level = opts.get("--level").map(String::as_str).unwrap_or("draft");
    let path = |key: &str, default: &str| {
        opts.get(key)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join(default))
    };
    let output = if command == "evidence" {
        require(!write, "evidence does not support --write")?;
        let schemas = evidence::schemas(&root)?;
        let m = evidence::matrix(
            &root,
            &load(&path(
                "--matrix",
                "deploy/release/release-rollback-matrix.json",
            ))?,
            level,
            None,
            None,
        )?;
        let r = evidence::retirement(
            &root,
            &load(&path(
                "--retirement",
                "deploy/release/historical-plan-retirement.json",
            ))?,
        )?;
        json!({"result":m["status"],"level":level,"matrix":m,"retirement":r,"schema_sha256":schemas})
    } else {
        let destination = path("--manifest", "deploy/release/compatibility-manifest.json");
        if write {
            let rev = opts
                .get("--source-rev")
                .ok_or("--write requires --source-rev")?;
            let inputs = opts.get("--inputs").ok_or("--write requires --inputs")?;
            let v = compatibility::generate(&root, rev, &load(std::path::Path::new(inputs))?)?;
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(
                &destination,
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&compatibility::canonical(&v))?
                ),
            )?;
            json!({"written":destination})
        } else {
            compatibility::validate(&root, &load(&destination)?, level)?
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("RELEASE ERROR: {e}");
        std::process::exit(2)
    }
}

async fn integration_cli(args: &[String]) -> Result<()> {
    let mut options = std::collections::BTreeMap::new();
    let mut dry = false;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--dry-run" {
            dry = true;
            i += 1;
            continue;
        }
        require(
            [
                "--gate",
                "--fresh-database-action",
                "--fresh-database-name",
                "--spec",
                "--repo-root",
                "--receipt",
            ]
            .contains(&args[i].as_str()),
            "unknown integration option",
        )?;
        let v = args.get(i + 1).ok_or("missing option value")?;
        require(
            options.insert(args[i].clone(), v.clone()).is_none(),
            "duplicate option",
        )?;
        i += 2;
    }
    require(
        options.contains_key("--gate") != options.contains_key("--fresh-database-action"),
        "choose gate or fresh database action",
    )?;
    if let Some(action) = options.get("--fresh-database-action") {
        let name = options
            .get("--fresh-database-name")
            .ok_or("fresh database name required")?;
        let dsn = std::env::var("AI_PLATFORM_FRESH_DATABASE_ADMIN_DSN")
            .map_err(|_| "fresh admin DSN missing")?;
        return integration::fresh_action(action, name, &dsn).await;
    }
    let root = options
        .get("--repo-root")
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?;
    let spec = options
        .get("--spec")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("deploy/release/integration-gates.json"));
    let spec = load(&spec)?;
    integration::validate_spec(&spec)?;
    let identity = integration::identity(&root)?;
    let gate = &options["--gate"];
    let receipt = if let Some(p) = options.get("--receipt") {
        std::path::PathBuf::from(p)
    } else {
        integration::default_receipt(&root, gate, &identity.1)?
    };
    let code = integration::run(gate, &spec, &root, dry, &receipt, &identity)?;
    if code != 0 {
        std::process::exit(code)
    }
    Ok(())
}
