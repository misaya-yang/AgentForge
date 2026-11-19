//! Portable durable evidence validation and explicitly authorized reversible quarantine.
use crate::*;
use std::{
    collections::BTreeSet,
    os::unix::fs::MetadataExt,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing string {k}").into())
}
fn strings(v: &Value) -> Result<Vec<&str>> {
    v.as_array()
        .ok_or("expected array")?
        .iter()
        .map(|v| v.as_str().ok_or_else(|| "expected string".into()))
        .collect()
}
fn relative(value: &str) -> Result<&str> {
    require(
        !value.contains(['*', '?', '[', ']', '{', '}'])
            && !value.is_empty()
            && !value.contains(['\\', '\0', ':'])
            && !value.starts_with('/')
            && value.split('/').all(|p| !matches!(p, "" | "." | "..")),
        "explicit safe relative path required",
    )?;
    Ok(value)
}
fn git(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?)
}
fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    let out = git(root, args)?;
    require(out.status.success(), "Git evidence identity failed")?;
    Ok(String::from_utf8(out.stdout)?.trim().into())
}
fn policy(root: &Path, path: &Path) -> Result<Value> {
    require(
        path.is_file() && !path.is_symlink(),
        "policy must be real file",
    )?;
    let policy_rel = path
        .canonicalize()?
        .strip_prefix(root)?
        .to_string_lossy()
        .into_owned();
    let mut p = object(path)?;
    require(
        p["schema_version"] == "ai-platform/evidence-policy/v1",
        "policy schema invalid",
    )?;
    let scratch = p["scratch_roots"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("scratch roots required")?;
    let mut roots = BTreeSet::new();
    for item in scratch {
        let path = relative(text(item, "path")?)?;
        require(
            ![".git", "tmp", "web"].contains(&path)
                && item["min_age_seconds"].as_u64().is_some()
                && roots.insert(path.to_owned()),
            "unsafe scratch policy",
        )?;
    }
    for path in strings(&p["durable_roots"])? {
        relative(path)?;
        require(
            ![".git", "reports"].contains(&path) && roots.insert(path.to_owned()),
            "unsafe durable policy",
        )?;
    }
    let forbidden: BTreeSet<_> = strings(&p["forbidden_path_parts"])?
        .iter()
        .map(|s| s.to_lowercase())
        .collect();
    require(
        [".env", ".playwright", "auth", "storage-state"]
            .iter()
            .all(|s| forbidden.contains(*s)),
        "weakened authentication protections",
    )?;
    let restricted: BTreeSet<_> = strings(&p["restricted_suffixes"])?
        .iter()
        .map(|s| s.to_lowercase())
        .collect();
    require(
        [".har", ".trace", ".zip", ".webm", ".mp4", ".log"]
            .iter()
            .all(|s| restricted.contains(*s)),
        "weakened raw evidence protections",
    )?;
    require(
        p["cleanup"]
            == json!({"default":"dry-run","apply_requires_authorization_manifest":true,"apply_requires_external_quarantine":true}),
        "cleanup must remain dry-run and fail closed",
    )?;
    safe(root, relative(text(&p, "manifest")?)?)?;
    p["_validated_policy_path"] = json!(policy_rel);
    Ok(p)
}
fn references(root: &Path, p: &Value) -> Result<Vec<(String, String)>> {
    let mut prefixes: Vec<String> = strings(&p["durable_roots"])?
        .iter()
        .map(|s| format!("{s}/"))
        .collect();
    for item in p["scratch_roots"].as_array().unwrap() {
        prefixes.push(format!("{}/", text(item, "path")?));
    }
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in strings(&p["reference_globs"])? {
        builder.add(
            globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()?,
        );
    }
    let patterns = builder.build()?;
    let mut found = BTreeSet::new();
    for path in walk(root, &[".git", "target", "node_modules", ".venv"], false)? {
        if !patterns.is_match(&path) {
            continue;
        }
        let data = object(&safe(root, &path.to_string_lossy())?)?;
        let encoded = serde_json::to_string(&data)?;
        for prefix in &prefixes {
            let re = regex::Regex::new(&format!(r#"{}[^\s"';,\]]+"#, regex::escape(prefix)))?;
            for m in re.find_iter(&encoded) {
                found.insert((path.to_string_lossy().into_owned(), m.as_str().to_owned()));
            }
        }
    }
    Ok(found.into_iter().collect())
}
pub(crate) fn timestamp(s: &str) -> bool {
    // ISO8601 with explicit offset, Gregorian date and valid clock components.
    let re = regex::Regex::new(
        r"^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})(?::(\d{2})(?:\.\d+)?)?(Z|[+-]\d{2}:\d{2})$",
    )
    .unwrap();
    let Some(c) = re.captures(s) else {
        return false;
    };
    let n = |i| {
        c.get(i)
            .map(|m| m.as_str().parse::<u32>().unwrap())
            .unwrap_or(0)
    };
    let (y, m, d) = (n(1), n(2), n(3));
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 0,
    };
    let zone = &c[7];
    y > 0
        && d > 0
        && d <= days
        && n(4) < 24
        && n(5) < 60
        && n(6) < 60
        && (zone == "Z"
            || (zone[1..3].parse::<u32>().unwrap() < 24 && zone[4..].parse::<u32>().unwrap() < 60))
}
fn validate(root: &Path, p: &Value) -> Result<Value> {
    let path = safe(root, text(p, "manifest")?)?;
    let manifest = object(&path)?;
    require(
        manifest["schema_version"] == "ai-platform/evidence-manifest/v1"
            && manifest["policy"] == p["_validated_policy_path"],
        "manifest schema/policy mismatch",
    )?;
    let entries = manifest["entries"]
        .as_array()
        .ok_or("manifest entries missing")?;
    let required = [
        "id",
        "owner",
        "evidence_tier",
        "media_type",
        "bytes",
        "generated_at",
        "retention",
        "source_git_sha",
        "command",
        "scenario",
        "sha256",
        "viewport",
        "redaction_reviewer",
        "access_policy",
    ];
    let mut ids = BTreeSet::new();
    let mut durable = BTreeSet::new();
    let hex = |s: &str, len: usize| {
        s.len() == len
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    };
    for entry in entries {
        let fields = entry.as_object().ok_or("entry must be object")?;
        require(
            required.iter().all(|k| fields.contains_key(*k))
                && fields.keys().all(|k| {
                    required.contains(&k.as_str())
                        || ["repository_path", "uri"].contains(&k.as_str())
                }),
            "entry field contract mismatch",
        )?;
        let id = text(entry, "id")?;
        require(ids.insert(id), "duplicate evidence ID")?;
        require(
            ["L0", "L1", "L2", "L3"].contains(&text(entry, "evidence_tier")?),
            "invalid evidence tier",
        )?;
        for key in [
            "owner",
            "media_type",
            "generated_at",
            "retention",
            "command",
            "scenario",
            "redaction_reviewer",
            "access_policy",
        ] {
            text(entry, key)?;
        }
        require(
            timestamp(text(entry, "generated_at")?),
            "generation timestamp requires valid timezone",
        )?;
        require(
            entry["viewport"].is_null()
                || entry["viewport"].is_object()
                    && ["width", "height"]
                        .iter()
                        .all(|k| entry["viewport"][k].as_u64().is_some_and(|v| v > 0)),
            "invalid viewport",
        )?;
        require(
            entry["bytes"].as_u64().is_some_and(|v| v > 0),
            "invalid byte length",
        )?;
        let source = text(entry, "source_git_sha")?;
        require(
            hex(source, 40) && hex(text(entry, "sha256")?, 64),
            "invalid provenance hashes",
        )?;
        require(
            git_text(root, &["rev-parse", &format!("{source}^{{commit}}")])? == source
                && git(root, &["merge-base", "--is-ancestor", source, "HEAD"])?
                    .status
                    .success(),
            "source Git commit unavailable or not ancestor",
        )?;
        let repository = entry["repository_path"].as_str().filter(|s| !s.is_empty());
        let uri = entry["uri"].as_str().filter(|s| !s.is_empty());
        require(
            repository.is_some() != uri.is_some(),
            "exactly one path or URI required",
        )?;
        if let Some(rel) = repository {
            relative(rel)?;
            require(
                strings(&p["durable_roots"])?
                    .iter()
                    .any(|r| rel.starts_with(&format!("{r}/"))),
                "outside durable roots",
            )?;
            let artifact = safe(root, rel)?;
            require(
                artifact.is_file()
                    && std::fs::metadata(&artifact)?.len() == entry["bytes"].as_u64().unwrap()
                    && digest(&artifact)? == entry["sha256"],
                "durable evidence size/hash drift",
            )?;
            require(
                git(root, &["cat-file", "-e", &format!("HEAD:{rel}")])?
                    .status
                    .success()
                    && git(root, &["diff", "--quiet", "HEAD", "--", rel])?
                        .status
                        .success(),
                "durable evidence must be committed clean",
            )?;
            require(
                !text(entry, "media_type")?.starts_with("image/") || !entry["viewport"].is_null(),
                "image evidence viewport required",
            )?;
            require(
                entry["access_policy"] == "repository",
                "repository access policy required",
            )?;
            durable.insert(rel);
        } else {
            let uri = uri.unwrap();
            let rest = uri.strip_prefix("sealed://").ok_or("sealed URI required")?;
            let (host, path) = rest.split_once('/').ok_or("sealed URI artifact missing")?;
            require(
                !host.is_empty()
                    && !path.is_empty()
                    && !uri.contains(['@', '?', '#'])
                    && entry["access_policy"] != "repository",
                "invalid sealed evidence URI",
            )?;
        }
    }
    for (source, reference) in references(root, p)? {
        require(
            !p["scratch_roots"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| reference.starts_with(&format!("{}/", v["path"].as_str().unwrap()))),
            format!("oracle {source} references scratch evidence"),
        )?;
        if strings(&p["durable_roots"])?
            .iter()
            .any(|r| reference.starts_with(&format!("{r}/")))
        {
            require(
                durable.contains(reference.as_str()),
                "oracle durable evidence absent from manifest",
            )?;
        }
    }
    Ok(json!({"entries":entries.len(),"manifest_sha256":digest(&path)?}))
}
fn repo_identity(root: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    let common = resolve_path(
        root.join(git_text(root, &["rev-parse", "--git-common-dir"])?)
            .to_str()
            .ok_or("nonUTF8 path")?,
    )?;
    let worktrees = git_text(root, &["worktree", "list", "--porcelain"])?
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(resolve_path)
        .collect::<Result<Vec<_>>>()?;
    Ok((common, worktrees))
}
fn classify(root: &Path, p: &Value, rel: &str, refs: &BTreeSet<String>) -> Result<Value> {
    relative(rel)?;
    let candidate = root.join(rel);
    let m = match std::fs::symlink_metadata(&candidate) {
        Ok(m) => m,
        Err(e) => {
            return Ok(
                json!({"path":rel,"eligible":false,"reasons":[format!("missing/unreadable: {e}")]}),
            );
        }
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let age = now.saturating_sub(m.mtime().max(0) as u64);
    if m.file_type().is_symlink() {
        return Ok(
            json!({"path":rel,"bytes":null,"age_seconds":age,"eligible":false,"reasons":["symlink"],"sha256":null}),
        );
    }
    let (common, worktrees) = repo_identity(root)?;
    let resolved = resolve_path(candidate.to_str().ok_or("nonUTF8 path")?)?;
    let mut reasons = vec![];
    if safe(root, rel).is_err() {
        reasons.push("symlinked or unsafe ancestor".into());
    }
    if resolved == root {
        reasons.push("repository root".to_owned());
    }
    if resolved.starts_with(common) {
        reasons.push("Git common-dir".into());
    }
    if worktrees
        .iter()
        .any(|w| w != root && resolved.starts_with(w))
    {
        reasons.push("another Git worktree".into());
    }
    if !resolved.starts_with(root) {
        reasons.push("external path".into());
    }
    if m.dev() != std::fs::metadata(root)?.dev() {
        reasons.push("external mount".into());
    }
    let minimum = p["scratch_roots"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| rel.starts_with(&format!("{}/", v["path"].as_str().unwrap())))
        .map(|v| v["min_age_seconds"].as_u64().unwrap())
        .max();
    if minimum.is_none() {
        reasons.push("outside scratch allowlist".into());
    }
    for part in strings(&p["forbidden_path_parts"])? {
        let part = part.to_lowercase();
        if rel
            .split('/')
            .any(|p| p.to_lowercase() == part || p.to_lowercase().starts_with(&format!("{part}.")))
        {
            reasons.push(format!("forbidden path part {part}"));
        }
    }
    let suffix = candidate
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    if strings(&p["restricted_suffixes"])?.contains(&suffix.as_str()) {
        reasons.push("restricted raw evidence suffix".into());
    }
    if !m.is_file() {
        reasons.push("not a regular file".into());
    }
    if git(root, &["ls-files", "--error-unmatch", "--", rel])?
        .status
        .success()
    {
        reasons.push("committed/tracked".into());
    }
    if refs.contains(rel) {
        reasons.push("referenced evidence".into());
    }
    if age < minimum.unwrap_or(0) {
        reasons.push(format!("younger than {}s", minimum.unwrap()));
    }
    let sha = if reasons.is_empty() {
        Some(digest(&candidate)?)
    } else {
        None
    };
    Ok(
        json!({"path":rel,"bytes":if m.is_file(){Some(m.len())}else{None},"age_seconds":age,"eligible":reasons.is_empty(),"reasons":reasons,"sha256":sha}),
    )
}
pub fn run(args: &Args) -> Result<Value> {
    let root = args.root().canonicalize()?;
    let action = args
        .positionals
        .get(1)
        .map(String::as_str)
        .unwrap_or("validate");
    require(
        ["validate", "status", "cleanup"].contains(&action),
        "unknown evidence operation",
    )?;
    require(
        !args.flag("--apply") || action == "cleanup",
        "--apply only valid for cleanup",
    )?;
    let p = policy(
        &root,
        &args
            .values
            .get("--policy")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("deploy/evidence/policy.json")),
    )?;
    let validated = validate(&root, &p)?;
    let result = if action == "validate" {
        json!({"action":action,"result":"pass","entries":validated["entries"],"manifest_sha256":validated["manifest_sha256"]})
    } else {
        let mut paths = args.multiple.get("--path").cloned().unwrap_or_default();
        if paths.is_empty() {
            for item in p["scratch_roots"].as_array().unwrap() {
                let rel = text(item, "path")?;
                let path = root.join(rel);
                if path.is_dir() && !path.is_symlink() {
                    for child in walk(&path, &[], true)? {
                        paths.push(format!("{rel}/{}", child.to_string_lossy()));
                    }
                }
            }
            paths.sort();
            paths.dedup();
        }
        let mut refs: BTreeSet<_> = references(&root, &p)?.into_iter().map(|(_, r)| r).collect();
        for entry in object(&safe(&root, text(&p, "manifest")?)?)?["entries"]
            .as_array()
            .unwrap()
        {
            if let Some(path) = entry["repository_path"].as_str() {
                refs.insert(path.into());
            }
        }
        let records = paths
            .iter()
            .map(|path| classify(&root, &p, path, &refs))
            .collect::<Result<Vec<_>>>()?;
        let mut result = json!({"action":action,"mode":if args.flag("--apply"){"apply"}else{"dry-run"},"records":records,"result":"dry-run"});
        if args.flag("--apply") {
            require(
                !records.is_empty() && records.iter().all(|r| r["eligible"] == true),
                "cleanup refuses ineligible or empty selections",
            )?;
            let authorization = object(Path::new(args.get("--authorization")?))?;
            require(
                authorization["schema_version"] == "ai-platform/evidence-cleanup-authorization/v1"
                    && authorization["approved_by"] == "user",
                "explicit user authorization manifest required",
            )?;
            let run = text(&authorization, "run_id")?;
            require(
                regex::Regex::new(r"^[a-zA-Z0-9._-]{1,64}$")?.is_match(run)
                    && run != "."
                    && run != "..",
                "invalid cleanup run ID",
            )?;
            let expected: BTreeMap<_, _> = records
                .iter()
                .map(|r| (r["path"].as_str().unwrap(), r["sha256"].as_str().unwrap()))
                .collect();
            let approved = authorization["artifacts"]
                .as_array()
                .ok_or("authorization artifacts missing")?;
            let actual = approved
                .iter()
                .map(|a| Ok((text(a, "path")?, text(a, "sha256")?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            require(
                expected == actual && actual.len() == approved.len(),
                "authorization must exactly match eligible artifacts",
            )?;
            let quarantine = Path::new(args.get("--quarantine")?);
            require(
                quarantine.is_dir() && !quarantine.is_symlink(),
                "existing real quarantine directory required",
            )?;
            let quarantine = quarantine.canonicalize()?;
            let (_, mut worktrees) = repo_identity(&root)?;
            worktrees.push(root.clone());
            require(
                worktrees
                    .iter()
                    .all(|w| !w.starts_with(&quarantine) && !quarantine.starts_with(w)),
                "quarantine must be external to every worktree",
            )?;
            let dest = quarantine.join(run);
            require(!dest.exists(), "quarantine run already exists")?;
            std::fs::create_dir(&dest)?;
            let mut moved = vec![];
            for record in &records {
                let rel = record["path"].as_str().unwrap();
                let fresh = classify(&root, &p, rel, &refs)?;
                require(
                    fresh["eligible"] == true && fresh["sha256"] == record["sha256"],
                    "artifact changed before quarantine",
                )?;
                let src = safe(&root, rel)?;
                let target = safe(&dest, rel)?;
                std::fs::create_dir_all(target.parent().unwrap())?;
                std::fs::rename(src, target)?;
                moved.push(rel);
            }
            result["result"] = json!("quarantined");
            result["moved"] = json!(moved);
        }
        result
    };
    if let Some(path) = args.values.get("--output") {
        write_json(Path::new(path), &result)?;
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamps_require_real_dates_and_offsets() {
        for good in ["2026-10-15T10:00:00Z", "2024-02-29T13:25:30.123+08:00"] {
            assert!(timestamp(good));
        }
        for bad in [
            "2026-10-15T10:00:00",
            "2025-02-29T10:00:00Z",
            "2026-10-15T25:00:00Z",
        ] {
            assert!(!timestamp(bad));
        }
    }
    #[test]
    fn explicit_paths_only() {
        for bad in ["../x", "a/*", "/abs", "a//b", "a/./b"] {
            assert!(relative(bad).is_err());
        }
        assert!(relative("tmp/screens/a.png").is_ok());
    }
}
