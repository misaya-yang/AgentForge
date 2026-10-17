//! RFC 8785 JCS, explicitly versioned beside the frozen Python v1 verifier.
//! Input is I-JSON: reject duplicate names, invalid Unicode/nonfinite numbers,
//! preserve Unicode without normalization, sort property names by UTF-16 units,
//! and serialize binary64 with ECMAScript shortest-number spelling (ryu-js).
//! Numeric identifiers beyond binary64 precision must be represented as strings.
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt};

pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_DEPTH: usize = 128;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanonicalError {
    InvalidJson,
    DuplicateProperty,
    NonFiniteNumber,
    LimitExceeded,
    InvalidDigest,
    UnsupportedVersion,
}
impl fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "canonical_json_{self:?}")
    }
}
impl std::error::Error for CanonicalError {}
type Result<T> = std::result::Result<T, CanonicalError>;

/// A finite IEEE-754 binary64 value. JSON parsing follows the RFC's binary64
/// model; this type cannot be constructed with NaN/Infinity through safe APIs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FiniteNumber(f64);
impl FiniteNumber {
    pub fn new(value: f64) -> Result<Self> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(CanonicalError::NonFiniteNumber)
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum CanonicalValue {
    Null,
    Bool(bool),
    Number(FiniteNumber),
    String(String),
    Array(Vec<CanonicalValue>),
    Object(BTreeMap<String, CanonicalValue>),
}
impl CanonicalValue {
    pub fn number(value: f64) -> Result<Self> {
        FiniteNumber::new(value).map(Self::Number)
    }
    pub fn object(entries: impl IntoIterator<Item = (String, Self)>) -> Result<Self> {
        let mut map = BTreeMap::new();
        for (key, value) in entries {
            if map.insert(key, value).is_some() {
                return Err(CanonicalError::DuplicateProperty);
            }
        }
        Ok(Self::Object(map))
    }
}
struct JsonVisitor;
impl<'de> Visitor<'de> for JsonVisitor {
    type Value = CanonicalValue;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("I-JSON value with unique property names and finite binary64 numbers")
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(CanonicalValue::Null)
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Self::Value, E> {
        Ok(CanonicalValue::Bool(v))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
        Ok(CanonicalValue::String(v.into()))
    }
    fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Self::Value, E> {
        Ok(CanonicalValue::String(v))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
        self.visit_f64(v as f64)
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
        self.visit_f64(v as f64)
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
        CanonicalValue::number(v).map_err(E::custom)
    }
    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut values = vec![];
        while let Some(value) = seq.next_element()? {
            values.push(value);
        }
        Ok(CanonicalValue::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut values = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("canonical_duplicate_property"));
            }
            values.insert(key, map.next_value()?);
        }
        Ok(CanonicalValue::Object(values))
    }
}
impl<'de> Deserialize<'de> for CanonicalValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CanonicalDocument(CanonicalValue);
impl CanonicalDocument {
    pub fn from_json(input: &[u8]) -> Result<Self> {
        if input.len() > MAX_DOCUMENT_BYTES {
            return Err(CanonicalError::LimitExceeded);
        }
        // serde_json rejects unpaired surrogates, nonfinite number literals,
        // trailing content and excessive recursion. Our visitor rejects duplicate
        // keys before any map can silently overwrite their original values.
        let value = serde_json::from_slice(input).map_err(|e| {
            if e.to_string().contains("canonical_duplicate_property") {
                CanonicalError::DuplicateProperty
            } else {
                CanonicalError::InvalidJson
            }
        })?;
        Ok(Self(value))
    }
    pub fn new(value: CanonicalValue) -> Self {
        Self(value)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut output = String::new();
        encode(&self.0, &mut output, 0)?;
        Ok(output.into_bytes())
    }
    pub fn sha256_hex(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(self.to_bytes()?)))
    }
}
fn encode(value: &CanonicalValue, output: &mut String, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH || output.len() > MAX_DOCUMENT_BYTES {
        return Err(CanonicalError::LimitExceeded);
    }
    match value {
        CanonicalValue::Null => output.push_str("null"),
        CanonicalValue::Bool(v) => output.push_str(if *v { "true" } else { "false" }),
        CanonicalValue::Number(v) => {
            if v.0 == 0.0 {
                output.push('0');
            } else {
                output.push_str(ryu_js::Buffer::new().format_finite(v.0));
            }
        }
        CanonicalValue::String(s) => encode_string(s, output),
        CanonicalValue::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                encode(value, output, depth + 1)?;
            }
            output.push(']');
        }
        CanonicalValue::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            output.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                encode_string(key, output);
                output.push(':');
                encode(&values[key], output, depth + 1)?;
            }
            output.push('}');
        }
    }
    if output.len() > MAX_DOCUMENT_BYTES {
        return Err(CanonicalError::LimitExceeded);
    }
    Ok(())
}
fn encode_string(value: &str, output: &mut String) {
    use fmt::Write;
    output.push('"');
    for ch in value.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{09}' => output.push_str("\\t"),
            '\u{0a}' => output.push_str("\\n"),
            '\u{0c}' => output.push_str("\\f"),
            '\u{0d}' => output.push_str("\\r"),
            ch if ch < '\u{20}' => {
                write!(output, "\\u{:04x}", ch as u32).expect("String formatting");
            }
            ch => output.push(ch),
        }
    }
    output.push('"');
}

