//! Official MCP 2025-11-25 / 2026-07-28 wire behavior. No transport or fallback.
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Revision {
    Legacy2025,
    Modern2026,
}
pub const SUPPORTED: [&str; 2] = ["2026-07-28", "2025-11-25"];
impl Revision {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "2025-11-25" => Ok(Self::Legacy2025),
            "2026-07-28" => Ok(Self::Modern2026),
            _ => Err("MCP_PROTOCOL_VERSION_UNSUPPORTED"),
        }
    }
    /// Repository record or explicit operator setting; absent setting preserves v2025.
    pub fn configured(record: &Value) -> Result<Self, &'static str> {
        if let Some(value) = record
            .get("protocol_version")
            .or_else(|| record.get("protocol"))
        {
            return Self::parse(value.as_str().ok_or("MCP_PROTOCOL_VERSION_UNSUPPORTED")?);
        }
        Self::parse(
            &std::env::var("AI_PLATFORM_MCP_PROTOCOL_REVISION")
                .unwrap_or_else(|_| "2025-11-25".into()),
        )
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Legacy2025 => "2025-11-25",
            Self::Modern2026 => "2026-07-28",
        }
    }
    pub fn modern(self) -> bool {
        self == Self::Modern2026
    }
    pub fn decorate(self, payload: &mut Value) -> Result<HeaderMap, &'static str> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "mcp-protocol-version",
            HeaderValue::from_static(self.as_str()),
        );
        if !self.modern() {
            return Ok(headers);
        }
        let method = payload["method"]
            .as_str()
            .filter(|m| {
                !m.is_empty() && m.len() <= 128 && m.bytes().all(|b| (0x21..=0x7e).contains(&b))
            })
            .ok_or("MCP_REQUEST_INVALID")?
            .to_owned();
        if !payload.get("params").is_some() {
            payload["params"] = json!({});
        }
        let params = payload["params"]
            .as_object_mut()
            .ok_or("MCP_REQUEST_INVALID")?;
        if !params.contains_key("_meta") {
            params.insert("_meta".into(), json!({}));
        }
        let meta = params
            .get_mut("_meta")
            .and_then(Value::as_object_mut)
            .ok_or("MCP_REQUEST_INVALID")?;
        for (key, value) in [
            (
                "io.modelcontextprotocol/protocolVersion",
                json!(self.as_str()),
            ),
            (
                "io.modelcontextprotocol/clientInfo",
                json!({"name":"ai-gateway","version":"1.0.0"}),
            ),
            ("io.modelcontextprotocol/clientCapabilities", json!({})),
        ] {
            if meta.get(key).is_some_and(|old| old != &value) {
                return Err("MCP_REQUEST_METADATA_MISMATCH");
            }
            meta.insert(key.into(), value);
        }
        headers.insert(
            "mcp-method",
            HeaderValue::from_str(&method).map_err(|_| "MCP_REQUEST_INVALID")?,
        );
        if ["tools/call", "resources/read", "prompts/get"].contains(&method.as_str()) {
            let key = if method == "resources/read" {
                "uri"
            } else {
                "name"
            };
            let name = payload["params"][key]
                .as_str()
                .ok_or("MCP_REQUEST_INVALID")?;
            headers.insert(
                "mcp-name",
                HeaderValue::from_str(&encode_header(name)?).map_err(|_| "MCP_REQUEST_INVALID")?,
            );
        }
        Ok(headers)
    }
    pub fn validate_discovery(self, result: &Value) -> Result<(), &'static str> {
        if !self.modern() {
            return Err("MCP_PROTOCOL_VERSION_MISMATCH");
        }
        let versions = result["supportedVersions"]
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 16)
            .ok_or("MCP_DISCOVERY_INVALID")?;
        if !versions.iter().all(|v| v.as_str().is_some()) {
            return Err("MCP_DISCOVERY_INVALID");
        }
        if !versions.iter().any(|v| v == self.as_str()) {
            return Err("MCP_PROTOCOL_DOWNGRADE_REJECTED");
        }
        if result["resultType"] != "complete" || !result["capabilities"].is_object() {
            return Err("MCP_DISCOVERY_INVALID");
        }
        Ok(())
    }
    pub fn validate_result(self, result: &Value) -> Result<(), &'static str> {
        if self.modern() && result["resultType"] != "complete" {
            return Err("MCP_RESULT_PATTERN_UNSUPPORTED");
        }
        Ok(())
    }
    pub fn check_session(self, headers: &HeaderMap) -> Result<(), &'static str> {
        if self.modern() && headers.contains_key("mcp-session-id") {
            return Err("MCP_MODERN_SESSION_REJECTED");
        }
        Ok(())
    }
}
pub fn encode_header(value: &str) -> Result<String, &'static str> {
    if value.len() > 8192 {
        return Err("MCP_HEADER_VALUE_TOO_LARGE");
    }
    if value.is_empty()
        || !value.is_ascii()
        || value.bytes().any(|b| !(0x20..=0x7e).contains(&b))
        || value.starts_with(' ')
        || value.ends_with(' ')
        || (value.starts_with("=?base64?") && value.ends_with("?="))
    {
        Ok(format!("=?base64?{}?=", STANDARD.encode(value.as_bytes())))
    } else {
        Ok(value.into())
    }
}
/// Never retry tools/call after a version error. A recognized modern error is
/// explicit evidence of modern semantics, not permission to initialize legacy.
pub fn classify_error(response: &Value) -> Option<&'static str> {
    match response["error"]["code"].as_i64() {
        Some(-32022) => Some("MCP_PROTOCOL_DOWNGRADE_REJECTED"),
        Some(-32020) => Some("MCP_HEADER_BODY_MISMATCH"),
        Some(-32021) => Some("MCP_CLIENT_CAPABILITY_REQUIRED"),
        _ => None,
    }
}
#[cfg(test)]
#[path = "revision_tests.rs"]
mod tests;

