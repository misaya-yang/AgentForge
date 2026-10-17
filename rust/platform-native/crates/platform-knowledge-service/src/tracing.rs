//! Knowledge adapter for the shared source-equivalent distributed tracing stack.
pub use ai_platform_service_primitives::distributed_tracing::{
    SdkTracerProvider, TraceCarrier, current_carrier,
};
use axum::{extract::Request, middleware::Next, response::Response};
pub fn initialize() -> SdkTracerProvider {
    ai_platform_service_primitives::distributed_tracing::initialize("knowledge-service")
}
pub async fn inbound(mut request: Request, next: Next) -> Response {
    if let Some(context) = request
        .extensions()
        .get::<crate::host::middleware::RequestContext>()
    {
        let request_id = context.request_id.clone();
        request
            .extensions_mut()
            .insert(ai_platform_service_primitives::distributed_tracing::RequestId(
                request_id,
            ));
    }
    ai_platform_service_primitives::distributed_tracing::inbound(request, next).await
}
