use super::*;
impl McpSession {
    pub(super) async fn initialize(self: Arc<Self>) -> Result<McpInitializedClient, McpError> {
        if self.config.revision.modern() {
            let result = self.rpc("server/discover", json!({})).await?;
            self.config
                .revision
                .validate_discovery(&result)
                .map_err(broker_error)?;
        } else {
            let result=self.rpc("initialize",json!({"protocolVersion":MCP_PROTOCOL_VERSION,"capabilities":{"tools":{"listChanged":true}},"clientInfo":{"name":"ai-gateway","version":"1.0.0"}})).await?;
            if result.get("protocolVersion").and_then(Value::as_str)
                != Some(self.config.revision.as_str())
            {
                return Err(broker_error("MCP_PROTOCOL_VERSION_MISMATCH"));
            }
            validate_server_info(result.get("serverInfo"))?;
            self.send(
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                true,
            )
            .await?;
        }
        let version = self.config.revision.as_str().to_owned();
        Ok(McpInitializedClient {
            client: self,
            protocol_version: Some(version),
        })
    }
}

fn validate_server_info(value: Option<&Value>) -> Result<(), McpError> {
    let Some(value) = value.filter(|v| python_truthy(v)) else {
        return Ok(());
    };
    if value.is_object() {
        return Ok(());
    }
    if let Some(entries) = value.as_array() {
        for entry in entries {
            match entry {
                Value::Array(pair)
                    if pair.len() == 2 && !pair[0].is_array() && !pair[0].is_object() => {}
                Value::String(pair) if pair.chars().count() == 2 => {}
                _ => return Err(McpError::Unhandled),
            }
        }
        return Ok(());
    }
    Err(McpError::Unhandled)
}
