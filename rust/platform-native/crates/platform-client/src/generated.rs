//! Serde request, response and stream models from the current native OAS 3.2 producer.
//! Regenerate with `ai-platform-openapi codegen --out sdk`; CI uses `--check`.
//! Networking remains owned by this crate's existing authenticated Client.
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../sdk/rust/generated_models.rs"
));
