//! Observe real model stream completion; never inspect content-bearing events.
use crate::telemetry::{Operation, OperationKind};
use codex_extension_api::{
    ModelRequestContributor, ModelRequestInput, ModelRequestKind, ModelResponseInterceptor,
    ModelResponseStream, ResponseEvent,
};
#[derive(Debug)]
pub(crate) struct ModelTelemetry;
struct ResponseTelemetry(Operation);
impl ModelRequestContributor for ModelTelemetry {
    fn request(&self, input: ModelRequestInput<'_>) -> Option<Box<dyn ModelResponseInterceptor>> {
        if input.kind != ModelRequestKind::Generation {
            return None;
        }
        let metadata = input.existing_metadata.as_ref();
        let parent = metadata
            .and_then(|m| m.get("traceparent"))
            .map(String::as_str);
        let state = metadata
            .and_then(|m| m.get("tracestate"))
            .map(String::as_str);
        let mut operation =
            Operation::start(parent, state, OperationKind::Chat).with_model(input.model);
        if let Some(run) = metadata.and_then(|m| m.get("x-ai-run-id")) {
            if let Ok(turns) = turns().lock() {
                if let Some(turn) = turns.get(run) {
                    operation.inherit_tenant(turn);
                }
            }
        }
        Some(Box::new(ResponseTelemetry(operation)))
    }
}
impl ModelResponseInterceptor for ResponseTelemetry {
    fn intercept(self: Box<Self>, mut stream: ModelResponseStream) -> ModelResponseStream {
        Box::pin(async_stream::stream! {
            let mut operation=Some(self.0);
            while let Some(event)=std::future::poll_fn(|cx|stream.as_mut().poll_next(cx)).await {
                if matches!(&event, Ok(ResponseEvent::OutputTextDelta(_))) {
                    if let Some(operation) = operation.as_mut() { operation.first_chunk(); }
                }
                if let Ok(ResponseEvent::Completed{token_usage,..})=&event {
                    if let Some(mut operation)=operation.take(){
                        if let Some(usage)=token_usage { operation.tokens(usage.input_tokens.max(0)as u64,usage.output_tokens.max(0)as u64); }
                        operation.success();
                    }
                }
                if event.is_err(){drop(operation.take());}
                yield event;
            }
        })
    }
}

use codex_extension_api::{
    ExtensionFuture, TurnAbortInput, TurnErrorInput, TurnLifecycleContributor, TurnStopInput,
};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};
static TURNS: OnceLock<Mutex<HashMap<String, Operation>>> = OnceLock::new();
fn turns() -> &'static Mutex<HashMap<String, Operation>> {
    TURNS.get_or_init(|| Mutex::new(HashMap::new()))
}
/// Bounded process-local observation only. Durable run state remains authoritative.
pub(crate) struct TurnAdmission {
    key: Option<String>,
}
impl TurnAdmission {
    pub(crate) fn admitted(&mut self) {
        self.key = None;
    }
}
impl Drop for TurnAdmission {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            finish_turn(&key, false);
        }
    }
}
pub(crate) fn start_turn(metadata: &mut HashMap<String, String>, key: &str) -> TurnAdmission {
    start_turn_scoped(metadata, key, None)
}
/// Tenant comes from authenticated host state and is never inserted in model metadata.
pub(crate) fn start_turn_for_tenant(
    metadata: &mut HashMap<String, String>,
    key: &str,
    tenant: &str,
) -> TurnAdmission {
    start_turn_scoped(metadata, key, Some(tenant))
}
fn start_turn_scoped(
    metadata: &mut HashMap<String, String>,
    key: &str,
    tenant: Option<&str>,
) -> TurnAdmission {
    let mut admission = TurnAdmission { key: None };
    let Ok(mut turns) = turns().lock() else {
        return admission;
    };
    if turns.len() >= 1024 || turns.contains_key(key) {
        return admission;
    }
    let mut operation = Operation::start(
        metadata.get("traceparent").map(String::as_str),
        metadata.get("tracestate").map(String::as_str),
        OperationKind::InvokeAgent,
    );
    if let Some(tenant) = tenant {
        operation = operation.with_tenant(tenant);
    }
    if let Some(parent) = operation.traceparent() {
        metadata.insert("traceparent".into(), parent);
    }
    turns.insert(key.into(), operation);
    admission.key = Some(key.into());
    admission
}
fn finish_turn(key: &str, success: bool) {
    if let Ok(mut turns) = turns().lock() {
        if let Some(mut operation) = turns.remove(key) {
            if success {
                operation.success();
            }
        }
    }
}
impl TurnLifecycleContributor for ModelTelemetry {
    fn on_turn_stop<'a>(&'a self, input: TurnStopInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            finish_turn(input.turn_store.level_id(), true);
        })
    }
    fn on_turn_error<'a>(&'a self, input: TurnErrorInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            finish_turn(input.turn_store.level_id(), false);
        })
    }
    fn on_turn_abort<'a>(&'a self, input: TurnAbortInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            finish_turn(input.turn_store.level_id(), false);
        })
    }
}
pub(crate) fn drain() {
    if let Ok(mut turns) = turns().lock() {
        turns.clear();
    }
}
pub(crate) fn approval_start(run: &str, approval: uuid::Uuid) {
    if let Ok(mut turns) = turns().lock() {
        let key = format!("approval/{approval}");
        if turns.len() < 1024 && !turns.contains_key(&key) {
            let parent = turns.get(run).and_then(Operation::traceparent);
            turns.insert(
                key,
                Operation::start(parent.as_deref(), None, OperationKind::ApprovalWait),
            );
        }
    }
}
pub(crate) fn approval_finish(approval: uuid::Uuid) {
    finish_turn(&format!("approval/{approval}"), true);
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Dropped(Arc<AtomicUsize>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[test]
    fn tenant_observation_is_local_and_failed_admission_is_removed() {
        let mut metadata = HashMap::new();
        let key = "telemetry-test-private-run";
        let admission = start_turn_for_tenant(&mut metadata, key, "private-tenant-name");
        assert!(turns().lock().unwrap().contains_key(key));
        assert!(!format!("{metadata:?}").contains("private-tenant-name"));
        drop(admission);
        assert!(!turns().lock().unwrap().contains_key(key));
    }
    #[tokio::test]
    async fn interceptor_preserves_content_and_drops_upstream_on_cancel() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let guard = Dropped(dropped.clone());
        let upstream = Box::pin(
            async_stream::stream! {let _guard=guard;yield Ok(ResponseEvent::OutputTextDelta("private content stays only in original stream".into()));std::future::pending::<()>().await;},
        );
        let mut stream = Box::new(ResponseTelemetry(Operation::start(
            None,
            None,
            OperationKind::Chat,
        )))
        .intercept(upstream);
        let event = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx))
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(event,ResponseEvent::OutputTextDelta(s) if s=="private content stays only in original stream")
        );
        drop(stream);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn completed_event_and_eof_are_transparent() {
        let upstream = Box::pin(
            async_stream::stream! {yield Ok(ResponseEvent::Completed{response_id:"not-exported".into(),token_usage:None,usage_metadata:None,end_turn:Some(true)});},
        );
        let mut stream = Box::new(ResponseTelemetry(Operation::start(
            None,
            None,
            OperationKind::Chat,
        )))
        .intercept(upstream);
        assert!(matches!(
            std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await,
            Some(Ok(ResponseEvent::Completed { .. }))
        ));
        assert!(
            std::future::poll_fn(|cx| stream.as_mut().poll_next(cx))
                .await
                .is_none()
        );
    }
}
