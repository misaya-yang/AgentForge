use serde::{
    Deserialize, Deserializer,
    de::{Error, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON value")
            }
            fn visit_bool<E: Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: Error>(self, v: f64) -> std::result::Result<Unique, E> {
                Ok(Unique(
                    serde_json::Number::from_f64(v)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                ))
            }
            fn visit_str<E: Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: Error>(self, v: String) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_none<E: Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut v = vec![];
                while let Some(Unique(item)) = a.next_element()? {
                    v.push(item);
                }
                Ok(Unique(Value::Array(v)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut v = serde_json::Map::new();
                while let Some((key, Unique(value))) = a.next_entry::<String, Unique>()? {
                    if v.contains_key(&key) {
                        return Err(A::Error::custom("DUPLICATE_KEY"));
                    }
                    v.insert(key, value);
                }
                Ok(Unique(Value::Object(v)))
            }
        }
        d.deserialize_any(V)
    }
}
pub fn json(text: &str) -> std::result::Result<Value, String> {
    serde_json::from_str::<Unique>(text)
        .map(|v| v.0)
        .map_err(|e| e.to_string())
}
fn expand_merges(value: &mut Value) -> std::result::Result<(), String> {
    match value {
        Value::Array(items) => {
            for item in items {
                expand_merges(item)?;
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                expand_merges(value)?;
            }
            if let Some(merge) = map.remove("<<") {
                let maps = match merge {
                    Value::Object(m) => vec![m],
                    Value::Array(values) => values
                        .into_iter()
                        .map(|v| match v {
                            Value::Object(m) => Ok(m),
                            _ => Err("invalid YAML merge".to_owned()),
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()?,
                    _ => return Err("invalid YAML merge".into()),
                };
                for merged in maps {
                    for (key, value) in merged {
                        if map.contains_key(&key) {
                            return Err("DUPLICATE_KEY".into());
                        }
                        map.insert(key, value);
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}
pub fn yaml(text: &str) -> std::result::Result<Value, String> {
    let normalized = crate::yaml11::normalize(text)?;
    let mut value = serde_yaml::from_str::<Unique>(&normalized)
        .map(|v| v.0)
        .map_err(|e| e.to_string())?;
    expand_merges(&mut value)?;
    Ok(value)
}
