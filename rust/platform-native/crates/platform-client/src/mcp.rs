//! Explicitly configured MCP transports and namespaced tool management.
use crate::Error;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[derive(Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub server_name: String,
}
pub enum TransportConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        api_key: Option<String>,
    },
}
enum Transport {
    Stdio {
        child: tokio::process::Child,
        input: tokio::process::ChildStdin,
        output: BufReader<tokio::process::ChildStdout>,
    },
    Http {
        client: reqwest::Client,
        url: url::Url,
        api_key: Option<String>,
        session: Option<String>,
    },
}
pub struct McpClient {
    pub name: String,
    pub tools: Vec<Tool>,
    transport: Transport,
    next_id: u64,
}
impl McpClient {
    pub async fn connect(name: String, config: TransportConfig) -> Result<Self, Error> {
        if name.is_empty() || name.contains("__") {
            return Err(Error::Request);
        }
        let transport = match config {
            TransportConfig::Stdio { command, args, env } => {
                let mut child = tokio::process::Command::new(command)
                    .args(args)
                    .envs(env)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|_| Error::Transport)?;
                let input = child.stdin.take().ok_or(Error::Transport)?;
                let output = BufReader::new(child.stdout.take().ok_or(Error::Transport)?);
                Transport::Stdio {
                    child,
                    input,
                    output,
                }
            }
            TransportConfig::Http { url, api_key } => {
                let url = url::Url::parse(&url).map_err(|_| Error::Origin)?;
                let local = url
                    .host_str()
                    .and_then(|s| s.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
                    .is_some_and(|ip| ip.is_loopback());
                if !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                    || url.query().is_some()
                    || !(url.scheme() == "https" || url.scheme() == "http" && local)
                {
                    return Err(Error::Origin);
                }
                let client = reqwest::Client::builder()
                    .retry(reqwest::retry::never())
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(30))
                    .build()
                    .map_err(|_| Error::Transport)?;
                Transport::Http {
                    client,
                    url,
                    api_key,
                    session: None,
                }
            }
        };
        let mut client = Self {
            name,
            tools: Vec::new(),
            transport,
            next_id: 0,
        };
        let init=client.request("initialize",json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"clientInfo":{"name":"ai-gateway-sdk","version":"1.0.0"}})).await?;
        if init["protocolVersion"] != "2025-11-25" {
            return Err(Error::Contract("MCP protocol version mismatch"));
        }
        client.notify("notifications/initialized").await?;
        let listing = client.request("tools/list", json!({})).await?;
        let tools = listing["tools"].as_array().ok_or(Error::Response)?;
        client.tools = tools
            .iter()
            .map(|v| {
                Ok(Tool {
                    name: v["name"].as_str().ok_or(Error::Response)?.into(),
                    description: v["description"].as_str().unwrap_or_default().into(),
                    input_schema: v.get("inputSchema").cloned().unwrap_or(json!({})),
                    server_name: client.name.clone(),
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(client)
    }
    async fn exchange(&mut self, payload: Value, notification: bool) -> Result<Value, Error> {
        let expected = payload.get("id").and_then(Value::as_u64);
        match &mut self.transport {
            Transport::Stdio { input, output, .. } => {
                let mut encoded = serde_json::to_vec(&payload).map_err(|_| Error::Request)?;
                encoded.push(b'\n');
                if encoded.len() > 1024 * 1024 {
                    return Err(Error::Request);
                }
                input
                    .write_all(&encoded)
                    .await
                    .map_err(|_| Error::Transport)?;
                input.flush().await.map_err(|_| Error::Transport)?;
                if notification {
                    return Ok(Value::Null);
                }
                tokio::time::timeout(Duration::from_secs(30), async {
                    loop {
                        let mut line = Vec::new();
                        loop {
                            let available =
                                output.fill_buf().await.map_err(|_| Error::Transport)?;
                            if available.is_empty() {
                                return Err(Error::Transport);
                            }
                            let n = available
                                .iter()
                                .position(|b| *b == b'\n')
                                .map_or(available.len(), |i| i + 1);
                            if line.len() + n > 1024 * 1024 {
                                return Err(Error::Response);
                            }
                            let finished = available[n - 1] == b'\n';
                            line.extend_from_slice(&available[..n]);
                            output.consume(n);
                            if finished {
                                break;
                            }
                        }
                        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
                            continue;
                        };
                        if value["id"].as_u64() == expected {
                            return Ok(value);
                        }
                    }
                })
                .await
                .map_err(|_| Error::Transport)?
            }
            Transport::Http {
                client,
                url,
                api_key,
                session,
            } => {
                let mut request = client
                    .post(url.clone())
                    .header("Accept", "application/json, text/event-stream")
                    .header("MCP-Protocol-Version", "2025-11-25")
                    .json(&payload);
                if let Some(key) = api_key {
                    request = request.bearer_auth(key.as_str())
                }
                if let Some(id) = session.as_ref() {
                    request = request.header("Mcp-Session-Id", id)
                }
                let mut response = request.send().await.map_err(|_| Error::Transport)?;
                if !response.status().is_success() {
                    return Err(Error::Status(response.status().as_u16()));
                }
                if let Some(id) = response
                    .headers()
                    .get("Mcp-Session-Id")
                    .and_then(|v| v.to_str().ok())
                {
                    if id.len() > 512 {
                        return Err(Error::Response);
                    }
                    *session = Some(id.into());
                }
                if notification {
                    return Ok(Value::Null);
                }
                let is_sse = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.starts_with("text/event-stream"));
                let mut bytes = Vec::new();
                let mut decoder = crate::sse::Decoder::default();
                while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
                    if is_sse {
                        for frame in decoder.push(&chunk)? {
                            let v: Value =
                                serde_json::from_str(&frame).map_err(|_| Error::Response)?;
                            if v["id"].as_u64() == expected {
                                return Ok(v);
                            }
                        }
                    } else {
                        if bytes.len() + chunk.len() > 1024 * 1024 {
                            return Err(Error::Response);
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                }
                if is_sse {
                    for frame in decoder.finish()? {
                        let v: Value = serde_json::from_str(&frame).map_err(|_| Error::Response)?;
                        if v["id"].as_u64() == expected {
                            return Ok(v);
                        }
                    }
                    return Err(Error::Response);
                }
                serde_json::from_slice(&bytes).map_err(|_| Error::Response)
            }
        }
    }
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, Error> {
        self.next_id = self.next_id.checked_add(1).ok_or(Error::Request)?;
        let id = self.next_id;
        let value = self
            .exchange(
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
                false,
            )
            .await?;
        if value["jsonrpc"] != "2.0"
            || value["id"].as_u64() != Some(id)
            || value.get("error").is_some()
        {
            return Err(Error::Response);
        }
        value.get("result").cloned().ok_or(Error::Response)
    }
    async fn notify(&mut self, method: &str) -> Result<(), Error> {
        self.exchange(json!({"jsonrpc":"2.0","method":method,"params":{}}), true)
            .await
            .map(|_| ())
    }
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, Error> {
        if !arguments.is_object() || !self.tools.iter().any(|t| t.name == name) {
            return Err(Error::Request);
        }
        self.request("tools/call", json!({"name":name,"arguments":arguments}))
            .await
    }
    pub async fn close(&mut self) -> Result<(), Error> {
        if let Transport::Stdio { child, .. } = &mut self.transport {
            if child.try_wait().map_err(|_| Error::Transport)?.is_some() {
                return Ok(());
            }
            child.kill().await.map_err(|_| Error::Transport)?;
            child.wait().await.map_err(|_| Error::Transport)?;
        }
        Ok(())
    }
}
#[derive(Default)]
pub struct McpManager {
    clients: BTreeMap<String, McpClient>,
}
impl McpManager {
    pub async fn connect(
        &mut self,
        name: String,
        config: TransportConfig,
    ) -> Result<Vec<Tool>, Error> {
        if let Some(mut old) = self.clients.remove(&name) {
            old.close().await?;
        }
        let client = McpClient::connect(name.clone(), config).await?;
        let tools = client.tools.clone();
        self.clients.insert(name, client);
        Ok(tools)
    }
    pub fn list_tools(&self) -> Vec<Value> {
        self.clients.iter().flat_map(|(name,c)|c.tools.iter().map(move|t|json!({"name":format!("mcp_{name}__{}",t.name),"description":t.description,"server":name,"input_schema":t.input_schema}))).collect()
    }
    pub async fn call_tool(&mut self, qualified: &str, args: Value) -> Result<Value, Error> {
        let (server, name) = qualified
            .strip_prefix("mcp_")
            .and_then(|s| s.split_once("__"))
            .ok_or(Error::Request)?;
        self.clients
            .get_mut(server)
            .ok_or(Error::Request)?
            .call_tool(name, args)
            .await
    }
    pub async fn disconnect(&mut self, name: Option<&str>) -> Result<(), Error> {
        if let Some(name) = name {
            if let Some(mut c) = self.clients.remove(name) {
                c.close().await?;
            }
        } else {
            for (_, mut c) in std::mem::take(&mut self.clients) {
                c.close().await?;
            }
        }
        Ok(())
    }
}