pub fn tool_headers(schema: &Value, arguments: &Value) -> Result<HeaderMap, &'static str> {
    if !schema.is_object() {
        return Err("MCP_TOOL_HEADER_SCHEMA_INVALID");
    }
    fn walk(
        schema: &Value,
        args: Option<&Value>,
        reachable: bool,
        names: &mut std::collections::BTreeSet<String>,
        out: &mut HeaderMap,
        depth: usize,
    ) -> Result<(), &'static str> {
        if depth > 32 {
            return Err("MCP_TOOL_HEADER_SCHEMA_INVALID");
        }
        match schema {
            Value::Object(object) => {
                if let Some(annotation) = object.get("x-mcp-header") {
                    let name = annotation
                        .as_str()
                        .filter(|n| {
                            !n.is_empty()
                                && n.len() <= 128
                                && n.bytes().all(|b| {
                                    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
                                })
                        })
                        .ok_or("MCP_TOOL_HEADER_SCHEMA_INVALID")?;
                    let kind = object
                        .get("type")
                        .and_then(Value::as_str)
                        .ok_or("MCP_TOOL_HEADER_SCHEMA_INVALID")?;
                    if !reachable
                        || !names.insert(name.to_ascii_lowercase())
                        || !["integer", "string", "boolean"].contains(&kind)
                    {
                        return Err("MCP_TOOL_HEADER_SCHEMA_INVALID");
                    }
                    if let Some(value) = args {
                        let value = match kind {
                            "string" => value.as_str().map(str::to_owned),
                            "boolean" => value.as_bool().map(|v| v.to_string()),
                            "integer" => value
                                .as_i64()
                                .filter(|n| {
                                    (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(n)
                                })
                                .map(|v| v.to_string()),
                            _ => None,
                        }
                        .ok_or("MCP_TOOL_HEADER_ARGUMENT_INVALID")?;
                        out.insert(
                            reqwest::header::HeaderName::from_bytes(
                                format!("Mcp-Param-{name}").as_bytes(),
                            )
                            .map_err(|_| "MCP_TOOL_HEADER_SCHEMA_INVALID")?,
                            HeaderValue::from_str(&encode_header(&value)?)
                                .map_err(|_| "MCP_TOOL_HEADER_ARGUMENT_INVALID")?,
                        );
                    }
                }
                for (key, value) in object {
                    if key == "properties" {
                        let properties =
                            value.as_object().ok_or("MCP_TOOL_HEADER_SCHEMA_INVALID")?;
                        for (name, child) in properties {
                            walk(
                                child,
                                args.and_then(|a| a.get(name)),
                                reachable || depth == 0,
                                names,
                                out,
                                depth + 1,
                            )?;
                        }
                    } else if key != "x-mcp-header" {
                        walk(value, None, false, names, out, depth + 1)?;
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    walk(value, None, false, names, out, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut out = HeaderMap::new();
    walk(
        schema,
        Some(arguments),
        false,
        &mut Default::default(),
        &mut out,
        0,
    )?;
    if out.len() > 64 || out.values().map(|v| v.as_bytes().len()).sum::<usize>() > 32768 {
        return Err("MCP_HEADER_VALUE_TOO_LARGE");
    }
    Ok(out)
}
