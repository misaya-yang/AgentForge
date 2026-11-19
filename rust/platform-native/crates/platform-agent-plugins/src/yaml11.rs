//! Resolve legacy implicit scalars from parser events, preserving quoted text.
//! Libyaml is already the parser underlying serde_yaml. No textual line guessing.
use unsafe_libyaml::*;
pub fn normalize(text: &str) -> Result<String, String> {
    // SAFETY: parser and events are initialized by libyaml, kept within input
    // lifetime, and deleted exactly once. Event-owned pointers are copied before
    // deletion; all replacement byte ranges are checked against the UTF-8 input.
    unsafe {
        let mut parser = std::mem::MaybeUninit::<yaml_parser_t>::uninit();
        if !yaml_parser_initialize(parser.as_mut_ptr()).ok {
            return Err("YAML parser initialization failed".into());
        }
        let mut parser = parser.assume_init();
        yaml_parser_set_input_string(&mut parser, text.as_ptr(), text.len() as u64);
        let mut changes = vec![];
        let result = (|| {
            loop {
                let mut event = std::mem::MaybeUninit::<yaml_event_t>::uninit();
                if !yaml_parser_parse(&mut parser, event.as_mut_ptr()).ok {
                    return Err("invalid YAML".to_owned());
                }
                let mut event = event.assume_init();
                let done = event.type_ == YAML_STREAM_END_EVENT;
                if event.type_ == YAML_SCALAR_EVENT {
                    let scalar = event.data.scalar;
                    if scalar.style == YAML_PLAIN_SCALAR_STYLE && scalar.plain_implicit {
                        let value = std::str::from_utf8(std::slice::from_raw_parts(
                            scalar.value,
                            scalar.length as usize,
                        ))
                        .map_err(|_| "invalid YAML UTF-8".to_owned());
                        if let Ok(value) = value {
                            if let Some(replacement) = resolved(value) {
                                let start = event.start_mark.index as usize;
                                let end = event.end_mark.index as usize;
                                if text.get(start..end).is_none() {
                                    yaml_event_delete(&mut event);
                                    return Err("invalid YAML scalar range".into());
                                }
                                let anchor = if scalar.anchor.is_null() {
                                    String::new()
                                } else {
                                    format!(
                                        "&{} ",
                                        std::ffi::CStr::from_ptr(scalar.anchor.cast())
                                            .to_string_lossy()
                                    )
                                };
                                changes.push((start, end, format!("{anchor}{replacement}")));
                            }
                        }
                    }
                }
                yaml_event_delete(&mut event);
                if done {
                    break;
                }
            }
            Ok(())
        })();
        yaml_parser_delete(&mut parser);
        result?;
        let mut output = text.to_owned();
        for (start, end, value) in changes.into_iter().rev() {
            output.replace_range(start..end, &value)
        }
        Ok(output)
    }
}
fn resolved(value: &str) -> Option<String> {
    match value {
        "yes" | "Yes" | "YES" | "on" | "On" | "ON" => return Some("true".into()),
        "no" | "No" | "NO" | "off" | "Off" | "OFF" => return Some("false".into()),
        _ => {}
    }
    let compact = value.replace('_', "");
    let (sign, digits) = compact
        .strip_prefix('-')
        .map(|s| (-1i64, s))
        .or_else(|| compact.strip_prefix('+').map(|s| (1, s)))
        .unwrap_or((1, &compact));
    if digits.starts_with("0o") {
        return serde_json::to_string(value).ok();
    }
    if digits.len() > 1
        && digits.starts_with('0')
        && digits.bytes().all(|c| (b'0'..=b'7').contains(&c))
    {
        return i64::from_str_radix(&digits[1..], 8)
            .ok()
            .and_then(|n| n.checked_mul(sign))
            .map(|n| n.to_string());
    }
    if digits.len() > 1 && digits.starts_with('0') && digits.bytes().all(|c| c.is_ascii_digit()) {
        return serde_json::to_string(value).ok();
    }
    if digits.contains(':') {
        let parts = digits.split(':').collect::<Vec<_>>();
        if parts[0].starts_with(|c: char| ('1'..='9').contains(&c))
            && parts[0].bytes().all(|c| c.is_ascii_digit())
            && parts[1..]
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 2 && p.parse::<u32>().is_ok_and(|n| n < 60))
        {
            return parts
                .iter()
                .try_fold(0i64, |n, p| {
                    n.checked_mul(60)?.checked_add(p.parse::<i64>().ok()?)
                })
                .and_then(|n| n.checked_mul(sign))
                .map(|n| n.to_string());
        }
    }
    // PyYAML only resolves scientific notation when a decimal point and an
    // explicitly signed exponent are present; serde_yaml also resolves 1e3.
    if let Some((mantissa, exponent)) = digits.split_once(['e', 'E']) {
        if !mantissa.contains('.') || !exponent.starts_with(['+', '-']) {
            if compact.parse::<f64>().is_ok() {
                return serde_json::to_string(value).ok();
            }
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_events_preserve_quoted_and_block_scalars() {
        let source = "plain: on\nquoted: 'on'\ndouble: \"yes\"\nblock: |\n  off\nflow: [YES, Off, 'NO']\nanchor: &flag no\nalias: *flag\n";
        let v: serde_json::Value = serde_yaml::from_str(&normalize(source).unwrap()).unwrap();
        assert_eq!(v["plain"], true);
        assert_eq!(v["quoted"], "on");
        assert_eq!(v["double"], "yes");
        assert_eq!(v["block"], "off\n");
        assert_eq!(v["flow"], serde_json::json!([true, false, "NO"]));
        assert_eq!(v["alias"], false);
    }
    #[test]
    fn legacy_integer_and_scientific_resolution() {
        let v: serde_json::Value = serde_yaml::from_str(
            &normalize(
                "octal: 012\nsexagesimal: 1:20\nnew_octal: 0o12\nexponent: 1e3\nquoted: '012'\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"octal":10,"sexagesimal":80,"new_octal":"0o12","exponent":"1e3","quoted":"012"})
        );
    }
}
