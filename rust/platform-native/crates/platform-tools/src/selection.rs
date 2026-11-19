//! Fail-closed changed-path gate selection. Git is read-only and NUL-delimited.
use crate::*;
use std::{collections::BTreeSet, process::Command};
fn glob(pattern: &str) -> Result<regex::Regex> {
    let chars: Vec<_> = pattern.chars().collect();
    let mut i = 0;
    let mut result = String::from("^");
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') && chars.get(i + 2) == Some(&'/') => {
                result.push_str("(?:[^/]+/)*");
                i += 3;
            }
            '*' if chars.get(i + 1) == Some(&'*') => {
                result.push_str(".*");
                i += 2;
            }
            '*' => {
                result.push_str("[^/]*");
                i += 1;
            }
            '?' => {
                result.push_str("[^/]");
                i += 1;
            }
            ch => {
                result.push_str(&regex::escape(&ch.to_string()));
                i += 1;
            }
        }
    }
    result.push('$');
    Ok(regex::Regex::new(&result)?)
}
fn select(
    gates: &serde_json::Map<String, Value>,
    paths: &[String],
) -> Result<(BTreeMap<String, Value>, Vec<String>)> {
    let mut compiled = vec![];
    for (name, fields) in gates {
        for trigger in fields["triggers"]
            .as_array()
            .ok_or("gate triggers must be an array")?
        {
            let trigger = trigger.as_str().ok_or("trigger must be a string")?;
            compiled.push((name, trigger, glob(trigger)?));
        }
    }
    let mut selected: BTreeMap<String, Value> = BTreeMap::new();
    let mut ungated = vec![];
    for path in paths {
        let mut matched = false;
        for (name, trigger, regex) in &compiled {
            if !regex.is_match(path) {
                continue;
            }
            matched = true;
            let entry=selected.entry((*name).clone()).or_insert_with(||{let fields=&gates[*name];json!({"make":fields["make"],"shell":fields["shell"],"tier":fields["tier"],"required_on":fields["required_on"].as_array().cloned().unwrap_or_default(),"resource":fields["resource"],"ci_job":fields["ci_job"],"matched_paths":[],"matched_triggers":[]})});
            entry["matched_paths"]
                .as_array_mut()
                .unwrap()
                .push(json!(path));
            let triggers = entry["matched_triggers"].as_array_mut().unwrap();
            if !triggers.contains(&json!(trigger)) {
                triggers.push(json!(trigger));
                triggers.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
            }
        }
        if !matched {
            ungated.push(path.clone());
        }
    }
    Ok((selected, ungated))
}
pub fn changed(root: &Path, base: &str) -> Result<Vec<String>> {
    require(
        !base.is_empty() && !base.starts_with('-'),
        "invalid base SHA",
    )?;
    let run = |args: &[&str]| -> Result<Vec<u8>> {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()?;
        require(out.status.success(), "Git changed-path query failed")?;
        Ok(out.stdout)
    };
    run(&["rev-parse", "--verify", "--quiet", base])?;
    let mut paths = BTreeSet::new();
    for bytes in [
        run(&["diff", "--name-only", "-z", base, "--"])?,
        run(&["ls-files", "--others", "--exclude-standard", "-z"])?,
    ] {
        for path in bytes.split(|c| *c == 0).filter(|p| !p.is_empty()) {
            paths.insert(std::str::from_utf8(path)?.to_owned());
        }
    }
    Ok(paths.into_iter().collect())
}
pub fn run(args: &Args) -> Result<Value> {
    if args.flag("--selftest") {
        let gates = json!({"a":{"triggers":["src/**"]}});
        let (selected, ungated) = select(
            gates.as_object().unwrap(),
            &["src/main.rs".into(), "unknown".into()],
        )?;
        require(
            selected.len() == 1 && ungated == ["unknown"],
            "selection selftest failed",
        )?;
        return Ok(json!({"result":"pass","selftest":"affected-gates"}));
    }
    let root = args.root();
    let base = args.get("--base")?;
    let harness = args
        .values
        .get("--harness")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("harness.yml"));
    let config: Value = serde_yaml::from_str(&std::fs::read_to_string(harness)?)?;
    let gates = config["gates"]
        .as_object()
        .filter(|g| !g.is_empty())
        .ok_or("no gates found")?;
    let paths = changed(&root, base)?;
    let (selected, ungated) = select(gates, &paths)?;
    let value = json!({"gate":"affected-gates","tier":"L0-selector","base":base,"changed_paths":paths,"selected":selected,"ungated_paths":ungated,"result":if ungated.is_empty(){"pass"}else{"fail"}});
    let evidence = args
        .values
        .get("--evidence-out")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("tmp/gate-evidence/affected-gates.json"));
    write_json(&evidence, &value)?;
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn glob_parity() {
        for (pattern, path, expected) in [
            ("src/*.rs", "src/a/b.rs", false),
            ("src/**", "src/a/b.rs", true),
            ("**/state.json", "state.json", true),
            ("**/state.json", "a/b/state.json", true),
            ("a?.sql", "a1.sql", true),
            ("[a]", "a", false),
            ("[a]", "[a]", true),
        ] {
            assert_eq!(glob(pattern).unwrap().is_match(path), expected);
        }
    }
    #[test]
    fn overlapping_triggers_and_ungated() {
        let gates = json!({"a":{"triggers":["src/**","**/*.rs"],"required_on":["change"]},"b":{"triggers":["harness.yml"]}});
        let (selected, ungated) = select(
            gates.as_object().unwrap(),
            &["src/a.rs".into(), "README.md".into()],
        )
        .unwrap();
        assert_eq!(
            selected["a"]["matched_paths"],
            json!(["src/a.rs", "src/a.rs"])
        );
        assert_eq!(
            selected["a"]["matched_triggers"],
            json!(["**/*.rs", "src/**"])
        );
        assert_eq!(ungated, ["README.md"]);
    }
}
