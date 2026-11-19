//! Deterministic, offline OpenAPI 3.2 SDK production. Transports are caller-owned:
//! this module cannot change credentials, CSRF, authority, retries, or SSE parsing.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};
const METHODS: [&str; 9] = [
    "get", "post", "put", "patch", "delete", "head", "options", "trace", "query",
];
fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization")
}
fn rust_quote(value: &str) -> String {
    format!("{value:?}")
}
fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn identifier(value: &str) -> String {
    let mut out = String::from("Api");
    let mut upper = true;
    for c in value.chars() {
        if !c.is_ascii_alphanumeric() {
            upper = true;
            continue;
        }
        out.push(if upper { c.to_ascii_uppercase() } else { c });
        upper = false;
    }
    if out == "Api" {
        out.push_str("Value");
    }
    out
}
fn field_name(value: &str) -> String {
    // Prefix avoids every current/future language keyword, including `self`.
    let mut out = String::from("field_");
    for c in value.chars() {
        out.push(if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '_'
        });
    }
    out
}
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .map(|(key, value)| format!("{}:{}", quote(key), canonical_json(value)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => value.to_string(),
    }
}
fn pointer_part(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}
fn fragment(value: &str) -> Result<String, String> {
    let raw = value
        .strip_prefix('#')
        .ok_or_else(|| format!("unbundled external reference {value}"))?;
    let mut bytes = Vec::new();
    let mut chars = raw.as_bytes().iter().copied();
    while let Some(c) = chars.next() {
        if c == b'%' {
            let a = chars.next().ok_or("truncated reference escape")?;
            let b = chars.next().ok_or("truncated reference escape")?;
            let pair = std::str::from_utf8(&[a, b])
                .map_err(|_| "invalid reference escape")?
                .to_owned();
            bytes.push(u8::from_str_radix(&pair, 16).map_err(|_| "invalid reference escape")?);
        } else {
            bytes.push(c)
        }
    }
    let decoded = String::from_utf8(bytes).map_err(|_| "reference is not UTF-8")?;
    if !decoded.is_empty() && !decoded.starts_with('/') {
        return Err(format!(
            "unsupported reference anchor {value}; use a JSON pointer"
        ));
    }
    // serde_json accepts malformed ~ escapes; require RFC 6901 spelling.
    for part in decoded.split('/') {
        let mut chars = part.chars();
        while let Some(c) = chars.next() {
            if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
                return Err("invalid JSON pointer escape".into());
            }
        }
    }
    Ok(decoded)
}
fn resolve<'a>(document: &'a Value, value: &Value) -> Result<&'a Value, String> {
    let reference = value
        .get("$ref")
        .and_then(Value::as_str)
        .ok_or("missing reference")?;
    document
        .pointer(&fragment(reference)?)
        .ok_or_else(|| format!("unresolved reference {reference}"))
}
fn resolved(document: &Value, value: &Value) -> Result<Value, String> {
    let mut current = value.clone();
    let mut seen = BTreeSet::new();
    while let Some(reference) = current.get("$ref").and_then(Value::as_str) {
        if !seen.insert(reference.to_owned()) {
            return Err("cyclic non-schema reference".into());
        }
        if current.as_object().is_some_and(|fields| {
            fields.keys().any(|key| {
                !matches!(key.as_str(), "$ref" | "summary" | "description")
                    && !key.starts_with("x-")
            })
        }) {
            return Err("non-schema reference siblings require producer resolution".into());
        }
        current = resolve(document, &current)?.clone();
    }
    Ok(current)
}
#[derive(Default)]
struct Names {
    by_key: BTreeMap<String, String>,
    by_name: BTreeMap<String, String>,
}
impl Names {
    fn get(&mut self, key: &str, hint: &str) -> String {
        if let Some(id) = self.by_key.get(key) {
            return id.clone();
        }
        let base = identifier(hint);
        let mut id = base.clone();
        if self.by_name.get(&id).is_some_and(|old| old != key) {
            id = format!("{base}_{}", &hash(key)[..12]);
        }
        // A digest collision must not silently overwrite another definition.
        let mut suffix = 0;
        while self.by_name.get(&id).is_some_and(|old| old != key) {
            suffix += 1;
            id = format!("{base}_{}_{suffix}", hash(key));
        }
        self.by_key.insert(key.into(), id.clone());
        self.by_name.insert(id.clone(), key.into());
        id
    }
}
struct Types<'a> {
    document: &'a Value,
    names: Names,
    refs: BTreeMap<String, String>,
    pending: BTreeMap<String, Value>,
    python: BTreeMap<String, String>,
    rust: BTreeMap<String, String>,
    typescript: BTreeMap<String, String>,
}
impl<'a> Types<'a> {
    fn new(document: &'a Value) -> Self {
        let mut names = Names::default();
        names
            .by_name
            .insert("ApiNever".into(), "reserved:never".into());
        Self {
            document,
            names,
            refs: BTreeMap::new(),
            pending: BTreeMap::new(),
            python: BTreeMap::new(),
            rust: BTreeMap::new(),
            typescript: BTreeMap::new(),
        }
    }
    fn reference(&mut self, schema: &Value) -> Result<Option<String>, String> {
        let Some(raw) = schema.get("$ref").and_then(Value::as_str) else {
            return Ok(None);
        };
        let pointer = fragment(raw)?;
        if let Some(id) = self.refs.get(&pointer) {
            return Ok(Some(id.clone()));
        }
        let target = self
            .document
            .pointer(&pointer)
            .ok_or_else(|| format!("unresolved schema reference {raw}"))?
            .clone();
        let id = self.names.get(
            &format!("ref:{pointer}"),
            pointer.rsplit('/').next().unwrap_or("Root"),
        );
        self.refs.insert(pointer, id.clone());
        self.pending.insert(id.clone(), target);
        Ok(Some(id))
    }
    fn alias(&mut self, schema: &Value, key: &str, hint: &str, lang: u8) -> Result<String, String> {
        let id = self.names.get(key, hint);
        let ty = self.ty(schema, key, hint, lang, 0)?;
        match lang {
            0 => {
                self.typescript
                    .insert(id.clone(), format!("export type {id} = {ty};\n"));
            }
            1 => {
                if ty != quote(&id) {
                    self.python
                        .insert(id.clone(), format!("{id}: TypeAlias = {ty}\n"));
                }
            }
            _ => {
                if ty != id {
                    self.rust.insert(id.clone(),format!("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n#[serde(transparent)]\npub struct {id}(pub {ty});\n"));
                }
            }
        }
        Ok(id)
    }
    fn ty(
        &mut self,
        schema: &Value,
        key: &str,
        hint: &str,
        lang: u8,
        depth: usize,
    ) -> Result<String, String> {
        if depth > 64 {
            return Err("schema nesting exceeds 64".into());
        }
        let any = match lang {
            0 => "unknown",
            1 => "Any",
            _ => "serde_json::Value",
        };
        if schema == &Value::Bool(true) {
            return Ok(any.into());
        }
        if schema == &Value::Bool(false) {
            return Ok(match lang {
                0 => "never",
                1 => "Never",
                _ => "ApiNever",
            }
            .into());
        }
        if !schema.is_object() {
            return Err(format!("schema must be object or boolean: {key}"));
        }
        if schema.get("$dynamicRef").is_some() {
            return Err("dynamic schema references need producer resolution".into());
        }
        if schema["x-codegen-void"] == true {
            return Ok(match lang {
                0 => "void",
                1 => "None",
                _ => "()",
            }
            .into());
        }
        if schema["nullable"] == true {
            let mut nonnull = schema.clone();
            nonnull.as_object_mut().unwrap().remove("nullable");
            return self.ty(
                &json!({"anyOf":[nonnull,{"type":"null"}]}),
                key,
                hint,
                lang,
                depth + 1,
            );
        }
        if let Some(reference) = self.reference(schema)? {
            let siblings = schema
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, _)| {
                    matches!(
                        key.as_str(),
                        "type"
                            | "properties"
                            | "required"
                            | "additionalProperties"
                            | "oneOf"
                            | "anyOf"
                            | "allOf"
                            | "enum"
                            | "const"
                            | "items"
                    )
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<serde_json::Map<_, _>>();
            if !siblings.is_empty() {
                let only = json!({"$ref":schema["$ref"]});
                return self.ty(
                    &json!({"allOf":[only,Value::Object(siblings)]}),
                    key,
                    hint,
                    lang,
                    depth + 1,
                );
            }
            return Ok(match lang {
                1 => quote(&reference),
                2 => format!("Box<{reference}>"),
                _ => reference,
            });
        }
        if let Some(parts) = schema.get("allOf").and_then(Value::as_array) {
            if parts.is_empty() {
                return Err("empty allOf".into());
            }
            let mut parts = parts.clone();
            let mut siblings = schema.as_object().unwrap().clone();
            siblings.remove("allOf");
            if !siblings.is_empty() {
                parts.push(Value::Object(siblings));
            }
            let merged = merge_objects(self.document, &parts, &mut BTreeSet::new(), depth + 1)?;
            return self.ty(&merged, key, hint, lang, depth + 1);
        }
        for keyword in ["oneOf", "anyOf"] {
            if let Some(parts) = schema.get(keyword).and_then(Value::as_array) {
                if parts.is_empty() {
                    return Err(format!("empty {keyword}"));
                }
                let mut alternatives = Vec::new();
                for (i, part) in parts.iter().enumerate() {
                    alternatives.push(self.ty(
                        part,
                        &format!("{key}/{keyword}/{i}"),
                        &format!("{hint}Part{i}"),
                        lang,
                        depth + 1,
                    )?)
                }
                return Ok(match lang {
                    0 => format!("({})", alternatives.join(" | ")),
                    1 => format!("Union[{}]", alternatives.join(", ")),
                    _ => {
                        let id = self.names.get(key, hint);
                        let cases = alternatives
                            .iter()
                            .enumerate()
                            .map(|(i, t)| format!("    Part{i}({t}),"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        self.rust.insert(id.clone(),format!("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n#[serde(untagged)]\npub enum {id} {{\n{cases}\n}}\n"));
                        id
                    }
                });
            }
        }
        if let Some(kinds) = schema.get("type").and_then(Value::as_array) {
            if kinds.is_empty() {
                return Err("empty type array".into());
            }
            let parts = kinds
                .iter()
                .map(|kind| {
                    let mut part = schema.clone();
                    part["type"] = kind.clone();
                    part
                })
                .collect::<Vec<_>>();
            return self.ty(&json!({"anyOf":parts}), key, hint, lang, depth + 1);
        }
        if let Some(values) = schema
            .get("enum")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| schema.get("const").map(|v| vec![v.clone()]))
        {
            if values.is_empty() {
                return Err("empty enum".into());
            }
            if lang == 0 {
                return Ok(values
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join(" | "));
            }
            if lang == 1
                && values.iter().all(|v| {
                    v.is_string() || v.is_boolean() || v.is_i64() || v.is_u64() || v.is_null()
                })
            {
                return Ok(format!(
                    "Literal[{}]",
                    values
                        .iter()
                        .map(|v| match v {
                            Value::Bool(true) => "True".into(),
                            Value::Bool(false) => "False".into(),
                            Value::Null => "None".into(),
                            _ => v.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if lang == 2 && values.iter().all(Value::is_string) {
                let id = self.names.get(key, hint);
                let cases = values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        format!(
                            "    #[serde(rename = {})]\n    Value{i},",
                            rust_quote(v.as_str().unwrap())
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.rust.insert(id.clone(),format!("#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]\npub enum {id} {{\n{cases}\n}}\n"));
                return Ok(id);
            }
        }
        let kind = schema
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                if schema.get("properties").is_some()
                    || schema.get("additionalProperties").is_some()
                {
                    "object"
                } else {
                    "any"
                }
            });
        if kind == "string" && schema.get("format").and_then(Value::as_str) == Some("binary") {
            return Ok(match lang {
                0 => "Blob",
                1 => "bytes",
                _ => "Vec<u8>",
            }
            .into());
        }
        let scalar = match (kind, lang) {
            ("null", 0) => Some("null"),
            ("null", 1) => Some("None"),
            ("null", _) => Some("()"),
            ("boolean", 0) => Some("boolean"),
            ("boolean", _) => Some("bool"),
            ("integer" | "number", 0) => Some("number"),
            ("integer", 1) => Some("int"),
            ("number", 1) => Some("float"),
            ("integer", _) => Some("i64"),
            ("number", _) => Some("f64"),
            ("string", 0) => Some("string"),
            ("string", 1) => Some("str"),
            ("string", _) => Some("String"),
            _ => None,
        };
        if let Some(ty) = scalar {
            return Ok(ty.into());
        }
        if kind == "array" {
            let ty = self.ty(
                schema.get("items").unwrap_or(&Value::Bool(true)),
                &format!("{key}/items"),
                &format!("{hint}Item"),
                lang,
                depth + 1,
            )?;
            return Ok(match lang {
                0 => format!("Array<{ty}>"),
                1 => format!("list[{ty}]"),
                _ => format!("Vec<{ty}>"),
            });
        }
        if kind != "object" {
            return Ok(any.into());
        }
        let id = self.names.get(key, hint);
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>();
        let mut fields = Vec::new();
        let mut used = BTreeSet::new();
        let mut properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for key in &required {
            properties
                .entry((*key).to_owned())
                .or_insert(Value::Bool(true));
        }
        {
            for (key_name, value) in properties.iter().collect::<BTreeMap<_, _>>() {
                let ty = self.ty(
                    value,
                    &format!("{key}/properties/{}", pointer_part(key_name)),
                    &format!("{hint}{}", identifier(key_name)),
                    lang,
                    depth + 1,
                )?;
                let needed = required.contains(key_name.as_str());
                fields.push(match lang {
                    0 => format!("{}{}: {ty}", quote(key_name), if needed { "" } else { "?" }),
                    1 => format!(
                        "{}: {}[{ty}]",
                        quote(key_name),
                        if needed { "Required" } else { "NotRequired" }
                    ),
                    _ => {
                        let mut field = field_name(key_name);
                        if !used.insert(field.clone()) {
                            field.push_str(&format!("_{}", &hash(key_name)[..12]));
                            if !used.insert(field.clone()) {
                                return Err("Rust field-name collision".into());
                            }
                        }
                        let annotation = if needed {
                            String::new()
                        } else {
                            ", default, skip_serializing_if = \"Option::is_none\"".into()
                        };
                        format!(
                            "    #[serde(rename = {}{annotation})]\n    pub {field}: {},",
                            rust_quote(key_name),
                            if needed { ty } else { format!("Option<{ty}>") }
                        )
                    }
                })
            }
        }
        let extra = schema
            .get("additionalProperties")
            .unwrap_or(&Value::Bool(true));
        let closed = extra == &Value::Bool(false);
        let additional = if closed {
            None
        } else {
            Some(self.ty(
                extra,
                &format!("{key}/additionalProperties"),
                &format!("{hint}Additional"),
                lang,
                depth + 1,
            )?)
        };
        Ok(match lang {
            0 => {
                let object = format!("{{ {} }}", fields.join("; "));
                match additional {
                    Some(ty) => format!(
                        "({object} & Record<string, {}>)",
                        if fields.is_empty() {
                            ty
                        } else {
                            "unknown".into()
                        }
                    ),
                    None => object,
                }
            }
            1 => {
                if fields.is_empty() && !closed {
                    return Ok(format!("dict[str, {}]", additional.unwrap_or("Any".into())));
                }
                self.python.insert(
                    id.clone(),
                    format!(
                        "{id} = TypedDict({}, {{{}}}, total=False)\n",
                        quote(&id),
                        fields.join(", ")
                    ),
                );
                quote(&id)
            }
            _ => {
                if let Some(ty) = additional {
                    fields.push(format!("    #[serde(flatten, default)]\n    pub additional: std::collections::BTreeMap<String, {ty}>,"))
                }
                self.rust.insert(id.clone(),format!("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n{}pub struct {id} {{\n{}\n}}\n",if closed{"#[serde(deny_unknown_fields)]\n"}else{""},fields.join("\n")));
                id
            }
        })
    }
    fn finish_refs(&mut self) -> Result<(), String> {
        while let Some((id, schema)) = self.pending.pop_first() {
            let mut target = &schema;
            let mut seen = BTreeSet::new();
            while let Some(reference) = target.get("$ref").and_then(Value::as_str) {
                if !seen.insert(reference.to_owned()) {
                    return Err("cyclic schema aliases need a structural boundary".into());
                }
                target = resolve(self.document, target)?;
            }
            for lang in 0..=2 {
                let key = self.names.by_name[&id].clone();
                let ty = self.ty(&schema, &key, &id, lang, 0)?;
                match lang {
                    0 => {
                        self.typescript
                            .insert(id.clone(), format!("export type {id} = {ty};\n"));
                    }
                    1 => {
                        if ty != quote(&id) {
                            self.python
                                .insert(id.clone(), format!("{id}: TypeAlias = {ty}\n"));
                        }
                    }
                    _ => {
                        if ty != id {
                            self.rust.insert(id.clone(),format!("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n#[serde(transparent)]\npub struct {id}(pub {ty});\n"));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
fn merge_objects(
    document: &Value,
    parts: &[Value],
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> Result<Value, String> {
    if depth > 64 {
        return Err("allOf nesting exceeds 64".into());
    }
    let mut properties = serde_json::Map::new();
    let mut required = BTreeSet::new();
    let mut closed = false;
    let mut additional = Value::Bool(true);
    for part in parts {
        let mut p = part.clone();
        if let Some(r) = p.get("$ref").and_then(Value::as_str) {
            let r = r.to_owned();
            if !seen.insert(r.clone()) {
                return Err("recursive allOf cannot be flattened".into());
            }
            let target = resolve(document, &p)?.clone();
            let mut siblings = p.as_object().unwrap().clone();
            siblings.remove("$ref");
            p = merge_objects(
                document,
                &[target, Value::Object(siblings)],
                seen,
                depth + 1,
            )?;
            seen.remove(&r);
        }
        if let Some(nested) = p.get("allOf").and_then(Value::as_array) {
            let mut parts = nested.clone();
            let mut siblings = p.as_object().unwrap().clone();
            siblings.remove("allOf");
            parts.push(Value::Object(siblings));
            p = merge_objects(document, &parts, seen, depth + 1)?;
        }
        if p == Value::Bool(true) {
            continue;
        }
        if p.get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t != "object")
            || !p.is_object()
        {
            return Err("allOf requires compatible object schemas".into());
        }
        if let Some(props) = p.get("properties").and_then(Value::as_object) {
            for (k, v) in props {
                if let Some(old) = properties.insert(k.clone(), v.clone()) {
                    if old != *v {
                        return Err(format!("conflicting allOf field {k}"));
                    }
                }
            }
        }
        for key in p
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            required.insert(key.to_owned());
        }
        if let Some(extra) = p.get("additionalProperties") {
            if extra == &Value::Bool(false) {
                closed = true
            } else if extra.is_object() {
                if additional != Value::Bool(true) && additional != *extra {
                    return Err("conflicting allOf additionalProperties".into());
                }
                additional = extra.clone();
            }
        }
    }
    Ok(
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":if closed{Value::Bool(false)}else{additional}}),
    )
}
#[derive(Clone)]
struct Operation {
    key: String,
    id: String,
    request: String,
    response: String,
    event: String,
    stream: bool,
    unary: bool,
    binary: bool,
    request_required: bool,
    parameters: String,
    body: String,
    media: Vec<String>,
    method: String,
    path: String,
    python_method: String,
}
fn combine(types: Vec<Value>) -> Value {
    if types.len() == 1 {
        types.into_iter().next().unwrap()
    } else {
        json!({"anyOf":types})
    }
}
/// Generate all schema and inline operation models from one validated producer document.
pub fn generate(document: &Value) -> Result<BTreeMap<String, String>, String> {
    crate::validation::validate(document)?;
    let mut types = Types::new(document);
    if let Some(schemas) = document
        .pointer("/components/schemas")
        .and_then(Value::as_object)
    {
        for (raw, schema) in schemas.iter().collect::<BTreeMap<_, _>>() {
            let pointer = format!("/components/schemas/{}", pointer_part(raw));
            let key = format!("ref:{pointer}");
            let id = types.names.get(&key, raw);
            types.refs.insert(pointer, id.clone());
            types.pending.insert(id, schema.clone());
        }
    }
    let mut operations = Vec::new();
    let paths = document["paths"]
        .as_object()
        .ok_or("OpenAPI paths missing")?;
    for (path, item) in paths.iter().collect::<BTreeMap<_, _>>() {
        let item = resolved(document, item)?;
        let mut entries = METHODS
            .into_iter()
            .filter_map(|method| {
                item.get(method)
                    .map(|operation| (method.to_owned(), operation.clone()))
            })
            .collect::<Vec<_>>();
        if let Some(additional) = item.get("additionalOperations").and_then(Value::as_object) {
            for (method, operation) in additional.iter().collect::<BTreeMap<_, _>>() {
                if method.is_empty()
                    || !method
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                    || METHODS
                        .iter()
                        .any(|known| known.eq_ignore_ascii_case(method))
                {
                    return Err("invalid/duplicate additional HTTP method".into());
                }
                entries.push((method.clone(), operation.clone()));
            }
        }
        for (method, operation) in entries {
            let op = resolved(document, &operation)?;
            let key = format!("{} {path}", method.to_uppercase());
            let hint = format!("{}{}", method, identifier(path));
            let op_key = format!("operation:{key}");
            let id = types.names.get(&op_key, &hint);
            let mut parameters = BTreeMap::<String, BTreeMap<String, Value>>::new();
            for parameter in item
                .get("parameters")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .chain(
                    op.get("parameters")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten(),
                )
            {
                let parameter = resolved(document, parameter)?;
                let location = parameter["in"]
                    .as_str()
                    .ok_or("parameter location missing")?;
                let name = parameter["name"].as_str().ok_or("parameter name missing")?;
                if !["path", "query", "querystring", "header", "cookie"].contains(&location) {
                    return Err(format!("unsupported parameter location {location}"));
                }
                parameters
                    .entry(location.into())
                    .or_default()
                    .insert(name.into(), parameter);
            }
            let mut parameter_properties = BTreeMap::new();
            let mut parameter_required = Vec::new();
            for (location, values) in parameters {
                let mut properties = BTreeMap::new();
                let mut required = Vec::new();
                for (name, parameter) in values {
                    let schema = if let Some(s) = parameter.get("schema") {
                        s.clone()
                    } else {
                        let content = parameter
                            .get("content")
                            .and_then(Value::as_object)
                            .ok_or("parameter needs schema/content")?;
                        let schemas = content
                            .values()
                            .map(|media| media.get("schema").cloned().unwrap_or(Value::Bool(true)))
                            .collect::<Vec<_>>();
                        combine(schemas)
                    };
                    properties.insert(name.clone(), schema);
                    if location == "path" || parameter["required"] == true {
                        required.push(name)
                    }
                }
                if !required.is_empty() {
                    parameter_required.push(location.clone())
                }
                parameter_properties.insert(location,json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}));
            }
            let parameter_schema = json!({"type":"object","properties":parameter_properties,"required":parameter_required,"additionalProperties":false});
            let body = op
                .get("requestBody")
                .map(|v| resolved(document, v))
                .transpose()?;
            let request_required = body.as_ref().is_some_and(|b| b["required"] == true);
            let mut media = Vec::new();
            let mut body_schemas = Vec::new();
            let mut body_variants = Vec::new();
            if let Some(content) = body
                .as_ref()
                .and_then(|v| v.get("content"))
                .and_then(Value::as_object)
            {
                for (mime, description) in content.iter().collect::<BTreeMap<_, _>>() {
                    media.push(mime.clone());
                    let schema = description
                        .get("schema")
                        .cloned()
                        .unwrap_or(Value::Bool(true));
                    for lang in 0..=2 {
                        types.alias(
                            &schema,
                            &format!("{op_key}/request/{mime}"),
                            &format!("{hint}Request{}", identifier(mime)),
                            lang,
                        )?;
                    }
                    body_variants.push((mime.clone(), schema.clone()));
                    body_schemas.push(schema);
                }
            }
            let body_schema = if body_schemas.is_empty() {
                Value::Bool(false)
            } else {
                combine(body_schemas)
            };
            let mut success = Vec::new();
            let mut events = Vec::new();
            let mut binary = false;
            for (status, response) in op["responses"]
                .as_object()
                .ok_or("operation responses missing")?
                .iter()
                .collect::<BTreeMap<_, _>>()
            {
                let response = resolved(document, response)?;
                let successful = status.starts_with('2');
                if let Some(content) = response.get("content").and_then(Value::as_object) {
                    for (mime, description) in content.iter().collect::<BTreeMap<_, _>>() {
                        let event = description.get("itemSchema").or_else(|| {
                            description.get("schema").and_then(|s| s.get("itemSchema"))
                        });
                        let streaming = event.is_some()
                            || [
                                "text/event-stream",
                                "application/x-ndjson",
                                "application/jsonl",
                                "application/json-seq",
                            ]
                            .contains(&mime.as_str());
                        let schema = event
                            .or_else(|| description.get("schema"))
                            .cloned()
                            .unwrap_or(Value::Bool(true));
                        for lang in 0..=2 {
                            types.alias(
                                &schema,
                                &format!("{op_key}/response/{status}/{mime}"),
                                &format!("{hint}Response{status}{}", identifier(mime)),
                                lang,
                            )?;
                        }
                        if successful {
                            binary |= !streaming
                                && resolved(document, &schema)?
                                    .get("format")
                                    .and_then(Value::as_str)
                                    == Some("binary");
                            if streaming {
                                events.push(schema)
                            } else {
                                success.push(schema)
                            }
                        }
                    }
                } else if successful {
                    success.push(json!({"x-codegen-void":true}));
                }
            }
            let stream = !events.is_empty();
            let unary = !success.is_empty() || !stream;
            let response_schema = if success.is_empty() {
                if stream {
                    Value::Bool(false)
                } else {
                    Value::Bool(true)
                }
            } else {
                combine(success)
            };
            let event_schema = if stream {
                combine(events)
            } else {
                Value::Bool(false)
            };
            let mut request_properties =
                parameter_schema["properties"].as_object().unwrap().clone();
            let mut request_keys = parameter_schema["required"].as_array().unwrap().clone();
            if body_schema != Value::Bool(false) {
                request_properties.insert("body".into(), body_schema.clone());
                if request_required {
                    request_keys.push(json!("body"))
                }
                if media.len() > 1 {
                    request_properties
                        .insert("content_type".into(), json!({"type":"string","enum":media}));
                    request_keys.push(json!("content_type"));
                }
            }
            let request_schema = if body_variants.len() > 1 {
                let mut alternatives = Vec::new();
                for (mime, schema) in &body_variants {
                    let mut properties =
                        parameter_schema["properties"].as_object().unwrap().clone();
                    properties.insert("body".into(), schema.clone());
                    properties.insert("content_type".into(), json!({"const":mime}));
                    let mut required = parameter_schema["required"].as_array().unwrap().clone();
                    required.push(json!("body"));
                    required.push(json!("content_type"));
                    alternatives.push(json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}));
                }
                if !request_required {
                    alternatives.push(parameter_schema.clone());
                }
                combine(alternatives)
            } else {
                json!({"type":"object","properties":request_properties,"required":request_keys,"additionalProperties":false})
            };
            for lang in 0..=2 {
                types.alias(
                    &request_schema,
                    &format!("{op_key}/request"),
                    &format!("{hint}Request"),
                    lang,
                )?;
                types.alias(
                    &response_schema,
                    &format!("{op_key}/response"),
                    &format!("{hint}Response"),
                    lang,
                )?;
                types.alias(
                    &event_schema,
                    &format!("{op_key}/event"),
                    &format!("{hint}Event"),
                    lang,
                )?;
            }
            let request = types
                .names
                .get(&format!("{op_key}/request"), &format!("{hint}Request"));
            let response = types
                .names
                .get(&format!("{op_key}/response"), &format!("{hint}Response"));
            let event = types
                .names
                .get(&format!("{op_key}/event"), &format!("{hint}Event"));
            let parameters = types.ty(
                &parameter_schema,
                &format!("{op_key}/parameters"),
                &format!("{hint}Parameters"),
                0,
                0,
            )?;
            let body = types.ty(
                &body_schema,
                &format!("{op_key}/body"),
                &format!("{hint}Body"),
                0,
                0,
            )?;
            let source_method = op
                .get("operationId")
                .and_then(Value::as_str)
                .unwrap_or(&hint);
            let slug = field_name(source_method);
            let slug = slug.trim_start_matches("field_");
            let python_method = format!(
                "operation_{}_{}",
                slug.chars().take(80).collect::<String>(),
                &hash(&key)[..12]
            );
            operations.push(Operation {
                key,
                id,
                request,
                response,
                event,
                stream,
                unary,
                binary,
                request_required,
                parameters,
                body,
                media,
                method: method.to_uppercase(),
                path: path.clone(),
                python_method,
            });
        }
    }
    types.finish_refs()?;
    let source = canonical_json(document);
    let sha = hash(&source);
    let header = format!(
        "Generated by ai-platform-openapi codegen; do not edit.\n// Source SHA-256: {sha}\n"
    );
    let mut ts = format!(
        "// {header}export namespace Models {{\n{}\n}}\n",
        types
            .typescript
            .values()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    for id in types.typescript.keys() {
        ts.push_str(&format!("export type {id} = Models.{id};\n"));
    }
    ts.push_str("export interface ApiOperations {\n");
    for op in &operations {
        ts.push_str(&format!("  {}: {{ request: {}; response: {}; event: {}; stream: {}; parameters: {}; body: {}; bodyRequired: {} }};\n",quote(&op.key),op.request,op.response,op.event,op.stream,op.parameters,op.body,op.request_required));
    }
    ts.push_str("}\n");
    let metadata=operations.iter().map(|op|(op.key.clone(),json!({"method":op.method,"path":op.path,"body_media_types":op.media,"stream":op.stream,"operation_type":op.id}))).collect::<BTreeMap<_,_>>();
    ts.push_str(&format!(
        "export const operationMetadata = {} as const;\n",
        serde_json::to_string_pretty(&metadata).unwrap()
    ));
    // Compact runtime subsets allow the console to validate dynamic download
    // URLs and encode multipart bodies without bundling every operation model.
    let binary = operations
        .iter()
        .filter(|op| op.binary)
        .map(|op| &op.key)
        .collect::<Vec<_>>();
    let non_json = operations
        .iter()
        .filter(|op| op.media.iter().any(|m| m != "application/json"))
        .map(|op| (&op.key, &op.media))
        .collect::<BTreeMap<_, _>>();
    ts.push_str(&format!("export const binaryOperations = {} as const;\nexport type BinaryOperation = typeof binaryOperations[number];\nexport const nonJsonRequestMediaTypes = {} as const;\n",serde_json::to_string_pretty(&binary).unwrap(),serde_json::to_string_pretty(&non_json).unwrap()));
    ts.push_str(&format!(
        "export const operationKeys = {} as const;\n",
        serde_json::to_string_pretty(&operations.iter().map(|op| &op.key).collect::<Vec<_>>())
            .unwrap()
    ));
    ts.push_str(TS_TRANSPORT);
    ts.push_str(include_str!("fetch_client.ts"));
    ts.push_str(r#"
export function createFetchClient(fetcher: (url: string, init: RequestInit) => Promise<Response>) {
  const wire = createFetchTransport<ApiOperations>(fetcher, nonJsonRequestMediaTypes);
  return { send: wire.send, request<K extends UnaryOperation>(operation: K, input: ApiRequest<K>, signal?: AbortSignal) { return wire.request(operation, input, { signal }); } };
}
export function queryKey<K extends Extract<UnaryOperation, `GET ${string}`>>(scope: string, operation: K, input: ApiRequest<K>) {
  return ["generated-api", scope, operation, input] as const;
}
export function queryOptions<K extends Extract<UnaryOperation, `GET ${string}`>>(client: Pick<TypedTransport, "request">, scope: string, operation: K, input: ApiRequest<K>) {
  return { queryKey: queryKey(scope, operation, input), queryFn: ({ signal }: { signal: AbortSignal }) => client.request(operation, input, signal), retry: false as const, gcTime: 0 };
}
"#);
    let mut py = format!(
        "# Generated by ai-platform-openapi codegen; do not edit.\n# Source SHA-256: {sha}\nfrom __future__ import annotations\nfrom collections.abc import AsyncIterator, Mapping\nfrom typing import Any, Literal, Never, NotRequired, Protocol, Required, TypedDict, TypeAlias, Union, cast\n\n"
    );
    py.push_str(
        &types
            .python
            .values()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
    );
    py.push_str("\n\nclass TypedTransport(Protocol):\n    async def request(self, operation: str, request: Mapping[str, Any]) -> Any: ...\n    def stream(self, operation: str, request: Mapping[str, Any]) -> AsyncIterator[Any]: ...\n\n\nclass Client:\n    \"\"\"Caller transport retains authentication, CSRF, base URL and stream policy.\"\"\"\n    def __init__(self, transport: TypedTransport) -> None:\n        self._transport = transport\n");
    for op in &operations {
        let method = op.python_method.clone();
        if op.unary {
            py.push_str(&format!("\n    async def {method}(self, request: {}) -> {}:\n        return cast({}, await self._transport.request({}, cast(Mapping[str, Any], request)))\n",op.request,op.response,op.response,quote(&op.key)));
        }
        if op.stream {
            let method = if op.unary {
                format!("{method}_stream")
            } else {
                method
            };
            py.push_str(&format!("\n    def {method}(self, request: {}) -> AsyncIterator[{}]:\n        return cast(AsyncIterator[{}], self._transport.stream({}, cast(Mapping[str, Any], request)))\n",op.request,op.event,op.event,quote(&op.key)));
        }
    }
    if operations.is_empty() {
        py.push_str("\n    pass\n")
    }
    let rust = format!(
        "// {header}\n// False schemas have a serializable uninhabited representation.\n#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\npub enum ApiNever {{}}\n\n{}",
        types.rust.values().cloned().collect::<Vec<_>>().join("\n")
    );
    let manifest = json!({"schema":"agentforge/generated-sdk/v2","openapi":"3.2.0","source_sha256":sha,"operations":operations.len(),"schema_models":types.refs.len(),"stream_operations":operations.iter().filter(|op|op.stream).map(|op|&op.key).collect::<Vec<_>>(),"operation_methods":operations.iter().map(|op|(&op.key,json!({"request":if op.unary{Some(op.python_method.clone())}else{None},"stream":if op.stream{Some(if op.unary{format!("{}_stream",op.python_method)}else{op.python_method.clone()})}else{None}}))).collect::<BTreeMap<_,_>>(),"generator":"ai-platform-openapi codegen","transports":"caller-owned authentication, CSRF, authority, cancellation, retries and stream parsing; no embedded credentials or URL","limits":["types preserve schema structure, not runtime range/pattern validation","Python TypedDict describes declared keys; unknown keys remain transport data","allOf must resolve to compatible object schemas; conflicts fail closed"]});
    Ok(BTreeMap::from([
        ("typescript/generated.ts".into(), ts),
        ("python/generated_models.py".into(), py),
        ("rust/generated_models.rs".into(), rust),
        (
            "generated-manifest.json".into(),
            format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
        ),
    ]))
}
const TS_TRANSPORT: &str = r#"
export type Operation = keyof ApiOperations;
export type ApiRequest<K extends Operation> = ApiOperations[K]['request'];
export type ApiResponse<K extends Operation> = ApiOperations[K]['response'];
export type ApiEvent<K extends Operation> = ApiOperations[K]['event'];
export type StreamOperation = { [K in Operation]: ApiOperations[K]['stream'] extends true ? K : never }[Operation];
export type UnaryOperation = { [K in Operation]: ApiOperations[K]['response'] extends never ? never : K }[Operation];
/** Inject the existing console/native transport. Never embeds fetch, a base URL,
 * credentials, CSRF, retries, redirects or an alternate SSE parser. */
export interface TypedTransport {
  request<K extends UnaryOperation>(operation: K, request: ApiRequest<K>, signal?: AbortSignal): Promise<ApiResponse<K>>;
  stream<K extends StreamOperation>(operation: K, request: ApiRequest<K>, signal?: AbortSignal): AsyncIterable<ApiEvent<K>>;
}
export function createClient(transport: TypedTransport): TypedTransport {
  return {
    request: (operation, request, signal) => transport.request(operation, request, signal),
    stream: (operation, request, signal) => transport.stream(operation, request, signal),
  };
}
"#;
/// Bundle relative JSON references only from the candidate's own directory tree.
/// No URL retrieval, root escape, symlink escape, or hidden network I/O.
pub fn read_candidate(path: &Path) -> Result<Value, String> {
    let entry = fs::canonicalize(path).map_err(|e| format!("candidate: {e}"))?;
    let root = entry
        .parent()
        .ok_or("candidate needs parent")?
        .to_path_buf();
    let mut loader = Loader {
        root,
        ids: BTreeMap::new(),
        resources: BTreeMap::new(),
        bytes: 0,
    };
    loader.ids.insert(entry.clone(), String::new());
    let mut document = loader.read(&entry)?;
    if document.get("x-codegen-resources").is_some() {
        return Err("reserved x-codegen-resources extension already present".into());
    }
    loader.rewrite(&mut document, &entry, 0)?;
    if !loader.resources.is_empty() {
        document["x-codegen-resources"] =
            serde_json::to_value(loader.resources).map_err(|e| e.to_string())?;
    }
    Ok(document)
}
struct Loader {
    root: PathBuf,
    ids: BTreeMap<PathBuf, String>,
    resources: BTreeMap<String, Value>,
    bytes: usize,
}
impl Loader {
    fn read(&mut self, path: &Path) -> Result<Value, String> {
        use std::io::Read;
        let limit = 32 * 1024 * 1024usize;
        let remaining = limit
            .checked_sub(self.bytes)
            .ok_or("candidate size overflow")?;
        let file = fs::File::open(path).map_err(|e| format!("read local reference: {e}"))?;
        let mut bytes = Vec::new();
        file.take((remaining + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or("candidate size overflow")?;
        if self.bytes > limit {
            return Err("candidate references exceed 32 MiB".into());
        }
        serde_json::from_slice(&bytes).map_err(|e| format!("candidate JSON: {e}"))
    }
    fn rewrite(&mut self, value: &mut Value, source: &Path, depth: usize) -> Result<(), String> {
        if depth > 128 {
            return Err("reference document nesting exceeds 128".into());
        }
        match value {
            Value::Object(map) => {
                if map
                    .get("$id")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                {
                    return Err("offline codegen requires lexical JSON pointers/files; $id resource rebasing is unsupported".into());
                }
                if let Some(raw) = map.get("$ref").and_then(Value::as_str).map(str::to_owned) {
                    let (file, suffix) = raw
                        .split_once('#')
                        .map_or((raw.as_str(), ""), |(file, fragment)| (file, fragment));
                    let pointer = fragment(&format!("#{suffix}"))?;
                    let target = if file.is_empty() {
                        source.to_path_buf()
                    } else {
                        let relative = Path::new(file);
                        if file.contains([':', '\\', '%'])
                            || relative.is_absolute()
                            || relative.components().any(|c| {
                                !matches!(
                                    c,
                                    Component::Normal(_) | Component::CurDir | Component::ParentDir
                                )
                            })
                        {
                            return Err(format!("unsafe relative reference {raw}"));
                        }
                        let target = fs::canonicalize(
                            source
                                .parent()
                                .ok_or("source directory missing")?
                                .join(relative),
                        )
                        .map_err(|e| format!("relative reference {raw}: {e}"))?;
                        if !target.starts_with(&self.root)
                            || target.extension().and_then(|s| s.to_str()) != Some("json")
                        {
                            return Err(format!(
                                "reference escapes candidate root or is not JSON: {raw}"
                            ));
                        }
                        target
                    };
                    let prefix = if let Some(id) = self.ids.get(&target) {
                        id.clone()
                    } else {
                        if self.ids.len() >= 128 {
                            return Err("more than 128 reference resources".into());
                        }
                        let relative = target
                            .strip_prefix(&self.root)
                            .map_err(|_| "reference root mismatch")?
                            .to_str()
                            .ok_or("reference filename is not UTF-8")?;
                        let id = hash(relative);
                        self.ids.insert(target.clone(), id.clone());
                        let mut resource = self.read(&target)?;
                        self.rewrite(&mut resource, &target, depth + 1)?;
                        self.resources.insert(id.clone(), resource);
                        id
                    };
                    map.insert(
                        "$ref".into(),
                        json!(if prefix.is_empty() {
                            format!("#{pointer}")
                        } else {
                            format!("#/x-codegen-resources/{prefix}{pointer}")
                        }),
                    );
                }
                for (key, child) in map {
                    if key != "$ref"
                        && !matches!(
                            key.as_str(),
                            "example" | "default" | "const" | "enum" | "value"
                        )
                    {
                        self.rewrite(child, source, depth + 1)?
                    }
                }
            }
            Value::Array(values) => {
                for child in values {
                    self.rewrite(child, source, depth + 1)?
                }
            }
            _ => {}
        }
        Ok(())
    }
}
fn arguments(args: &[String]) -> Result<(PathBuf, Option<PathBuf>, bool), String> {
    let mut out = None;
    let mut candidate = None;
    let mut check = false;
    let mut seen = BTreeSet::new();
    let mut index = 0;
    while index < args.len() {
        let option = args[index].as_str();
        if !seen.insert(option) {
            return Err(format!("duplicate option {option}"));
        }
        match option {
            "--out" | "--candidate" => {
                let value = args
                    .get(index + 1)
                    .filter(|v| !v.starts_with("--"))
                    .ok_or_else(|| format!("missing {option} value"))?;
                if option == "--out" {
                    out = Some(PathBuf::from(value))
                } else {
                    candidate = Some(PathBuf::from(value))
                }
                index += 2
            }
            "--check" => {
                check = true;
                index += 1
            }
            _ => return Err(format!("unknown option {option}")),
        }
    }
    Ok((out.ok_or("usage: ai-platform-openapi codegen --out SDK_DIRECTORY [--candidate OPENAPI_JSON] [--check]")?,candidate,check))
}
pub fn write_or_check(
    root: &Path,
    files: &BTreeMap<String, String>,
    check: bool,
) -> Result<(), String> {
    if check {
        let stale = files
            .iter()
            .filter(|(name, body)| {
                fs::read(root.join(name)).ok().as_deref() != Some(body.as_bytes())
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        if stale.is_empty() {
            return Ok(());
        }
        return Err(format!(
            "generated SDK artifacts are missing or stale: {}",
            stale.join(", ")
        ));
    }
    for (name, body) in files {
        let target = root.join(name);
        let parent = target.parent().ok_or("output parent missing")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let temporary = parent.join(format!(
            ".{}.{}.tmp",
            target.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        if let Err(error) = file
            .write_all(body.as_bytes())
            .and_then(|_| file.sync_all())
            .and_then(|_| fs::rename(&temporary, &target))
        {
            let _ = fs::remove_file(&temporary);
            return Err(error.to_string());
        }
    }
    Ok(())
}
pub fn run(args: &[String], produced: Option<Value>) -> u8 {
    let result = (|| -> Result<(), String> {
        let (root, candidate, check) = arguments(args)?;
        let document = match candidate {
            Some(path) => read_candidate(&path)?,
            None => produced.ok_or("current native producer document missing")?,
        };
        let files = generate(&document)?;
        write_or_check(&root, &files, check)?;
        println!(
            "native OpenAPI 3.2 SDK {}: {} artifacts",
            if check { "verified" } else { "generated" },
            files.len()
        );
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("SDK generation: {error}");
            2
        }
    }
}
#[cfg(test)]
#[path = "codegen_tests.rs"]
mod tests;
