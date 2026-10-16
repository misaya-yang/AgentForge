use crate::{AuthorityError, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
pub const START_MARKER: &str = "-- BEGIN ARC03 GENERATED OBJECT RELOCATION";
pub const END_MARKER: &str = "-- END ARC03 GENERATED OBJECT RELOCATION";
const SCHEMAS: &[&str] = &["assistant", "gateway", "knowledge", "public"];
const APP_ROLES: &[&str] = &[
    "gateway",
    "runtime",
    "capability_worker",
    "knowledge_api",
    "knowledge_worker",
];
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectPolicy {
    pub kind: String,
    pub name: String,
    pub schema: String,
    pub identity_arguments: String,
}
pub type GrantMatrix = BTreeSet<[String; 6]>;
fn fail(msg: &str) -> AuthorityError {
    AuthorityError::Refused(msg.into())
}
fn identifier(value: &str) -> bool {
    regex::Regex::new(r"^[a-z_][a-z0-9_]{0,62}$")
        .unwrap()
        .is_match(value)
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| fail("policy field must be a string"))
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    value[key]
        .as_array()
        .ok_or_else(|| fail("policy field must be an array"))
}
fn json(bytes: &[u8]) -> Result<Value> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| fail("policy bytes are invalid JSON"))?;
    if !value.is_object() {
        return Err(fail("policy bytes must contain an object"));
    }
    Ok(value)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn inventory_keys(inventory: &Value) -> Result<BTreeSet<(String, String)>> {
    let pg = &inventory["postgresql"];
    if !pg.is_object() {
        return Err(fail("inventory.postgresql must be an object"));
    }
    let mut result = BTreeSet::new();
    let mut add = |kind: &str, name: &str| -> Result<()> {
        if !identifier(name) || !result.insert((kind.into(), name.into())) {
            return Err(fail("unsafe or duplicate inventory object"));
        }
        Ok(())
    };
    for (kind, field, key) in [
        ("table", "tables", Some("table")),
        ("view", "views", None),
        ("function", "functions", None),
        ("type", "types", None),
        ("sequence", "sequences_explicit", None),
        ("sequence", "sequences_implicit_serial", Some("name")),
    ] {
        let entries = pg
            .get(field)
            .map(|v| {
                v.as_array()
                    .ok_or_else(|| fail("inventory field must be a list"))
            })
            .transpose()?;
        for row in entries.into_iter().flatten() {
            let value = if let Some(key) = key { &row[key] } else { row };
            add(
                kind,
                value
                    .as_str()
                    .ok_or_else(|| fail("inventory name must be a string"))?,
            )?;
        }
    }
    Ok(result)
}
pub fn load_policy_bytes(inventory_bytes: &[u8], policy_bytes: &[u8]) -> Result<Vec<ObjectPolicy>> {
    let inventory = json(inventory_bytes)?;
    let policy = json(policy_bytes)?;
    if policy["schema"] != "arc03-object-ownership/v1"
        || policy["inventory_sha256"] != digest(inventory_bytes)
    {
        return Err(fail(
            "ownership policy schema or exact inventory binding mismatch",
        ));
    }
    let mut objects = BTreeMap::new();
    for row in array(&policy, "objects")? {
        let (kind, name, schema) = (
            string(row, "kind")?,
            string(row, "name")?,
            string(row, "schema")?,
        );
        if !["table", "view", "sequence", "function", "type"].contains(&kind)
            || !identifier(name)
            || !SCHEMAS.contains(&schema)
        {
            return Err(fail("unsafe ownership mapping"));
        }
        if row["inventory_name"] != name
            || row["owner"] != "owner"
            || array(row, "evidence")?.is_empty()
        {
            return Err(fail(
                "mapping must retain name, owner and ownership evidence",
            ));
        }
        let arguments = row
            .get("identity_arguments")
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| fail("function identity must be string"))
            })
            .transpose()?
            .unwrap_or("");
        if (kind == "function"
            && !regex::Regex::new(r"^[a-z0-9_ ,.\[\]()+]*$")
                .unwrap()
                .is_match(arguments))
            || (kind != "function" && !arguments.is_empty())
        {
            return Err(fail("unsafe function identity"));
        }
        if objects
            .insert(
                (kind.to_string(), name.to_string()),
                ObjectPolicy {
                    kind: kind.into(),
                    name: name.into(),
                    schema: schema.into(),
                    identity_arguments: arguments.into(),
                },
            )
            .is_some()
        {
            return Err(fail("duplicate ownership mapping"));
        }
    }
    if objects.keys().cloned().collect::<BTreeSet<_>>() != inventory_keys(&inventory)? {
        return Err(fail("ownership coverage mismatch"));
    }
    Ok(objects.into_values().collect())
}
pub fn load_grant_matrix(inventory_bytes: &[u8], policy_bytes: &[u8]) -> Result<GrantMatrix> {
    let policy = json(policy_bytes)?;
    if policy["schema_version"] != "arc03-grants-policy/v1"
        || policy["inventory_sha256"] != digest(inventory_bytes)
    {
        return Err(fail("grants policy schema or inventory binding mismatch"));
    }
    let mut matrix = BTreeSet::new();
    let mut schema_roles = BTreeSet::new();
    for item in array(&policy, "objects")? {
        let (kind, schema, name) = (
            string(item, "inventory_kind")?,
            string(item, "schema")?,
            string(item, "name")?,
        );
        let arguments = item
            .get("identity_arguments")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !["table", "view", "sequence", "function", "type"].contains(&kind)
            || !SCHEMAS.contains(&schema)
            || !identifier(name)
        {
            return Err(fail("unsafe grants mapping"));
        }
        for access in array(item, "access")? {
            for role in array(access, "roles")? {
                let role = role
                    .as_str()
                    .filter(|s| APP_ROLES.contains(s))
                    .ok_or_else(|| fail("unknown grants role"))?;
                schema_roles.insert((schema.to_string(), role.to_string()));
                for privilege in array(access, "privileges")? {
                    let privilege = privilege
                        .as_str()
                        .ok_or_else(|| fail("unsafe grants privilege"))?;
                    if !privilege.chars().any(char::is_uppercase)
                        || privilege.chars().any(char::is_lowercase)
                    {
                        return Err(fail("unsafe grants privilege"));
                    }
                    matrix.insert([
                        kind.into(),
                        schema.into(),
                        name.into(),
                        arguments.into(),
                        format!("ai_gateway_{role}"),
                        privilege.into(),
                    ]);
                }
            }
        }
    }
    for (schema, role) in schema_roles {
        matrix.insert([
            "schema".into(),
            schema.clone(),
            schema,
            "".into(),
            format!("ai_gateway_{role}"),
            "USAGE".into(),
        ]);
    }
    Ok(matrix)
}
pub fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn values<'a>(rows: impl Iterator<Item = [&'a str; 3]>) -> String {
    let rows = rows
        .map(|row| {
            format!(
                "({})",
                row.into_iter()
                    .map(sql_literal)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        "VALUES (NULL::text, NULL::text, NULL::text)".into()
    } else {
        format!("VALUES\n            {}", rows.join(",\n            "))
    }
}
pub fn render_relocation(objects: &[ObjectPolicy]) -> String {
    let relations = values(
        objects
            .iter()
            .filter(|i| ["table", "view", "sequence"].contains(&i.kind.as_str()))
            .map(|i| [i.kind.as_str(), i.name.as_str(), i.schema.as_str()]),
    );
    let functions = values(objects.iter().filter(|i| i.kind == "function").map(|i| {
        [
            i.name.as_str(),
            i.identity_arguments.as_str(),
            i.schema.as_str(),
        ]
    }));
    let types = values(
        objects
            .iter()
            .filter(|i| i.kind == "type")
            .map(|i| [i.name.as_str(), i.schema.as_str(), i.schema.as_str()]),
    );
    include_str!("templates/render_relocation.sql.template")
        .replace("{START_MARKER}", START_MARKER)
        .replace("{END_MARKER}", END_MARKER)
        .replace("{relation_values}", &relations)
        .replace("{function_values}", &functions)
        .replace("{type_values}", &types)
}
fn location_check(item: &ObjectPolicy) -> String {
    let name = sql_literal(&item.name);
    let schema = sql_literal(&item.schema);
    let expression = if ["table", "view", "sequence"].contains(&item.kind.as_str()) {
        let kinds = match item.kind.as_str() {
            "table" => "('r', 'p', 'f')",
            "view" => "('v', 'm')",
            _ => "('S')",
        };
        format!(
            "SELECT count(*) = 1 FROM pg_class AS class JOIN pg_namespace AS namespace ON namespace.oid = class.relnamespace WHERE namespace.nspname = {schema} AND class.relname = {name} AND class.relkind IN {kinds}"
        )
    } else if item.kind == "function" {
        format!(
            "SELECT count(*) = 1 FROM pg_proc AS procedure JOIN pg_namespace AS namespace ON namespace.oid = procedure.pronamespace WHERE namespace.nspname = {schema} AND procedure.proname = {name} AND oidvectortypes(procedure.proargtypes) = {}",
            sql_literal(&item.identity_arguments)
        )
    } else {
        format!(
            "SELECT count(*) = 1 FROM pg_type AS type_object JOIN pg_namespace AS namespace ON namespace.oid = type_object.typnamespace WHERE namespace.nspname = {schema} AND type_object.typname = {name}"
        )
    };
    format!(
        "({}, ({expression}))",
        sql_literal(&format!(
            "location:{}:{}.{}",
            item.kind, item.schema, item.name
        ))
    )
}
pub fn render_verify(objects: &[ObjectPolicy], matrix: &GrantMatrix) -> Result<String> {
    if matrix.is_empty() {
        return Err(fail("exact grant matrix cannot be empty"));
    }
    let expected = matrix
        .iter()
        .map(|row| {
            format!(
                "({})",
                row.iter()
                    .map(|s| sql_literal(s))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join(",\n                ");
    let roles = APP_ROLES
        .iter()
        .copied()
        .chain(["migrator"])
        .map(|r| sql_literal(&format!("ai_gateway_{r}")))
        .collect::<Vec<_>>()
        .join(", ");
    let schemas = SCHEMAS
        .iter()
        .map(|s| sql_literal(s))
        .collect::<Vec<_>>()
        .join(", ");
    let matrix = include_str!("templates/grant_matrix_check.sql.template")
        .replace("{expected}", &expected)
        .replace("{roles}", &roles)
        .replace("{schemas}", &schemas);
    let mut locations = objects
        .iter()
        .map(location_check)
        .collect::<Vec<_>>()
        .join(",\n    ");
    if !locations.is_empty() {
        locations.push_str(",\n    ");
    }
    Ok(include_str!("templates/verify.sql.template")
        .replace("{locations}", &locations)
        .replace("{grant_matrix}", &matrix))
}
pub fn replace_relocation(sql: &str, generated: &str) -> Result<String> {
    let start = sql
        .find(START_MARKER)
        .ok_or_else(|| fail("cutover must contain exactly one generated marker pair"))?;
    let end = sql
        .find(END_MARKER)
        .filter(|end| *end >= start)
        .ok_or_else(|| fail("cutover must contain exactly one generated marker pair"))?
        + END_MARKER.len();
    if sql[start + START_MARKER.len()..].contains(START_MARKER) {
        return Err(fail("duplicate relocation marker"));
    }
    Ok(format!("{}{generated}{}", &sql[..start], &sql[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_exact_frozen_sql_without_python() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../database/baselines/2026_08_post_kb_v1");
        let inventory = std::fs::read(root.join("data-access-inventory.json")).unwrap();
        let objects = load_policy_bytes(
            &inventory,
            &std::fs::read(root.join("ownership-policy.json")).unwrap(),
        )
        .unwrap();
        let matrix = load_grant_matrix(
            &inventory,
            &std::fs::read(root.join("grants-policy.json")).unwrap(),
        )
        .unwrap();
        let old = std::fs::read_to_string(root.join("cutover_convergence.sql")).unwrap();
        assert_eq!(
            replace_relocation(&old, &render_relocation(&objects)).unwrap(),
            old
        );
        assert_eq!(
            render_verify(&objects, &matrix).unwrap(),
            std::fs::read_to_string(root.join("verify.sql")).unwrap()
        );
    }
    #[test]
    fn rejects_empty_matrix_and_bad_markers() {
        assert!(render_verify(&[], &BTreeSet::new()).is_err());
        assert!(replace_relocation("SELECT 1;", "new").is_err());
        assert!(
            replace_relocation(&format!("{START_MARKER}{START_MARKER}{END_MARKER}"), "new")
                .is_err()
        );
    }
}
