//! Bounded living-document checks. Dated reports, plans, accepted ADRs and frozen data are excluded.
use crate::*;
pub const LIVE_DOCS: &[&str] = &[
    "docs/harness/architecture.md",
    "CONTRIBUTING.md",
    "DEPLOY.md",
    "docs/deployment/operator-guide.md",
    "database/authority/README.md",
    "deploy/agent-runtime-source/README.md",
    "docs/design/enterprise-performance-contract.md",
    "rust/platform-native/crates/platform-migration-authority/README.md",
    "rust/platform-native/crates/platform-gateway/src/migration_host/bootstrap/README.md",
];

pub fn check(root: &Path) -> Result<Value> {
    let today = chrono::Utc::now().date_naive();
    let date = regex::Regex::new(r"(?m)^\*\*Last verified:\*\* (\d{4}-\d{2}-\d{2})\b")?;
    let scripts = regex::Regex::new(r"\bscripts/[A-Za-z0-9_./-]+\.(?:py|sh)\b")?;
    let links = regex::Regex::new(r"\[[^\]]*\]\(([^)]+)\)")?;
    let commands =
        regex::Regex::new(r"\bplatform-tools ([a-z][a-z0-9-]+)(?:[ \t]+([a-z][a-z0-9-]+))?")?;
    let mut errors = vec![];
    let mut warnings = vec![];
    for file in LIVE_DOCS {
        let path = root.join(file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            errors.push(format!("{file}: living document missing"));
            continue;
        };
        let verified = date
            .captures(&text)
            .and_then(|m| chrono::NaiveDate::parse_from_str(&m[1], "%Y-%m-%d").ok());
        match verified {
            None => errors.push(format!("{file}: missing/invalid Last verified date")),
            Some(day) if day > today => {
                errors.push(format!("{file}: verification date is in the future"))
            }
            Some(day) if today.signed_duration_since(day).num_days() > 30 => warnings.push(
                format!("{file}: source/command review is older than 30 days"),
            ),
            _ => {}
        }
        for script in scripts.find_iter(&text) {
            if !root.join(script.as_str()).is_file() {
                errors.push(format!(
                    "{file}: references a missing script: {}",
                    script.as_str()
                ));
            }
        }
        if text.contains("python -m database.authority")
            || text.contains("python3 -m database.authority")
        {
            errors.push(format!("{file}: retired Python authority invocation"));
        }
        for link in links.captures_iter(&text) {
            let target = &link[1];
            if target.contains("://")
                || target.starts_with(['#', '/'])
                || target.starts_with("mailto:")
            {
                continue;
            }
            if !path
                .parent()
                .unwrap()
                .join(target.split('#').next().unwrap())
                .exists()
            {
                errors.push(format!("{file}: broken local link: {target}"));
            }
        }
        for command in commands.captures_iter(&text) {
            let dispatcher =
                regex::Regex::new(&format!(r#""{}"\s*=>"#, regex::escape(&command[1])))?;
            if !dispatcher.is_match(include_str!("main.rs")) {
                errors.push(format!(
                    "{file}: platform-tools command is not implemented: {}",
                    &command[1]
                ));
            }
            if let Some(operation) = command.get(2) {
                let supported = match &command[1] {
                    "source-lock" => {
                        let main = include_str!("main.rs");
                        let start = regex::Regex::new(r#""source-lock"\s*=>"#)?
                            .find(main)
                            .ok_or("source-lock dispatcher missing")?
                            .end();
                        let end = regex::Regex::new(r#""source-candidate"\s*=>"#)?
                            .find(&main[start..])
                            .ok_or("source-lock dispatcher boundary missing")?
                            .start();
                        regex::Regex::new(&format!(
                            r#""{}"\s*=>"#,
                            regex::escape(operation.as_str())
                        ))?
                        .is_match(&main[start..start + end])
                    }
                    "source-candidate" => {
                        let operations = regex::Regex::new(r"matches!\(operation,\s*([^)]*)\)")?;
                        let names = operations
                            .captures(include_str!("source.rs"))
                            .ok_or("source-candidate operations missing")?;
                        names[1]
                            .split('|')
                            .any(|s| s.trim() == format!("\"{}\"", operation.as_str()))
                    }
                    _ => true,
                };
                if !supported {
                    errors.push(format!(
                        "{file}: nested platform-tools operation is not implemented: {} {}",
                        &command[1],
                        operation.as_str()
                    ));
                }
            }
        }
    }
    Ok(
        json!({"result":if errors.is_empty(){"pass"}else{"fail"},"tier":"L0",
        "documents_checked":LIVE_DOCS.len(),"freshness_policy":"warn after 30 days; dates describe source/command review only",
        "errors":errors,"warnings":warnings,"runtime_acceptance":"not_checked"}),
    )
}