/// The caller must select a declared version, never try v1 after v2 failure.
/// Old proof/hash call sites remain on their existing frozen v1 functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanonicalVersion {
    PythonV1 { ascii: bool, spaced: bool },
    JcsV2,
}
impl CanonicalVersion {
    /// V2 is selected only by its explicit wire name; no unknown-version fallback.
    /// V1 spelling flags remain a property of the historical owning protocol.
    pub fn from_wire(name: &str, legacy_ascii: bool, legacy_spaced: bool) -> Result<Self> {
        match name {
            "python_json/v1" => Ok(Self::PythonV1 {
                ascii: legacy_ascii,
                spaced: legacy_spaced,
            }),
            "jcs/v2" => Ok(Self::JcsV2),
            _ => Err(CanonicalError::UnsupportedVersion),
        }
    }
}
pub fn verify_json_hash(version: CanonicalVersion, input: &[u8], expected: &str) -> Result<bool> {
    if expected.len() != 64
        || !expected
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(CanonicalError::InvalidDigest);
    }
    if input.len() > MAX_DOCUMENT_BYTES {
        return Err(CanonicalError::LimitExceeded);
    }
    let actual = match version {
        CanonicalVersion::JcsV2 => CanonicalDocument::from_json(input)?.sha256_hex()?,
        CanonicalVersion::PythonV1 { ascii, spaced } => {
            let value: serde_json::Value =
                serde_json::from_slice(input).map_err(|_| CanonicalError::InvalidJson)?;
            format!(
                "{:x}",
                Sha256::digest(crate::python_json::python_json(&value, ascii, spaced).as_bytes())
            )
        }
    };
    // These are public content hashes, not MACs. Signature verification stays at
    // existing authorities; equality here must never substitute for authority.
    Ok(actual == expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn canonical(input: &str) -> String {
        String::from_utf8(
            CanonicalDocument::from_json(input.as_bytes())
                .unwrap()
                .to_bytes()
                .unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn rfc8785_section_3_2_2_sample() {
        let input = r#"{"numbers":[333333333.33333329,1E30,4.50,2e-3,0.000000000000000000000000001],"string":"\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"/","literals":[null,true,false]}"#;
        let expected = "{\"literals\":[null,true,false],\"numbers\":[333333333.3333333,1e+30,4.5,0.002,1e-27],\"string\":\"€$\\u000f\\nA'B\\\"\\\\\\\\\\\"/\"}";
        assert_eq!(canonical(input), expected);
    }
    #[test]
    fn rfc8785_appendix_b_number_vectors() {
        for (bits, expected) in [
            (0x0000000000000000, "0"),
            (0x8000000000000000, "0"),
            (0x0000000000000001, "5e-324"),
            (0x8000000000000001, "-5e-324"),
            (0x7fefffffffffffff, "1.7976931348623157e+308"),
            (0x4340000000000000, "9007199254740992"),
            (0x4430000000000000, "295147905179352830000"),
            (0x44b52d02c7e14af5, "9.999999999999997e+22"),
            (0x44b52d02c7e14af6, "1e+23"),
            (0x44b52d02c7e14af7, "1.0000000000000001e+23"),
            (0x444b1ae4d6e2ef4e, "999999999999999700000"),
            (0x444b1ae4d6e2ef4f, "999999999999999900000"),
            (0x444b1ae4d6e2ef50, "1e+21"),
            (0x3eb0c6f7a0b5ed8c, "9.999999999999997e-7"),
            (0x3eb0c6f7a0b5ed8d, "0.000001"),
            (0x41b3de4355555553, "333333333.3333332"),
            (0x41b3de4355555554, "333333333.33333325"),
            (0x41b3de4355555555, "333333333.3333333"),
            (0x41b3de4355555556, "333333333.3333334"),
            (0x41b3de4355555557, "333333333.33333343"),
            (0xbecbf647612f3696, "-0.0000033333333333333333"),
            (0x43143ff3c1cb0959, "1424953923781206.2"),
        ] {
            let value = CanonicalValue::number(f64::from_bits(bits)).unwrap();
            assert_eq!(
                String::from_utf8(CanonicalDocument::new(value).to_bytes().unwrap()).unwrap(),
                expected,
                "{bits:016x}"
            );
        }
    }
    #[test]
    fn utf16_order_and_unicode_preservation() {
        assert_eq!(
            canonical(
                r#"{"\u20ac":"Euro Sign","\r":"Carriage Return","\ufb33":"Hebrew Letter Dalet With Dagesh","1":"One","\ud83d\ude00":"Emoji: Grinning Face","\u0080":"Control","\u00f6":"Latin Small Letter O With Diaeresis"}"#
            ),
            "{\"\\r\":\"Carriage Return\",\"1\":\"One\",\"\u{80}\":\"Control\",\"ö\":\"Latin Small Letter O With Diaeresis\",\"€\":\"Euro Sign\",\"😀\":\"Emoji: Grinning Face\",\"דּ\":\"Hebrew Letter Dalet With Dagesh\"}"
        );
        assert_ne!(canonical(r#""é""#), canonical(r#""e\u0301""#));
    }
    #[test]
    fn rejects_duplicate_names_invalid_unicode_and_nonfinite() {
        assert_eq!(
            CanonicalDocument::from_json(br#"{"a":1,"\u0061":2}"#),
            Err(CanonicalError::DuplicateProperty)
        );
        assert_eq!(
            CanonicalDocument::from_json(br#"{"nested":{"a":1,"a":2}}"#),
            Err(CanonicalError::DuplicateProperty)
        );
        for input in [r#""\ud800""#, "NaN", "Infinity", "1e999"] {
            assert!(CanonicalDocument::from_json(input.as_bytes()).is_err());
        }
        assert_eq!(
            CanonicalValue::number(f64::NAN),
            Err(CanonicalError::NonFiniteNumber)
        );
        assert_eq!(
            CanonicalValue::number(f64::INFINITY),
            Err(CanonicalError::NonFiniteNumber)
        );
    }
    #[test]
    fn sha256_vector_and_explicit_version_selection() {
        assert_eq!(
            CanonicalDocument::from_json(b"{}")
                .unwrap()
                .sha256_hex()
                .unwrap(),
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        let jcs = CanonicalDocument::from_json(b"{\"x\":1.0}")
            .unwrap()
            .sha256_hex()
            .unwrap();
        assert!(verify_json_hash(CanonicalVersion::JcsV2, b"{\"x\":1.0}", &jcs).unwrap());
        assert!(
            !verify_json_hash(
                CanonicalVersion::PythonV1 {
                    ascii: false,
                    spaced: false
                },
                b"{\"x\":1.0}",
                &jcs
            )
            .unwrap()
        );
    }
    #[test]
    fn frozen_v1_hash_verifies_without_reinterpreting_as_jcs() {
        // Python json.dumps(sort_keys=True,separators=(',',':'),ensure_ascii=False).
        let expected = "bf32f56236899e13ef54db875d136c6cbcc65244464829c54911aa9069b0ae25";
        assert!(
            verify_json_hash(
                CanonicalVersion::PythonV1 {
                    ascii: false,
                    spaced: false
                },
                b"{\"x\":1.0}",
                expected
            )
            .unwrap()
        );
        assert!(!verify_json_hash(CanonicalVersion::JcsV2, b"{\"x\":1.0}", expected).unwrap());
    }
}
