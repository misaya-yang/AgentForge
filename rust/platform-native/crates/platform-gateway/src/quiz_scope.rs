//! Quiz plugin source-policy bridge; no grading or persistence implementation.
pub use ai_platform_plugin_quiz_gateway::quiz_scope::{NativeQuizScope, QuizAudience, QuizScope};
pub fn parse_source_scope(
    value: &serde_json::Value,
) -> Result<crate::session_history::SourceReferences, crate::artifact_shares_api::ArtifactSharesError>
{
    let source = ai_platform_plugin_quiz_gateway::quiz_scope::parse_source_scope(value)
        .map_err(crate::quiz_plugin_bridge::host_error)?;
    Ok(crate::quiz_plugin_bridge::host_sources(source))
}
