//! The frozen catalog was measured with C ordering. Locale-dependent ordering
//! of overloaded signatures must not turn identical catalogs into ACL drift.
use super::{Result, refused};
use serde_json::Value;

pub(super) fn canonicalize_routines(key: &str, rows: &mut [Value]) -> Result<()> {
    let fields: &[&str] = match key {
        "structural_lines/functions"
        | "acl_lines/function_owners"
        | "structural_catalog_detail_lines/aggregates" => &["schema", "name", "identity_args"],
        "acl_lines/function_acls" => &["schema", "name", "identity_args", "grantee"],
        _ => return Ok(()),
    };
    if rows
        .iter()
        .any(|row| fields.iter().any(|field| !row[*field].is_string()))
    {
        return Err(refused("routine fingerprint ordering key is malformed"));
    }
    rows.sort_by(|left, right| {
        fields
            .iter()
            .map(|field| left[*field].as_str().unwrap())
            .cmp(fields.iter().map(|field| right[*field].as_str().unwrap()))
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overloaded_signatures_follow_the_frozen_c_profile_in_every_routine_family() {
        for key in [
            "structural_lines/functions",
            "acl_lines/function_owners",
            "acl_lines/function_acls",
            "structural_catalog_detail_lines/aggregates",
        ] {
            let mut rows = vec![
                json!({"schema":"assistant", "name":"reserve", "identity_args":"p_call_id uuid", "grantee":"owner"}),
                json!({"schema":"assistant", "name":"reserve", "identity_args":"p_call uuid, p_owner uuid", "grantee":"owner"}),
            ];
            canonicalize_routines(key, &mut rows).unwrap();
            assert_eq!(rows[0]["identity_args"], "p_call uuid, p_owner uuid");
            let frozen = rows.clone();
            canonicalize_routines(key, &mut rows).unwrap();
            assert_eq!(rows, frozen);
        }
    }

    #[test]
    fn numeric_catalog_order_and_enum_position_are_preserved() {
        let original = vec![json!({"position":2}), json!({"position":10})];
        let mut rows = original.clone();
        canonicalize_routines("structural_lines/columns", &mut rows).unwrap();
        assert_eq!(rows, original);
        assert!(canonicalize_routines("acl_lines/function_owners", &mut rows).is_err());
    }
}
