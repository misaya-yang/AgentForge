//! ai-quiz compiled plugin: deterministic scoring and governed generation persistence.
//! The executor feature has no Gateway/Foundation dependency or Agent loop.
pub const CAPABILITY_ID: &str = "generate_quiz";
pub const PLUGIN_ID: &str = "ai-quiz";
pub mod quiz_grading;
pub use ai_platform_python_compat::unicode as quiz_unicode;
pub fn capability_record() -> serde_json::Value {
    serde_json::from_str(include_str!("../capability.json")).expect("reviewed quiz descriptor")
}
#[cfg(feature = "executor")]
pub mod executor;
