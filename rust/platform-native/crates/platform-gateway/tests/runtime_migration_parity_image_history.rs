//! All13 source test_image_signature_flow.py cases; no provider/storage I/O.
use ai_platform_gateway::migration_content::image_history::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
fn image(sig: Option<&str>) -> Value {
    let mut v = json!({"content_base64":"fake_b64_data","mime_type":"image/png"});
    if let Some(sig) = sig {
        v["thought_signature"] = json!(sig)
    }
    v
}
fn part(content: &Value, key: &str) -> Value {
    content["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p.get(key).is_some())
        .unwrap()
        .clone()
}
#[test]
fn signature_and_artifact_id_persisted_on_model_turn() {
    let mut h = vec![];
    append_image_turns(
        &mut h,
        "draw a cat",
        Some(&image(Some("sig_abc_123"))),
        Some("here you go"),
        Some("art_001"),
    );
    assert_eq!(h.len(), 2);
    assert_eq!(h[0], json!({"role":"user","text":"draw a cat"}));
    assert_eq!(
        h[1],
        json!({"role":"model","artifact_id":"art_001","thought_signature":"sig_abc_123","mime_type":"image/png","text":"here you go"})
    );
}
#[test]
fn no_image_bytes_ever_persisted_in_history() {
    let mut h = vec![];
    let mut i = image(Some("sig_abc"));
    i["content_base64"] = json!("X".repeat(1_500_000));
    append_image_turns(
        &mut h,
        "draw a fish",
        Some(&i),
        Some("result text"),
        Some("art_002"),
    );
    assert!(h[1].get("image_base64").is_none());
    assert!(h[1].get("file_uri").is_none());
    assert_eq!(h[1]["artifact_id"], "art_002");
    assert!(serde_json::to_vec(&h).unwrap().len() < 1000);
}
#[test]
fn no_artifact_id_means_text_only_model_turn() {
    let mut h = vec![];
    append_image_turns(
        &mut h,
        "draw a dog",
        Some(&image(Some("sig_xyz"))),
        Some("got it"),
        None,
    );
    for key in ["artifact_id", "image_base64", "file_uri"] {
        assert!(h[1].get(key).is_none());
    }
    assert_eq!(h[1]["thought_signature"], "sig_xyz");
    assert_eq!(h[1]["text"], "got it");
}
#[test]
fn no_signature_when_absent_in_result() {
    let mut h = vec![];
    append_image_turns(
        &mut h,
        "draw a fish",
        Some(&image(None)),
        None,
        Some("art_003"),
    );
    assert!(h[1].get("thought_signature").is_none());
}
#[tokio::test]
async fn inflate_attaches_bytes_to_model_turns_with_artifact_id() {
    let h = vec![
        json!({"role":"user","text":"draw a cat"}),
        json!({"role":"model","mime_type":"image/png","artifact_id":"art_001","thought_signature":"sig_first"}),
    ];
    let out = inflate_history_with_bytes(&h, |id| async move {
        assert_eq!(id, "art_001");
        Ok::<_, ()>(Some(b"binary_image_bytes_001".to_vec()))
    })
    .await;
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], h[0]);
    assert_eq!(out[1]["artifact_id"], "art_001");
    assert_eq!(
        out[1]["image_base64"],
        STANDARD.encode(b"binary_image_bytes_001")
    );
    assert_eq!(out[1]["thought_signature"], "sig_first");
}
#[tokio::test]
async fn inflate_does_not_mutate_input_history() {
    let h = vec![
        json!({"role":"user","text":"draw"}),
        json!({"role":"model","artifact_id":"art_001"}),
    ];
    let before = h.clone();
    inflate_history_with_bytes(&h, |_| async { Ok::<_, ()>(Some(b"big_bytes".to_vec())) }).await;
    assert_eq!(h, before);
    assert!(h[1].get("image_base64").is_none());
}
#[tokio::test]
async fn inflate_skips_turn_when_download_returns_none() {
    let h: Vec<_> = ["art_001", "art_missing", "art_002"]
        .into_iter()
        .map(|id| json!({"role":"model","artifact_id":id}))
        .collect();
    let out = inflate_history_with_bytes(&h, |id| async move {
        Ok::<_, ()>(if id == "art_missing" {
            None
        } else {
            Some(format!("bytes_for_{id}").into_bytes())
        })
    })
    .await;
    assert!(out[0].get("image_base64").is_some());
    assert!(out[1].get("image_base64").is_none());
    assert!(out[2].get("image_base64").is_some());
}
#[tokio::test]
async fn inflate_runs_downloads_in_parallel() {
    let h: Vec<_> = (0..5)
        .map(|i| json!({"role":"model","artifact_id":format!("art_{i}")}))
        .collect();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(5));
    let out = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        inflate_history_with_bytes(&h, |id| {
            let barrier = barrier.clone();
            async move {
                barrier.wait().await;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Ok::<_, ()>(Some(format!("bytes_for_{id}").into_bytes()))
            }
        }),
    )
    .await
    .expect("five parallel downloads must finish under source200ms bound");
    assert!(out.iter().all(|turn| turn.get("image_base64").is_some()));
}
#[test]
fn signature_replayed_on_inline_data_when_history_has_image_base64() {
    let h = vec![
        json!({"role":"user","text":"draw a cat"}),
        json!({"role":"model","mime_type":"image/png","image_base64":"Zm9v","thought_signature":"sig_abc_123"}),
    ];
    let out = build_gemini_contents_from_history(&h, "make it orange");
    assert_eq!(out.len(), 3);
    let p = part(&out[1], "inlineData");
    assert_eq!(p["thoughtSignature"], "sig_abc_123");
    assert_eq!(
        p["inlineData"],
        json!({"data":"Zm9v","mimeType":"image/png"})
    );
}
#[test]
fn legacy_file_uri_replay_still_works_for_back_compat() {
    let out = build_gemini_contents_from_history(
        &[
            json!({"role":"model","mime_type":"image/png","file_uri":"files/legacy_abc","thought_signature":"sig_legacy"}),
        ],
        "edit it",
    );
    let p = part(&out[0], "fileData");
    assert_eq!(p["fileData"]["fileUri"], "files/legacy_abc");
    assert_eq!(p["thoughtSignature"], "sig_legacy");
}
#[test]
fn image_base64_takes_precedence_over_file_uri_when_both_present() {
    let out = build_gemini_contents_from_history(
        &[
            json!({"role":"model","mime_type":"image/png","image_base64":"Zm9v","file_uri":"files/stale","thought_signature":"sig"}),
        ],
        "edit",
    );
    let parts = out[0]["parts"].as_array().unwrap();
    assert!(parts.iter().any(|p| p.get("inlineData").is_some()));
    assert!(!parts.iter().any(|p| p.get("fileData").is_some()));
}
#[test]
fn text_only_turn_replays_without_image_part() {
    let out = build_gemini_contents_from_history(
        &[
            json!({"role":"model","mime_type":"image/png","thought_signature":"sig_no_anchor","text":"ok"}),
        ],
        "next",
    );
    assert_eq!(out[0]["parts"], json!([{"text":"ok"}]));
}
#[tokio::test]
async fn full_artifact_backed_two_turn_roundtrip() {
    let mut h = vec![];
    append_image_turns(
        &mut h,
        "draw sunset",
        Some(&image(Some("sig_turn1"))),
        Some("done"),
        Some("art_t1"),
    );
    assert!(serde_json::to_vec(&h).unwrap().len() < 1000);
    let download = |id: String| async move {
        Ok::<_, ()>(match id.as_str() {
            "art_t1" => Some(b"image_t1_bytes".to_vec()),
            "art_t2" => Some(b"image_t2_bytes".to_vec()),
            _ => None,
        })
    };
    let inflated = inflate_history_with_bytes(&h, download).await;
    let contents = build_gemini_contents_from_history(&inflated, "add birds");
    assert_eq!(contents.len(), 3);
    let p = part(&contents[1], "inlineData");
    assert_eq!(p["inlineData"]["data"], STANDARD.encode(b"image_t1_bytes"));
    assert_eq!(p["thoughtSignature"], "sig_turn1");
    append_image_turns(
        &mut h,
        "add birds",
        Some(&image(Some("sig_turn2"))),
        None,
        Some("art_t2"),
    );
    let inflated = inflate_history_with_bytes(&h, download).await;
    let contents = build_gemini_contents_from_history(&inflated, "make sky pink");
    let model: Vec<_> = contents.iter().filter(|c| c["role"] == "model").collect();
    assert_eq!(model.len(), 2);
    let signatures: Vec<_> = model
        .into_iter()
        .map(|c| part(c, "inlineData")["thoughtSignature"].clone())
        .collect();
    assert_eq!(signatures, vec![json!("sig_turn1"), json!("sig_turn2")]);
}
