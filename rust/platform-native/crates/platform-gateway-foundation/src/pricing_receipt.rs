//! Immutable price-receipt compatibility with
//! `packages/ai-gateway-contracts/src/ai_gateway_contracts/pricing.py`.
//!
//! Price strings, USD/per-1K units, identity binding, default status, compact
//! sorted ASCII JSON, and SHA-256 versions follow that source. Canonical decimal
//! normalization uses its default Python Decimal context: 28 significant digits,
//! half-even rounding, fixed-point output, and a preserved negative-zero sign.
//! Receipt normalization is intentionally separate from settlement arithmetic:
//! a valid Python receipt can contain a price beyond `billing::TokenRates`' range.
//!
//! A version is a deterministic checksum, not a signature or pricing authority.
//! Callers must only freeze receipts from an authorized price-selection path and
//! retain the first receipt for subsequent cumulative usage observations.
//!
//! Explicit portability limits: decimal literals/canonical values are bounded
//! to 4 KiB, digits must be ASCII, and truthy object/array pricing statuses are
//! rejected instead of guessing Python object repr. Python non-default decimal
//! contexts, extreme exponents, Unicode numeric glyphs, and arbitrary-precision
//! JSON numbers are outside this adapter. Send prices as strings for fidelity.
//! No storage, credentials, pricing lookup, or request caches live here.

use serde::Serialize;
use serde_json::{Number, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt::{self, Write},
};

pub const DEFAULT_PRICING_STATUS: &str = "tenant_model";
pub const PRICING_CURRENCY: &str = "USD";
pub const PRICING_UNIT: &str = "per_1k_tokens";
const DECIMAL_PRECISION: usize = 28;
const MAX_DECIMAL_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingReceiptError {
    InvalidPricingSnapshot,
    IdentityMismatch,
    InvalidModelPrice,
    VersionMismatch,
    UnsupportedDecimalRange,
    UnsupportedPricingStatus,
}

impl fmt::Display for PricingReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPricingSnapshot => "invalid_pricing_snapshot",
            Self::IdentityMismatch => "pricing_snapshot_identity_mismatch",
            Self::InvalidModelPrice => "invalid_model_price",
            Self::VersionMismatch => "pricing_snapshot_version_mismatch",
            Self::UnsupportedDecimalRange => "pricing_snapshot_decimal_range_unsupported",
            Self::UnsupportedPricingStatus => "pricing_snapshot_status_type_unsupported",
        })
    }
}

impl std::error::Error for PricingReceiptError {}

/// Do not deserialize external data straight into this struct: use
/// `validate_pricing_snapshot` to check identity, units, normalized terms, and hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PricingSnapshot {
    pub tenant_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub currency: String,
    pub unit: String,
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    pub pricing_status: String,
    pub version: String,
}

impl PricingSnapshot {
    /// Exact bytes fed to hashlib.sha256 by Python, excluding `version`.
    pub fn canonical_terms_json(&self) -> String {
        let terms = BTreeMap::from([
            ("tenant_id", self.tenant_id.as_str()),
            ("provider_id", self.provider_id.as_str()),
            ("model_id", self.model_id.as_str()),
            ("currency", self.currency.as_str()),
            ("unit", self.unit.as_str()),
            ("input_price_per_1k", self.input_price_per_1k.as_str()),
            ("output_price_per_1k", self.output_price_per_1k.as_str()),
            ("pricing_status", self.pricing_status.as_str()),
        ]);
        let utf8 = serde_json::to_string(&terms).expect("string-only terms serialize");
        let mut ascii = String::with_capacity(utf8.len());
        for character in utf8.chars() {
            // Python ensure_ascii also escapes DEL (U+007F).
            if character <= '\u{7e}' {
                ascii.push(character);
            } else {
                let mut units = [0_u16; 2];
                for unit in character.encode_utf16(&mut units) {
                    write!(ascii, "\\u{unit:04x}").expect("writing to String succeeds");
                }
            }
        }
        ascii
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("string-only snapshot serializes")
    }
}

