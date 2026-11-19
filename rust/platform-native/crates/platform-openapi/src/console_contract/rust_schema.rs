//! Read serde request DTOs from their actual owner source without linking services.
//! Unsupported wire types fail generation rather than becoming an `unknown` body.
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use syn::{Fields, GenericArgument, Item, PathArguments, Type};

pub(super) struct Registry {
    items: BTreeMap<String, Item>,
}
fn attribute(attrs: &[syn::Attribute], key: &str) -> Option<String> {
    for attr in attrs.iter().filter(|a| a.path().is_ident("serde")) {
        if let syn::Meta::List(meta) = &attr.meta {
            let text = meta.tokens.to_string();
            let pattern = format!(r#"(?:^|,)\s*{}\s*=\s*"([^"]+)""#, regex::escape(key));
            if let Some(value) = regex::Regex::new(&pattern).unwrap().captures(&text) {
                return Some(value[1].to_owned());
            }
            if text.split(',').any(|part| part.trim() == key) {
                return Some(String::new());
            }
        }
    }
    None
}
fn renamed(name: &str, rule: Option<&str>) -> String {
    match rule {
        Some("snake_case") => {
            let mut out = String::new();
            for (i, c) in name.chars().enumerate() {
                if c.is_uppercase() && i > 0 {
                    out.push('_');
                }
                out.extend(c.to_lowercase());
            }
            out
        }
        Some("lowercase") => name.to_lowercase(),
        Some("camelCase") => {
            let mut upper = false;
            name.chars()
                .flat_map(|c| {
                    if c == '_' {
                        upper = true;
                        Vec::new()
                    } else if upper {
                        upper = false;
                        c.to_uppercase().collect()
                    } else {
                        vec![c]
                    }
                })
                .collect()
        }
        None => name.to_owned(),
        other => panic!("unsupported serde naming rule {other:?}"),
    }
}
impl Registry {
    pub(super) fn new(sources: &[&str]) -> Self {
        fn collect(items: Vec<Item>, output: &mut BTreeMap<String, Item>) {
            for item in items {
                let name = match &item {
                    Item::Struct(v) => Some(v.ident.to_string()),
                    Item::Enum(v) => Some(v.ident.to_string()),
                    Item::Type(v) => Some(v.ident.to_string()),
                    _ => None,
                };
                if let Some(name) = name {
                    output.entry(name).or_insert(item);
                } else if let Item::Mod(module) = item {
                    if let Some((_, items)) = module.content {
                        collect(items, output);
                    }
                }
            }
        }
        let mut items = BTreeMap::new();
        for source in sources {
            collect(
                syn::parse_file(source)
                    .expect("owner source must parse")
                    .items,
                &mut items,
            );
        }
        Self { items }
    }
    pub(super) fn schema(&self, name: &str) -> Value {
        self.named(name, 0)
    }
    fn named(&self, name: &str, depth: usize) -> Value {
        assert!(depth < 24, "recursive/unbounded request DTO {name}");
        match self
            .items
            .get(name)
            .unwrap_or_else(|| panic!("unresolved owner request DTO {name}"))
        {
            Item::Type(v) => self.ty(&v.ty, depth + 1),
            Item::Struct(v) => self.fields(&v.fields, &v.attrs, depth + 1),
            Item::Enum(v) => {
                let rule = attribute(&v.attrs, "rename_all");
                let tag = attribute(&v.attrs, "tag");
                let content = attribute(&v.attrs, "content");
                let untagged = attribute(&v.attrs, "untagged").is_some();
                let all_units = v.variants.iter().all(|v| matches!(v.fields, Fields::Unit));
                if all_units && !untagged && tag.is_none() {
                    return json!({"type":"string","enum":v.variants.iter().map(|v|attribute(&v.attrs,"rename").unwrap_or_else(||renamed(&v.ident.to_string(),rule.as_deref()))).collect::<Vec<_>>()});
                }
                json!({"oneOf":v.variants.iter().map(|variant| {
                    let name=attribute(&variant.attrs,"rename").unwrap_or_else(||renamed(&variant.ident.to_string(),rule.as_deref()));
                    let fields=self.fields(&variant.fields,&variant.attrs,depth+1);
                    if untagged { fields }
                    else if let Some(tag)=&tag {
                        let mut props=Map::new();props.insert(tag.clone(),json!({"const":name}));let mut required=vec![tag.clone()];
                        if let Some(content)=&content { if !matches!(variant.fields,Fields::Unit) {props.insert(content.clone(),fields);required.push(content.clone());} }
                        else if let Some(values)=fields["properties"].as_object() {props.extend(values.clone());required.extend(fields["required"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned));}
                        json!({"type":"object","properties":props,"required":required,"additionalProperties":false})
                    } else if matches!(variant.fields,Fields::Unit) {json!({"const":name})}
                    else {json!({"type":"object","required":[name.clone()],"properties":{name:fields},"additionalProperties":false})}
                }).collect::<Vec<_>>()})
            }
            _ => unreachable!(),
        }
    }
    fn fields(&self, fields: &Fields, attrs: &[syn::Attribute], depth: usize) -> Value {
        match fields {
            Fields::Unit => json!({"type":"null"}),
            Fields::Unnamed(v) if v.unnamed.len() == 1 => self.ty(&v.unnamed[0].ty, depth + 1),
            Fields::Unnamed(v) => {
                json!({"type":"array","prefixItems":v.unnamed.iter().map(|f|self.ty(&f.ty,depth+1)).collect::<Vec<_>>(),"minItems":v.unnamed.len(),"maxItems":v.unnamed.len()})
            }
            Fields::Named(v) => {
                let mut props = Map::new();
                let mut required = Vec::new();
                let mut extra = attribute(attrs, "deny_unknown_fields").is_none();
                let rule = attribute(attrs, "rename_all");
                let default = attribute(attrs, "default").is_some();
                for field in &v.named {
                    if attribute(&field.attrs, "skip").is_some()
                        || attribute(&field.attrs, "skip_deserializing").is_some()
                    {
                        continue;
                    }
                    let shape = self.ty(&field.ty, depth + 1);
                    if attribute(&field.attrs, "flatten").is_some() {
                        if let Some(values) = shape["properties"].as_object() {
                            props.extend(values.clone());
                            required.extend(
                                shape["required"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned),
                            );
                        }
                        extra |= shape["additionalProperties"] != false;
                        continue;
                    }
                    let name = attribute(&field.attrs, "rename").unwrap_or_else(|| {
                        renamed(&field.ident.as_ref().unwrap().to_string(), rule.as_deref())
                    });
                    if !default
                        && attribute(&field.attrs, "default").is_none()
                        && !optional(&field.ty)
                    {
                        required.push(name.clone());
                    }
                    props.insert(name, shape);
                }
                json!({"type":"object","properties":props,"required":required,"additionalProperties":extra})
            }
        }
    }
    fn ty(&self, ty: &Type, depth: usize) -> Value {
        if let Type::Reference(v) = ty {
            return self.ty(&v.elem, depth + 1);
        }
        if let Type::Slice(v) = ty {
            return json!({"type":"array","items":self.ty(&v.elem,depth+1)});
        }
        if let Type::Array(v) = ty {
            return json!({"type":"array","items":self.ty(&v.elem,depth+1)});
        }
        let Type::Path(path) = ty else {
            panic!("unsupported wire request type");
        };
        let segment = path.path.segments.last().expect("type path");
        let name = segment.ident.to_string();
        let args: Vec<_> = if let PathArguments::AngleBracketed(args) = &segment.arguments {
            args.args
                .iter()
                .filter_map(|v| {
                    if let GenericArgument::Type(t) = v {
                        Some(t)
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            vec![]
        };
        match name.as_str() {
            "String" | "str" | "Uuid" | "Url" | "DateTime" | "NaiveDateTime" | "NaiveDate" => {
                json!({"type":"string"})
            }
            "bool" => json!({"type":"boolean"}),
            "u8" | "u16" | "u32" | "u64" | "usize" => json!({"type":"integer","minimum":0}),
            "i8" | "i16" | "i32" | "i64" | "isize" => json!({"type":"integer"}),
            "f32" | "f64" | "Decimal" => json!({"type":"number"}),
            "Value" => json!({}),
            "Option" => json!({"anyOf":[self.ty(args[0],depth+1),{"type":"null"}]}),
            "Vec" | "BTreeSet" | "HashSet" => {
                json!({"type":"array","items":self.ty(args[0],depth+1)})
            }
            "Map" | "BTreeMap" | "HashMap" => {
                json!({"type":"object","additionalProperties":self.ty(args[1],depth+1)})
            }
            "Box" | "Arc" => self.ty(args[0], depth + 1),
            _ => self.named(&name, depth + 1),
        }
    }
}
fn optional(ty: &Type) -> bool {
    matches!(ty,Type::Path(p) if p.path.segments.last().is_some_and(|s|s.ident=="Option"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_serde_required_optional_default_and_closed_names_are_preserved() {
        let registry = Registry::new(&[
            r#"#[serde(deny_unknown_fields)] struct Input { name:String, optional:Option<u64>, #[serde(default)] enabled:bool, #[serde(rename="wireName")] local:String }"#,
        ]);
        let s = registry.schema("Input");
        assert_eq!(s["required"], json!(["name", "wireName"]));
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["properties"]["optional"]["anyOf"][1]["type"], "null");
    }
    #[test]
    #[should_panic(expected = "unresolved owner request DTO")]
    fn unknown_types_never_silently_generate_an_unknown_request_body() {
        Registry::new(&["struct Input { value:MissingDto }"]).schema("Input");
    }
}
