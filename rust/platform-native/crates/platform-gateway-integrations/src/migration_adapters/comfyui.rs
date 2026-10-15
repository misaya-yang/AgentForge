use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};
pub struct ComfyUiAdapter {
    url: String,
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
            url: format!("{base}{path}"),
        })
    }
}
impl ProtocolAdapter for ComfyUiAdapter {
    fn invoke<'a>(&'a self, request: &'a UnifiedRequest) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async move {
            // The socket is owned by this future; cancellation closes the transport,
            // never leaving a detached websocket reader or background generation task.
            let (mut socket, _) = connect_async(&self.url)
                .await
                .map_err(|_| AdapterFailure::ConnectError)?;
            let result=async{
            socket.send(Message::Text(json!({"type":"execute","data":request.parameters.clone().unwrap_or_default()}).to_string().into())).await.map_err(|_|AdapterFailure::WriteError)?;
            while let Some(message)=socket.next().await{
                let message=message.map_err(|_|AdapterFailure::ReadError)?;
                let value:Value=match message{Message::Text(text)=>serde_json::from_str(&text),Message::Binary(bytes)=>serde_json::from_slice(&bytes),Message::Close(_)=>break,_=>continue}.map_err(|_|validation("comfyui invalid JSON message"))?;
                match value.get("type").and_then(Value::as_str){
                    Some("executed")=>return Ok(UnifiedResponse::new(&request.request_id,"success",vec![content(ContentType::Json,value)])),
                    Some("error")=>{let mut response=UnifiedResponse::new(&request.request_id,"error",vec![]);response.error=Some(Map::from_iter([("message".into(),value.get("error").cloned().unwrap_or(Value::Null))]));return Ok(response)},_=>{}
                }
            }
            let mut response=UnifiedResponse::new(&request.request_id,"error",vec![]);response.error=Some(Map::from_iter([("message".into(),json!("comfyui no result"))]));Ok(response)
        }.await;
            let _ = socket.close(None).await;
            result
        })
    }
    // BaseConnector.health_check in the canonical websocket transport is True.
    fn health_check(&self) -> AdapterFuture<'_, bool> {
        Box::pin(async { Ok(true) })
    }
}