/// `str(Decimal(...).normalize())` is not the wire contract: Python explicitly
/// uses `format(..., "f")`, so no exponent may remain in the hashed terms.
pub fn normalize_price(value: &str) -> Result<String, PricingReceiptError> {
    if value.len() > MAX_DECIMAL_BYTES {
        return Err(PricingReceiptError::UnsupportedDecimalRange);
    }
    // Decimal accepts underscores anywhere in its constructor's literal.
    let cleaned = value.trim().replace('_', "");
    let (negative, unsigned) = if let Some(value) = cleaned.strip_prefix('-') {
        (true, value)
    } else {
        (false, cleaned.strip_prefix('+').unwrap_or(&cleaned))
    };
    let mut parts = unsigned.split(['e', 'E']);
    let coefficient = parts.next().ok_or(PricingReceiptError::InvalidModelPrice)?;
    let exponent = match parts.next() {
        None => 0_i64,
        Some(value) => {
            let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
            if digits.is_empty() || !digits.bytes().all(|digit| digit.is_ascii_digit()) {
                return Err(PricingReceiptError::InvalidModelPrice);
            }
            value
                .parse::<i64>()
                .map_err(|_| PricingReceiptError::UnsupportedDecimalRange)?
        }
    };
    if parts.next().is_some() {
        return Err(PricingReceiptError::InvalidModelPrice);
    }
    let mut digits = Vec::with_capacity(coefficient.len());
    let mut decimal_seen = false;
    let mut fractional_digits = 0_i64;
    for character in coefficient.bytes() {
        match character {
            b'.' if !decimal_seen => decimal_seen = true,
            b'0'..=b'9' => {
                digits.push(character);
                if decimal_seen {
                    fractional_digits += 1;
                }
            }
            _ => return Err(PricingReceiptError::InvalidModelPrice),
        }
    }
    if digits.is_empty() {
        return Err(PricingReceiptError::InvalidModelPrice);
    }
    let Some(first_nonzero) = digits.iter().position(|digit| *digit != b'0') else {
        return Ok(if negative { "-0" } else { "0" }.to_owned());
    };
    if negative {
        return Err(PricingReceiptError::InvalidModelPrice);
    }
    digits.drain(..first_nonzero);
    let mut exponent = exponent
        .checked_sub(fractional_digits)
        .ok_or(PricingReceiptError::UnsupportedDecimalRange)?;
    // normalize() applies the context before trimming insignificant zeros.
    if digits.len() > DECIMAL_PRECISION {
        let discarded = &digits[DECIMAL_PRECISION..];
        let round_up = discarded[0] > b'5'
            || (discarded[0] == b'5'
                && (discarded[1..].iter().any(|digit| *digit != b'0')
                    || (digits[DECIMAL_PRECISION - 1] - b'0') % 2 == 1));
        exponent = exponent
            .checked_add((digits.len() - DECIMAL_PRECISION) as i64)
            .ok_or(PricingReceiptError::UnsupportedDecimalRange)?;
        digits.truncate(DECIMAL_PRECISION);
        if round_up {
            let mut carry = true;
            for digit in digits.iter_mut().rev() {
                if *digit == b'9' {
                    *digit = b'0';
                } else {
                    *digit += 1;
                    carry = false;
                    break;
                }
            }
            if carry {
                digits.insert(0, b'1');
            }
        }
    }
    while digits.last() == Some(&b'0') {
        digits.pop();
        exponent = exponent
            .checked_add(1)
            .ok_or(PricingReceiptError::UnsupportedDecimalRange)?;
    }
    let decimal_point = (digits.len() as i64)
        .checked_add(exponent)
        .ok_or(PricingReceiptError::UnsupportedDecimalRange)?;
    let output_len = if decimal_point <= 0 {
        2_i64
            .checked_sub(decimal_point)
            .and_then(|length| length.checked_add(digits.len() as i64))
    } else if decimal_point >= digits.len() as i64 {
        Some(decimal_point)
    } else {
        Some(digits.len() as i64 + 1)
    }
    .ok_or(PricingReceiptError::UnsupportedDecimalRange)?;
    if output_len > MAX_DECIMAL_BYTES as i64 {
        return Err(PricingReceiptError::UnsupportedDecimalRange);
    }
    let digits = String::from_utf8(digits).expect("ASCII decimal digits");
    let mut normalized = String::with_capacity(output_len as usize);
    if decimal_point <= 0 {
        normalized.push_str("0.");
        normalized.extend(std::iter::repeat_n('0', (-decimal_point) as usize));
        normalized.push_str(&digits);
    } else if decimal_point >= digits.len() as i64 {
        normalized.push_str(&digits);
        normalized.extend(std::iter::repeat_n(
            '0',
            decimal_point as usize - digits.len(),
        ));
    } else {
        let point = decimal_point as usize;
        normalized.push_str(&digits[..point]);
        normalized.push('.');
        normalized.push_str(&digits[point..]);
    }
    Ok(normalized)
}

