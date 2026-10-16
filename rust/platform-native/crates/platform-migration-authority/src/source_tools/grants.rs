use super::render_contract;
use crate::{AuthorityError, Result, credentials};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
const ROLES: &[&str] = &[
    "capability_worker",
    "gateway",
    "knowledge_api",
    "knowledge_worker",
    "migrator",
    "runtime",
];
const SCHEMAS: &[&str] = &["assistant", "gateway", "knowledge", "public"];
const PRIVILEGES: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE", "USAGE", "EXECUTE"];
fn fail(s: &str) -> AuthorityError {
    AuthorityError::Refused(s.into())
}
fn strings(v: &Value) -> Result<BTreeSet<String>> {
    v.as_array()
        .ok_or_else(|| fail("policy value must be string list"))?
        .iter()
        .map(|s| {
            s.as_str()
                .map(str::to_owned)
                .ok_or_else(|| fail("policy value must be string list"))
        })
        .collect()
}
fn object(kind: &str, schema: &str, name: &str, args: &str) -> (String, String) {
    let qualified = format!("\"{schema}\".\"{name}\"");
    match kind {
        "function" => ("FUNCTION".into(), format!("{qualified}({args})")),
        "table" | "view" => ("TABLE".into(), qualified),
        _ => (kind.to_uppercase(), qualified),
    }
}
pub fn generate(inventory_bytes: &[u8], policy_bytes: &[u8], prefix: &str) -> Result<String> {
    credentials::validate_role_prefix(prefix)?;
    let matrix = render_contract::load_grant_matrix(inventory_bytes, policy_bytes)?;
    let inventory: Value =
        serde_json::from_slice(inventory_bytes).map_err(|_| fail("invalid inventory JSON"))?;
    let policy: Value =
        serde_json::from_slice(policy_bytes).map_err(|_| fail("invalid grants JSON"))?;
    let mut ownership = policy.clone();
    ownership["schema"] = serde_json::json!("arc03-object-ownership/v1");
    if let Some(rows) = ownership["objects"].as_array_mut() {
        for row in rows {
            row["kind"] = row["inventory_kind"].clone();
            row["name"] = row["inventory_name"].clone();
        }
    }
    render_contract::load_policy_bytes(
        inventory_bytes,
        &serde_json::to_vec(&ownership).map_err(|_| fail("invalid ownership mapping"))?,
    )?;
    // The renderer checks mapping and identifier safety; generation also proves every
    // declared source reader/writer and function-mediated write has reviewed coverage.
    let mut managed = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for row in policy["objects"]
        .as_array()
        .ok_or_else(|| fail("policy objects must be array"))?
    {
        let kind = row["inventory_kind"]
            .as_str()
            .ok_or_else(|| fail("kind absent"))?;
        let name = row["inventory_name"]
            .as_str()
            .ok_or_else(|| fail("inventory name absent"))?;
        if !keys.insert((kind, name)) {
            return Err(fail("duplicate grants policy mapping"));
        }
        let schema = row["schema"]
            .as_str()
            .ok_or_else(|| fail("schema absent"))?;
        let target = row["name"].as_str().ok_or_else(|| fail("name absent"))?;
        let args = row["identity_arguments"].as_str().unwrap_or("");
        if (kind == "function" && !row["identity_arguments"].is_string())
            || (kind != "function" && row.get("identity_arguments").is_some())
        {
            return Err(fail("identity arguments only valid for functions"));
        }
        if !managed.insert((
            kind.to_owned(),
            schema.to_owned(),
            target.to_owned(),
            args.to_owned(),
        )) {
            return Err(fail("duplicate qualified grants object"));
        }
        let evidence = strings(&row["evidence"])?;
        if evidence.is_empty() {
            return Err(fail("ownership evidence missing"));
        }
        let mut readers = BTreeSet::new();
        let mut writers = BTreeSet::new();
        let mut direct_write = false;
        for access in row["access"]
            .as_array()
            .ok_or_else(|| fail("access must be array"))?
        {
            let units = strings(&access["units"])?;
            let roles = strings(&access["roles"])?;
            let privileges = strings(&access["privileges"])?;
            if units.is_empty()
                || roles.is_empty()
                || privileges.is_empty()
                || strings(&access["evidence"])?.is_empty()
            {
                return Err(fail(
                    "access requires units, roles, privileges and evidence",
                ));
            }
            let allowed: &[&str] = match kind {
                "table" => &["SELECT", "INSERT", "UPDATE", "DELETE"],
                "view" => &["SELECT"],
                "sequence" => &["USAGE", "SELECT"],
                "function" => &["EXECUTE"],
                "type" => &["USAGE"],
                _ => return Err(fail("unknown object kind")),
            };
            if privileges.iter().any(|p| !allowed.contains(&p.as_str()))
                || roles
                    .iter()
                    .any(|r| !credentials::APPLICATION_PRINCIPALS.contains(&r.as_str()))
            {
                return Err(fail("invalid role or object privilege"));
            }
            if privileges.contains("SELECT") {
                readers.extend(units.clone());
            }
            if ["INSERT", "UPDATE", "DELETE"]
                .iter()
                .any(|p| privileges.contains(*p))
            {
                writers.extend(units);
                direct_write = true;
            }
        }
        if kind == "table" {
            let table = inventory["postgresql"]["tables"]
                .as_array()
                .ok_or_else(|| fail("tables must be array"))?
                .iter()
                .find(|t| t["table"] == name)
                .ok_or_else(|| fail("unknown table"))?;
            let empty = serde_json::json!([]);
            let required_readers = strings(table.get("static_readers").unwrap_or(&empty))?;
            let required_writers = strings(table.get("static_writers").unwrap_or(&empty))?;
            if required_readers
                .iter()
                .any(|r| r != "database-tools" && !readers.contains(r))
                || required_writers
                    .iter()
                    .any(|r| r != "database-tools" && !writers.contains(r))
            {
                return Err(fail("unresolved source readers or generic writers"));
            }
            let functions = strings(table.get("function_writers").unwrap_or(&empty))?;
            if !functions.is_empty() {
                let reviewed = strings(row.get("function_evidence").unwrap_or(&empty))?;
                let only = row["function_only"] == true;
                if !functions.is_subset(&reviewed)
                    || (required_writers.is_empty() && !only)
                    || (only && direct_write)
                {
                    return Err(fail("unresolved function-mediated write policy"));
                }
            }
        }
    }
    let mut merged: BTreeMap<(String, String, String, String, String), BTreeSet<String>> =
        BTreeMap::new();
    for [kind, schema, name, args, role, privilege] in matrix {
        if kind == "schema" {
            continue;
        }
        merged
            .entry((
                kind,
                schema,
                name,
                args,
                role.trim_start_matches(crate::DEFAULT_ROLE_PREFIX).into(),
            ))
            .or_default()
            .insert(privilege);
    }
    let mut lines = vec![
        "-- Generated by scripts/inventory/generate_database_grants.py; DO NOT EDIT.".into(),
        format!(
            "-- data-access-inventory-sha256: {:x}",
            Sha256::digest(inventory_bytes)
        ),
        format!(
            "-- reviewed-grants-policy-sha256: {:x}",
            Sha256::digest(policy_bytes)
        ),
        "-- This file grants no schema CREATE and no privileges to PUBLIC.".into(),
        String::new(),
    ];
    for schema in SCHEMAS {
        for role in ROLES {
            lines.push(format!(
                "REVOKE ALL PRIVILEGES ON SCHEMA \"{schema}\" FROM \"{prefix}{role}\";"
            ));
        }
    }
    lines.push(String::new());
    for (kind, schema, name, args) in &managed {
        let (k, n) = object(kind, schema, name, args);
        for role in ROLES {
            lines.push(format!(
                "REVOKE ALL PRIVILEGES ON {k} {n} FROM \"{prefix}{role}\";"
            ));
        }
    }
    if !managed.is_empty() {
        lines.push(String::new());
    }
    let schema_roles: BTreeSet<_> = merged.keys().map(|(_, s, _, _, r)| (s, r)).collect();
    for (schema, role) in &schema_roles {
        lines.push(format!(
            "GRANT USAGE ON SCHEMA \"{schema}\" TO \"{prefix}{role}\";"
        ));
    }
    if !schema_roles.is_empty() {
        lines.push(String::new());
    }
    for ((kind, schema, name, args, role), privileges) in merged {
        let (k, n) = object(&kind, &schema, &name, &args);
        let privileges = PRIVILEGES
            .iter()
            .filter(|p| privileges.contains(**p))
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "GRANT {privileges} ON {k} {n} TO \"{prefix}{role}\";"
        ));
    }
    Ok(lines.join("\n") + "\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_grants_exact() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../database/baselines/2026_08_post_kb_v1");
        let actual = generate(
            &std::fs::read(root.join("data-access-inventory.json")).unwrap(),
            &std::fs::read(root.join("grants-policy.json")).unwrap(),
            crate::DEFAULT_ROLE_PREFIX,
        )
        .unwrap();
        assert_eq!(
            actual,
            std::fs::read_to_string(root.join("grants.sql")).unwrap()
        );
    }
}
