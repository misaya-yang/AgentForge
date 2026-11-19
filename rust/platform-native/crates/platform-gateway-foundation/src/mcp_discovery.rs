//! Immutable MCP discovery snapshots. The caller owns one supervised PostgreSQL
//! transaction covering both row locks, every snapshot/diff, and current pointers.
//! Constructors elsewhere are inert; this module performs no work on import.
use crate::mcp_registry_store::{McpContext, McpError};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, BTreeSet};

const ANNOTATIONS: &[&str] = &[
    "$comment",
    "default",
    "deprecated",
    "description",
    "examples",
    "readOnly",
    "title",
    "writeOnly",
];
fn object(value: &Value) -> Map<String, Value> {
    if let Some(v) = value.as_object() {
        return v.clone();
    }
    value
        .as_str()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
    }
}
fn py_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        _ => v.to_string(),
    }
}
/// Python json.dumps-compatible finite JSON float formatting. JSON excludes NaN
/// and infinities; arbitrary-size Python integers exceed serde_json's domain.
fn python_number(n: &serde_json::Number) -> String {
    let s = n.to_string();
    if n.is_i64() || n.is_u64() {
        return s;
    }
    let negative = s.starts_with('-');
    let body = s.trim_start_matches('-');
    let (mantissa, exponent) = body
        .split_once(['e', 'E'])
        .map(|(a, b)| (a, b.parse::<i32>().unwrap_or(0)))
        .unwrap_or((body, 0));
    let decimal = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let all = mantissa.replace('.', "");
    let leading = all.bytes().take_while(|b| *b == b'0').count();
    let digits = all[leading..].trim_end_matches('0');
    if digits.is_empty() {
        return if negative { "-0.0" } else { "0.0" }.into();
    }
    let power = decimal - leading as i32 - 1 + exponent;
    let sign = if negative { "-" } else { "" };
    if !(-4..16).contains(&power) {
        let tail = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        format!(
            "{sign}{}{tail}e{}{:02}",
            &digits[..1],
            if power < 0 { "-" } else { "+" },
            power.abs()
        )
    } else if power < 0 {
        format!("{sign}0.{}{}", "0".repeat((-power - 1) as usize), digits)
    } else {
        let point = (power + 1) as usize;
        if point >= digits.len() {
            format!("{sign}{}{}.0", digits, "0".repeat(point - digits.len()))
        } else {
            format!("{sign}{}.{}", &digits[..point], &digits[point..])
        }
    }
}
pub fn canonical_schema(value: &Value) -> String {
    match value {
        Value::Object(o) => {
            let keys: BTreeSet<_> = o.keys().collect();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|k| format!(
                        "{}:{}",
                        serde_json::to_string(k).expect("key"),
                        canonical_schema(&o[k])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical_schema).collect::<Vec<_>>().join(",")
        ),
        Value::Number(n) => python_number(n),
        _ => serde_json::to_string(value).expect("JSON value"),
    }
}
pub fn hash_tool_schema(schema: &Value) -> String {
    format!("{:x}", Sha256::digest(canonical_schema(schema).as_bytes()))
}
pub fn hash_tool_contract(schema: &Value, risk_level: &str, read_only: bool) -> String {
    hash_tool_schema(&json!({"input_schema":schema,"risk_level":risk_level,"read_only":read_only}))
}
pub fn mcp_runtime_name(server_id: &str, tool_id: &str) -> Result<String, McpError> {
    let server = uuid::Uuid::parse_str(server_id)
        .map_err(|_| McpError::Validation("MCP_SERVER_ID_INVALID".into()))?;
    let tool = uuid::Uuid::parse_str(tool_id)
        .map_err(|_| McpError::Validation("MCP_TOOL_ID_INVALID".into()))?;
    Ok(format!("mcp_{}__{}", server.simple(), tool.simple()))
}
fn view(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(k, _)| !ANNOTATIONS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), view(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(view).collect()),
        _ => v.clone(),
    }
}
fn numeric_equal(x: &serde_json::Number, y: &serde_json::Number) -> bool {
    let integral = |n: &serde_json::Number| n.is_i64() || n.is_u64();
    if integral(x) && integral(y) {
        return x == y;
    }
    if !integral(x) && !integral(y) {
        return x.as_f64() == y.as_f64();
    }
    let (integer, float) = if integral(x) {
        (x, y.as_f64())
    } else {
        (y, x.as_f64())
    };
    let Some(float) = float else { return false };
    if float.fract() != 0.0 {
        return false;
    }
    if let Some(value) = integer.as_i64() {
        (-9223372036854775808.0..9223372036854775808.0).contains(&float) && float as i64 == value
    } else if let Some(value) = integer.as_u64() {
        (0.0..18446744073709551616.0).contains(&float) && float as u64 == value
    } else {
        false
    }
}
fn py_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => numeric_equal(x, y),
        (Value::Bool(x), Value::Number(y)) | (Value::Number(y), Value::Bool(x)) => {
            y.as_f64() == Some(if *x { 1.0 } else { 0.0 })
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| py_equal(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, a)| y.get(k).is_some_and(|b| py_equal(a, b)))
        }
        _ => a == b,
    }
}
fn required(v: &Value) -> Option<BTreeSet<String>> {
    v.as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect()
}
fn compatible(previous: &Value, current: &Value) -> bool {
    if py_equal(&view(previous), &view(current)) {
        return true;
    }
    let (Some(before), Some(after)) = (previous.as_object(), current.as_object()) else {
        return false;
    };
    if !py_equal(&previous["type"], &current["type"]) {
        return false;
    }
    let empty = Map::new();
    let bp = match before.get("properties") {
        None => &empty,
        Some(Value::Object(o)) => o,
        _ => return false,
    };
    let ap = match after.get("properties") {
        None => &empty,
        Some(Value::Object(o)) => o,
        _ => return false,
    };
    if bp.keys().collect::<BTreeSet<_>>() != ap.keys().collect::<BTreeSet<_>>() {
        return false;
    }
    let empty_req = json!([]);
    let (Some(br), Some(ar)) = (
        required(before.get("required").unwrap_or(&empty_req)),
        required(after.get("required").unwrap_or(&empty_req)),
    ) else {
        return false;
    };
    if !ar.is_subset(&br) {
        return false;
    }
    if bp.iter().any(|(k, v)| !compatible(v, &ap[k])) {
        return false;
    }
    let remaining = |o: &Map<String, Value>| {
        Value::Object(
            o.iter()
                .filter(|(k, _)| {
                    !ANNOTATIONS.contains(&k.as_str())
                        && !["properties", "required", "items"].contains(&k.as_str())
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    };
    if !py_equal(&view(&remaining(before)), &view(&remaining(after))) {
        return false;
    }
    match (before.get("items"), after.get("items")) {
        (None, None) => true,
        (Some(b), Some(a)) => compatible(b, a),
        _ => false,
    }
}
fn properties(value: &Value) -> Map<String, Value> {
    value["properties"].as_object().cloned().unwrap_or_default()
}
fn required_set(value: &Value) -> Result<Vec<Value>, ()> {
    let values = if !truthy(value) {
        vec![]
    } else {
        match value {
            Value::Array(v) => v.clone(),
            Value::String(s) => s.chars().map(|c| json!(c.to_string())).collect(),
            Value::Object(m) => m.keys().map(|k| json!(k)).collect(),
            _ => return Err(()),
        }
    };
    let mut set = vec![];
    for value in values {
        if value.is_array() || value.is_object() {
            return Err(());
        }
        if !set.iter().any(|v| py_equal(v, &value)) {
            set.push(value)
        }
    }
    Ok(set)
}
fn sorted_difference(a: &[Value], b: &[Value]) -> Result<Vec<Value>, ()> {
    let mut values: Vec<Value> = a
        .iter()
        .filter(|a| !b.iter().any(|b| py_equal(a, b)))
        .cloned()
        .collect();
    if values.len() < 2 {
        return Ok(values);
    }
    let numeric = |v: &Value| v.is_number() || v.is_boolean();
    if values.iter().all(Value::is_string) {
        values.sort_by(|a, b| a.as_str().cmp(&b.as_str()))
    } else if values.iter().all(numeric) {
        let number = |v: &Value| {
            if let Value::Bool(v) = v {
                json!(if *v { 1 } else { 0 })
            } else {
                v.clone()
            }
        };
        values.sort_by(|a, b| {
            let a = number(a);
            let b = number(b);
            let a = a.as_number().unwrap();
            let b = b.as_number().unwrap();
            let integral = |n: &serde_json::Number| {
                n.as_i64()
                    .map(i128::from)
                    .or_else(|| n.as_u64().map(i128::from))
            };
            if let (Some(a), Some(b)) = (integral(a), integral(b)) {
                return a.cmp(&b);
            }
            let order = a.as_f64().unwrap().total_cmp(&b.as_f64().unwrap());
            if order == std::cmp::Ordering::Equal && !numeric_equal(a, b) {
                if let Some(a) = integral(a) {
                    return a.cmp(&(b.as_f64().unwrap() as i128));
                }
                if let Some(b) = integral(b) {
                    return (a.as_f64().unwrap() as i128).cmp(&b);
                }
            }
            order
        });
    } else {
        return Err(());
    }
    Ok(values)
}
fn checked_schema_diff(previous: Option<&Value>, current: &Value) -> Result<Value, ()> {
    if let Some(previous) = previous {
        let bp = properties(previous);
        let cp = properties(current);
        if bp
            .iter()
            .any(|(k, v)| cp.get(k).is_some_and(|c| !v.is_object() || !c.is_object()))
        {
            return Err(());
        }
        let br = required_set(&previous["required"])?;
        let ar = required_set(&current["required"])?;
        sorted_difference(&ar, &br)?;
        sorted_difference(&br, &ar)?;
    }
    Ok(schema_diff(previous, current))
}
pub fn schema_diff(previous: Option<&Value>, current: &Value) -> Value {
    let cp = properties(current);
    let current_names: BTreeSet<_> = cp.keys().cloned().collect();
    let Some(previous) = previous else {
        return json!({"breaking":false,"added_properties":current_names,"removed_properties":[],"required_added":[],"required_removed":[],"type_changes":{}});
    };
    let bp = properties(previous);
    let before_names: BTreeSet<_> = bp.keys().cloned().collect();
    let br = required_set(&previous["required"]).unwrap_or_default();
    let ar = required_set(&current["required"]).unwrap_or_default();
    let changes: Map<String, Value> = before_names
        .intersection(&current_names)
        .filter(|k| !py_equal(&bp[*k]["type"], &cp[*k]["type"]))
        .map(|k| (k.clone(), json!({"from":bp[k]["type"],"to":cp[k]["type"]})))
        .collect();
    json!({"breaking":!compatible(previous,current),"added_properties":current_names.difference(&before_names).collect::<Vec<_>>(),"removed_properties":before_names.difference(&current_names).collect::<Vec<_>>(),"required_added":sorted_difference(&ar,&br).unwrap_or_default(),"required_removed":sorted_difference(&br,&ar).unwrap_or_default(),"type_changes":changes})
}
fn input_schema(raw: &Value) -> Value {
    for key in ["inputSchema", "input_schema"] {
        if truthy(&raw[key]) {
            return raw[key].clone();
        }
    }
    json!({})
}
pub fn validate_tools(arguments: &Value) -> Result<(), McpError> {
    let tools = arguments
        .get("tools")
        .unwrap_or(arguments)
        .as_array()
        .ok_or_else(|| McpError::Validation("MCP_TOOL_SCHEMA_INVALID".into()))?;
    let mut names = BTreeSet::new();
    for tool in tools {
        let name = if truthy(&tool["name"]) {
            py_string(&tool["name"])
        } else {
            String::new()
        };
        if name.is_empty() || name.chars().count() > 255 {
            return Err(McpError::Validation("MCP_TOOL_NAME_INVALID".into()));
        }
        if !names.insert(name) {
            return Err(McpError::Validation("MCP_TOOL_NAME_DUPLICATE".into()));
        }
    }
    Ok(())
}
fn domain(kind: &str, code: &str) -> Value {
    json!({"__mcp_error":{"kind":kind,"code":code}})
}
/// Caller must validate_tools before opening its supervised transaction and
/// decode the domain sentinel. A local savepoint allows a domain failure after
/// earlier writes to roll those writes back before the supervisor sees Ok; SQL
/// failures still propagate and roll back the entire owned transaction. This
/// retains canonical statement/error ordering instead of prevalidating later
/// tools before an earlier SQL statement that can fail.
pub async fn record_discovery_native(
    connection: &mut PgConnection,
    context: &McpContext,
    arguments: Value,
) -> Result<Value, sqlx::Error> {
    sqlx::query("SAVEPOINT mcp_discovery_work")
        .execute(&mut *connection)
        .await?;
    let result = record_discovery_inner(connection, context, arguments).await?;
    if result.get("__mcp_error").is_some() {
        sqlx::query("ROLLBACK TO SAVEPOINT mcp_discovery_work")
            .execute(&mut *connection)
            .await?;
    }
    sqlx::query("RELEASE SAVEPOINT mcp_discovery_work")
        .execute(&mut *connection)
        .await?;
    Ok(result)
}
async fn record_discovery_inner(
    connection: &mut PgConnection,
    context: &McpContext,
    arguments: Value,
) -> Result<Value, sqlx::Error> {
    let tenant = &context.tenant_id;
    let server = arguments["server_id"].as_str().unwrap_or("");
    let tools = arguments["tools"].as_array().cloned().unwrap_or_default();
    let present=sqlx::query("SELECT server_id FROM mcp_servers WHERE tenant_id=$1 AND server_id=$2::uuid AND enabled=TRUE AND deleted_at IS NULL FOR UPDATE").bind(tenant).bind(server).fetch_optional(&mut *connection).await?;
    if present.is_none() {
        return Ok(domain("not_found", "MCP_SERVER_NOT_FOUND"));
    }
    let rows=sqlx::query("SELECT to_jsonb(q) AS item FROM (SELECT t.tool_id,t.upstream_name,t.runtime_name,t.current_snapshot_id,s.schema_version,s.schema_hash,s.contract_hash,s.input_schema,s.read_only,s.risk_level FROM mcp_tools t LEFT JOIN mcp_tool_snapshots s ON s.tenant_id=t.tenant_id AND s.snapshot_id=t.current_snapshot_id WHERE t.tenant_id=$1 AND t.server_id=$2::uuid FOR UPDATE OF t) q").bind(tenant).bind(server).fetch_all(&mut *connection).await?;
    let mut existing = BTreeMap::new();
    for row in rows {
        let item: Value = row.try_get("item")?;
        existing.insert(
            item["upstream_name"].as_str().unwrap_or("").to_owned(),
            item,
        );
    }
    let mut changed = vec![];
    let mut unchanged = vec![];
    let mut seen = BTreeSet::new();
    for raw in tools {
        let name = py_string(&raw["name"]);
        seen.insert(name.clone());
        let description = if truthy(&raw["description"]) {
            py_string(&raw["description"])
        } else {
            String::new()
        }
        .chars()
        .take(2000)
        .collect::<String>();
        let schema = input_schema(&raw);
        if !schema.is_object() {
            return Ok(domain("validation", "MCP_TOOL_SCHEMA_INVALID"));
        }
        let schema_hash = hash_tool_schema(&schema);
        let contract_hash = hash_tool_contract(&schema, "medium", false);
        let old = existing.get(&name);
        let tool_id = old
            .and_then(|v| v["tool_id"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let runtime_name = old
            .and_then(|v| v["runtime_name"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "mcp_{}__{}",
                    uuid::Uuid::parse_str(server)
                        .map(|id| id.simple().to_string())
                        .unwrap_or_default(),
                    uuid::Uuid::parse_str(&tool_id)
                        .map(|id| id.simple().to_string())
                        .unwrap_or_default()
                )
            });
        if let Some(old) = old
            && old["contract_hash"].as_str() == Some(contract_hash.as_str())
        {
            sqlx::query("UPDATE mcp_tools SET enabled=TRUE,updated_at=NOW() WHERE tenant_id=$1 AND tool_id=$2::uuid").bind(tenant).bind(&tool_id).execute(&mut *connection).await?;
            unchanged.push(json!({"tool_id":tool_id,"runtime_name":runtime_name,"schema_hash":schema_hash,"schema_version":old["schema_version"]}));
            continue;
        }
        let previous = old.map(|o| Value::Object(object(&o["input_schema"])));
        let version = old.and_then(|o| o["schema_version"].as_i64()).unwrap_or(0) + 1;
        let mut diff = match checked_schema_diff(previous.as_ref(), &schema) {
            Ok(diff) => diff,
            Err(()) => return Ok(domain("unhandled", "MCP_INTERNAL_ERROR")),
        };
        let old_risk = old.and_then(|o| o["risk_level"].as_str()).unwrap_or("");
        let old_read = old.map(|o| truthy(&o["read_only"])).unwrap_or(false);
        diff["risk_change"] = if !old_risk.is_empty() && old_risk != "medium" {
            json!({"from":old_risk,"to":"medium"})
        } else {
            Value::Null
        };
        diff["read_only_change"] = if old.is_some() && old_read {
            json!({"from":true,"to":false})
        } else {
            Value::Null
        };
        if !diff["risk_change"].is_null() || old_read {
            diff["breaking"] = json!(true)
        }
        let breaking = diff["breaking"].as_bool().unwrap_or(true);
        let snapshot = uuid::Uuid::new_v4().to_string();
        if old.is_none() {
            sqlx::query("INSERT INTO mcp_tools (tenant_id,tool_id,server_id,upstream_name,runtime_name) VALUES ($1,$2::uuid,$3::uuid,$4,$5)").bind(tenant).bind(&tool_id).bind(server).bind(&name).bind(&runtime_name).execute(&mut *connection).await?;
        }
        sqlx::query("INSERT INTO mcp_tool_snapshots (tenant_id,snapshot_id,server_id,tool_id,schema_version,schema_hash,description,input_schema,contract_hash,risk_level,read_only) VALUES ($1,$2::uuid,$3::uuid,$4::uuid,$5,$6,$7,$8::jsonb,$9,'medium',FALSE)").bind(tenant).bind(&snapshot).bind(server).bind(&tool_id).bind(version as i32).bind(&schema_hash).bind(&description).bind(sqlx::types::Json(&schema)).bind(&contract_hash).execute(&mut *connection).await?;
        let previous_snapshot = old.and_then(|o| o["current_snapshot_id"].as_str());
        sqlx::query("INSERT INTO mcp_schema_diffs (tenant_id,server_id,tool_id,from_snapshot_id,to_snapshot_id,breaking,diff) VALUES ($1,$2::uuid,$3::uuid,$4::uuid,$5::uuid,$6,$7::jsonb)").bind(tenant).bind(server).bind(&tool_id).bind(previous_snapshot).bind(&snapshot).bind(breaking).bind(sqlx::types::Json(&diff)).execute(&mut *connection).await?;
        sqlx::query("UPDATE mcp_tools SET current_snapshot_id=$3::uuid,enabled=TRUE,updated_at=NOW() WHERE tenant_id=$1 AND tool_id=$2::uuid").bind(tenant).bind(&tool_id).bind(&snapshot).execute(&mut *connection).await?;
        changed.push(json!({"tool_id":tool_id,"runtime_name":runtime_name,"schema_hash":schema_hash,"schema_version":version,"breaking":breaking,"diff":diff}));
    }
    let removed: Vec<String> = existing
        .keys()
        .filter(|k| !seen.contains(*k))
        .cloned()
        .collect();
    if !removed.is_empty() {
        sqlx::query("UPDATE mcp_tools SET enabled=FALSE,updated_at=NOW() WHERE tenant_id=$1 AND server_id=$2::uuid AND upstream_name=ANY($3::text[])").bind(tenant).bind(server).bind(&removed).execute(&mut *connection).await?;
    }
    sqlx::query("UPDATE mcp_servers SET health_status='healthy',circuit_state='closed',consecutive_failures=0,circuit_open_until=NULL,last_health_at=NOW(),last_error_code=NULL,updated_at=NOW() WHERE tenant_id=$1 AND server_id=$2::uuid").bind(tenant).bind(server).execute(&mut *connection).await?;
    let breaking = !removed.is_empty() || changed.iter().any(|v| v["breaking"] == true);
    Ok(
        json!({"server_id":server,"changed":changed,"unchanged":unchanged,"removed":removed,"breaking":breaking}),
    )
}