pub fn build_pricing_snapshot(
    tenant_id: &str,
    provider_id: &str,
    model_id: &str,
    input_price_per_1k: &str,
    output_price_per_1k: &str,
    pricing_status: &str,
) -> Result<PricingSnapshot, PricingReceiptError> {
    let mut snapshot = PricingSnapshot {
        tenant_id: tenant_id.to_owned(),
        provider_id: provider_id.to_owned(),
        model_id: model_id.to_owned(),
        currency: PRICING_CURRENCY.to_owned(),
        unit: PRICING_UNIT.to_owned(),
        input_price_per_1k: normalize_price(input_price_per_1k)?,
        output_price_per_1k: normalize_price(output_price_per_1k)?,
        pricing_status: pricing_status.to_owned(),
        version: String::new(),
    };
    snapshot.version = format!(
        "{:x}",
        Sha256::digest(snapshot.canonical_terms_json().as_bytes())
    );
    Ok(snapshot)
}

/// Python float str differs from JSON's spelling at exponent boundaries. Keep
/// that distinction for scalar pricing_status coercion and JSON-number prices.
fn python_number_string(value: &Number) -> String {
    if value.is_i64() || value.is_u64() {
        return value.to_string();
    }
    let raw = value.to_string();
    let (sign, raw) = raw
        .strip_prefix('-')
        .map_or(("", raw.as_str()), |value| ("-", value));
    let mut parts = raw.split(['e', 'E']);
    let coefficient = parts.next().expect("JSON number has coefficient");
    let exponent = parts.next().map_or(0, |value| {
        value.parse::<i32>().expect("finite JSON exponent")
    });
    let fraction = coefficient
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let mut digits = coefficient
        .replace('.', "")
        .trim_start_matches('0')
        .to_owned();
    if digits.is_empty() {
        return format!("{sign}0.0");
    }
    let mut exponent = exponent - fraction as i32;
    while digits.ends_with('0') {
        digits.pop();
        exponent += 1;
    }
    let adjusted = digits.len() as i32 + exponent - 1;
    if !(-4..16).contains(&adjusted) {
        let tail = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        let exponent_sign = if adjusted < 0 { '-' } else { '+' };
        format!(
            "{sign}{}{tail}e{exponent_sign}{:02}",
            &digits[..1],
            adjusted.abs()
        )
    } else {
        let point = digits.len() as i32 + exponent;
        if point <= 0 {
            format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
        } else if point >= digits.len() as i32 {
            format!(
                "{sign}{digits}{}.0",
                "0".repeat(point as usize - digits.len())
            )
        } else {
            format!(
                "{sign}{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        }
    }
}

fn price_input(value: Option<&Value>) -> Result<String, PricingReceiptError> {
    match value {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(Value::Number(value)) => Ok(python_number_string(value)),
        _ => Err(PricingReceiptError::InvalidModelPrice),
    }
}

fn status_input(value: Option<&Value>) -> Result<String, PricingReceiptError> {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => {
            Ok(DEFAULT_PRICING_STATUS.to_owned())
        }
        Some(Value::String(value)) if value.is_empty() => Ok(DEFAULT_PRICING_STATUS.to_owned()),
        Some(Value::Array(value)) if value.is_empty() => Ok(DEFAULT_PRICING_STATUS.to_owned()),
        Some(Value::Object(value)) if value.is_empty() => Ok(DEFAULT_PRICING_STATUS.to_owned()),
        Some(Value::Number(value)) if value.as_f64() == Some(0.0) => {
            Ok(DEFAULT_PRICING_STATUS.to_owned())
        }
        Some(Value::String(value)) => Ok(value.clone()),
        Some(Value::Bool(true)) => Ok("True".to_owned()),
        Some(Value::Number(value)) => Ok(python_number_string(value)),
        _ => Err(PricingReceiptError::UnsupportedPricingStatus),
    }
}

pub fn validate_pricing_snapshot(
    value: &Value,
    tenant_id: &str,
    provider_id: &str,
    model_id: &str,
) -> Result<PricingSnapshot, PricingReceiptError> {
    let value = value
        .as_object()
        .ok_or(PricingReceiptError::InvalidPricingSnapshot)?;
    for (key, expected) in [
        ("tenant_id", tenant_id),
        ("provider_id", provider_id),
        ("model_id", model_id),
        ("currency", PRICING_CURRENCY),
        ("unit", PRICING_UNIT),
    ] {
        if value.get(key).and_then(Value::as_str) != Some(expected) {
            return Err(PricingReceiptError::IdentityMismatch);
        }
    }
    let receipt = build_pricing_snapshot(
        tenant_id,
        provider_id,
        model_id,
        &price_input(value.get("input_price_per_1k"))?,
        &price_input(value.get("output_price_per_1k"))?,
        &status_input(value.get("pricing_status"))?,
    )?;
    if value.get("version").and_then(Value::as_str) != Some(receipt.version.as_str()) {
        return Err(PricingReceiptError::VersionMismatch);
    }
    Ok(receipt)
}

