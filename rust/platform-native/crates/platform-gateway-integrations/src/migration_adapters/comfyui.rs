use super::*;
use ai_platform_gateway_foundation::websocket_transport::{WebSocketConnector, WebSocketError};
pub struct ComfyUiAdapter {
    connector: WebSocketConnector,
}
impl ComfyUiAdapter {
    pub fn new(service: ServiceHandle) -> Result<Self, RegistryError> {
        if service.snapshot().connector_type != TransportType::Websocket {
            return Err(RegistryError::AdapterConstructionFailed);
        }
        let config = service.connector().snapshot();
        let base = config
            .get("base_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim_end_matches('/');
        let path = config
            .get("ws_path")
            .and_then(Value::as_str)
            .unwrap_or("/ws");
        Ok(Self {
            connector: WebSocketConnector::new(base, path),
        })
    }
}
impl ProtocolAdapter for ComfyUiAdapter {
    fn invoke<'a>(&'a self, request: &'a UnifiedRequest) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async move {
            // The socket is owned by this future; cancellation closes the transport,
            // never leaving a detached websocket reader or background generation task.
            let mut socket = self
                .connector
                .websocket()
                .await
                .map_err(|_| AdapterFailure::ConnectError)?;
            let result=async{
            socket.send_json(&json!({"type":"execute","data":request.parameters.clone().unwrap_or_default()})).await.map_err(|_|AdapterFailure::WriteError)?;
            while let Some(value) = socket.recv_json().await.map_err(|error| match error {
                WebSocketError::InvalidJson => validation("comfyui invalid JSON message"),
                _ => AdapterFailure::ReadError.into(),
            })? {
                match value.get("type").and_then(Value::as_str){
                    Some("executed")=>return Ok(UnifiedResponse::new(&request.request_id,"success",vec![content(ContentType::Json,value)])),
                    Some("error")=>{let mut response=UnifiedResponse::new(&request.request_id,"error",vec![]);response.error=Some(Map::from_iter([("message".into(),value.get("error").cloned().unwrap_or(Value::Null))]));return Ok(response)},_=>{}
                }
            }
            let mut response=UnifiedResponse::new(&request.request_id,"error",vec![]);response.error=Some(Map::from_iter([("message".into(),json!("comfyui no result"))]));Ok(response)
        }.await;
            let _ = socket.close().await;
            result
        })
    }
    // BaseConnector.health_check in the canonical websocket transport is True.
    fn health_check(&self) -> AdapterFuture<'_, bool> {
        Box::pin(async { Ok(true) })
    }
}
