use ai_platform_gateway_foundation::generic_rest_adapter::{
    RestResponse, decode_http_response_bytes,
};
use serde_json::json;

#[test]
fn json_encoding_detection_is_independent_of_charset() {
    let text = "{\"value\":\"中文😀\"}";
    let expected = json!({"value": "中文😀"});
    for little in [false, true] {
        let words = text
            .encode_utf16()
            .flat_map(|c| {
                if little {
                    c.to_le_bytes()
                } else {
                    c.to_be_bytes()
                }
            })
            .collect::<Vec<_>>();
        let wide = text
            .chars()
            .flat_map(|c| {
                if little {
                    (c as u32).to_le_bytes()
                } else {
                    (c as u32).to_be_bytes()
                }
            })
            .collect::<Vec<_>>();
        for bytes in [words, wide] {
            for content_type in ["application/json; charset=ascii", "text/plain"] {
                assert_eq!(
                    decode_http_response_bytes(200, content_type, &bytes).unwrap(),
                    RestResponse::Json(expected.clone())
                );
            }
        }
    }
    assert_eq!(
        decode_http_response_bytes(200, "application/json", b"\xef\xbb\xbf{\"a\":1}").unwrap(),
        RestResponse::Json(json!({"a":1}))
    );
}

#[test]
fn text_decoding_preserves_httpx_raw_fallback() {
    for (content_type, bytes, expected) in [
        (
            "text/plain; charset=iso-8859-1",
            &b"caf\xe9\x80"[..],
            "café\u{80}",
        ),
        ("text/plain; charset=windows-1252", &b"\x80"[..], "€"),
        ("text/plain; charset=ASCII", &b"a\xff"[..], "a�"),
        ("text/plain; charset=unknown", &b"a\xff"[..], "a�"),
        ("text/plain", &b"a\xff"[..], "a�"),
        // .json() must parse original bytes; successful text decoding must not
        // turn invalid-UTF-8 JSON into a parsed object in the fallback branch.
        (
            "text/plain; charset=latin1",
            &b"{\"a\":\"\xe9\"}"[..],
            "{\"a\":\"é\"}",
        ),
    ] {
        assert_eq!(
            decode_http_response_bytes(200, content_type, bytes).unwrap(),
            RestResponse::Json(json!({"raw": expected}))
        );
    }
}

#[test]
fn invalid_json_is_not_a_successful_application_json_response() {
    assert!(decode_http_response_bytes(200, "application/json", b"\xff").is_err());
    assert!(decode_http_response_bytes(500, "text/plain", b"failure").is_err());
    assert!(decode_http_response_bytes(200, "application/json", &[0xff, 0xfe, 0x7b]).is_err());
}

#[test]
fn source_text_codec_goldens() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/generic_rest_text_codecs.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let charset = case["charset"].as_str().unwrap();
        let bytes: Vec<u8> = case["bytes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| u8::try_from(v.as_u64().unwrap()).unwrap())
            .collect();
        let actual =
            decode_http_response_bytes(200, &format!("text/plain; charset={charset}"), &bytes);
        if case.get("error").is_some() {
            assert!(actual.is_err(), "{case}");
        } else {
            assert_eq!(
                actual.unwrap(),
                RestResponse::Json(json!({"raw": case["expected"]})),
                "{case}"
            );
        }
    }
}

#[test]
fn embedded_single_byte_codecs_are_complete() {
    let tables: serde_json::Value =
        serde_json::from_str(include_str!("../src/generic_rest_codecs.json")).unwrap();
    for (codec, map) in tables["maps"].as_object().unwrap() {
        assert_eq!(map.as_str().unwrap().chars().count(), 256, "{codec}");
    }
    for canonical in tables["aliases"].as_object().unwrap().values() {
        assert!(tables["maps"].get(canonical.as_str().unwrap()).is_some());
    }
}
