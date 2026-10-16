//! In-process builtin dispatch and instruction-only tenant artifact execution.
use super::{ContentFuture, skills::execute_instructions};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock, RwLock},
};
pub type BuiltinHandler = Arc<dyn Fn(Value, Value) -> ContentFuture<'static, Value> + Send + Sync>;
fn handlers() -> &'static RwLock<BTreeMap<String, BuiltinHandler>> {
    static HANDLERS: OnceLock<RwLock<BTreeMap<String, BuiltinHandler>>> = OnceLock::new();
    HANDLERS.get_or_init(|| RwLock::new(BTreeMap::new()))
}
pub fn register_builtin(name: impl Into<String>, handler: BuiltinHandler) {
    handlers()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(name.into(), handler);
}
#[derive(Default)]
pub struct SkillExecutor;
impl SkillExecutor {
    pub async fn execute(&self, skill: &Value, args: Value) -> Value {
        let entry = skill["entrypoint"].as_str().unwrap_or("");
        if let Some(name) = entry.strip_prefix("builtin://") {
            let handler = handlers()
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(name)
                .cloned();
            let Some(handler) = handler else {
                return json!({"success":false,"error":format!("Builtin skill handler '{name}' not found")});
            };
            match handler(args, skill.clone()).await {
                Ok(value) => value,
                Err(_) => {
                    tracing::error!(
                        event = "assistant.skill_builtin.internal_failure",
                        "Builtin skill execution failed"
                    );
                    json!({"success":false,"error":format!("Skill execution failed: {name}")})
                }
            }
        } else {
            execute_instructions(skill)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn builtin_registry_dispatches_only_registered_handlers() {
        let name = format!("test-{}", uuid::Uuid::new_v4());
        let skill = json!({"entrypoint":format!("builtin://{name}")});
        assert_eq!(
            SkillExecutor.execute(&skill, json!({})).await["success"],
            false
        );
        register_builtin(
            name.clone(),
            Arc::new(|args, _| Box::pin(async move { Ok(json!({"success":true,"result":args})) })),
        );
        assert_eq!(
            SkillExecutor.execute(&skill, json!({"x":1})).await["result"]["x"],
            1
        );
        register_builtin(
            name,
            Arc::new(|_, _| {
                Box::pin(
                    async move { Err(super::super::ContentError::new(500, "private failure")) },
                )
            }),
        );
        assert!(
            !SkillExecutor
                .execute(&skill, json!({}))
                .await
                .to_string()
                .contains("private failure")
        );
    }
}
