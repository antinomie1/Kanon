//! JSON-RPC request correlation, notifications and tool namespacing.

use super::*;

impl McpServer {
    /// Sends one JSON-RPC request and waits for its response.
    pub(super) async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        let mut guard = self.connection.lock().await;
        self.ensure_available().await?;
        // Stdio has one byte stream for every request. On cancellation, dropping this local
        // owner also closes any half-written request or half-consumed response; never hand those
        // bytes to the next caller. HTTP request cancellation leaves other responses intact.
        let mut owned = if matches!(guard.as_ref(), Some(Connection::Stdio { .. })) {
            guard.take()
        } else {
            None
        };
        let connection = owned.as_mut().or(guard.as_mut()).ok_or_else(|| {
            McpError::Transport(format!("MCP server '{}' is not connected", self.config.id))
        })?;
        let result = self.request_on(connection, method, params).await;
        if let Err(McpError::Transport(error)) = &result {
            // Framing, I/O and expired-session failures make the connection unusable. RPC/tool
            // errors are different: the server answered correctly and may handle the next call.
            *guard = None;
            self.clear_metadata();
            let mut health = self.health.lock().await;
            health.failures += 1;
            health.state = "reconnecting".to_string();
            health.tools = 0;
            health.last_error = Some(error.clone());
        } else if let Some(connection) = owned {
            // A complete result or RPC error consumed its entire frame. Restoring ownership has
            // no await, so cancellation cannot interrupt the commit after transport validation.
            *guard = Some(connection);
        }
        result
    }

    /// Uses a transport already exclusively owned by an initialization or live request.
    pub(super) async fn request_on(
        &self,
        connection: &mut Connection,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        let id = self
            .next_request_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let payload =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        // One deadline includes writes, server-initiated pings, and every streamed event.
        tokio::time::timeout(MCP_REQUEST_TIMEOUT, async {
            match connection {
                Connection::Stdio { stdout, stdin, .. } => {
                    write_stdio(stdin, &payload).await?;
                    let mut buffer = Vec::new();
                    loop {
                        buffer.clear();
                        // take() bounds allocation even if a broken server never writes a newline.
                        let read = (&mut *stdout)
                            .take((MCP_MAX_MESSAGE_BYTES + 1) as u64)
                            .read_until(b'\n', &mut buffer)
                            .await
                            .map_err(|error| {
                                McpError::Transport(format!("failed to read response: {error}"))
                            })?;
                        if read == 0 {
                            return Err(McpError::Transport(format!(
                                "MCP server '{}' closed its output while answering '{method}'",
                                self.config.id
                            )));
                        }
                        if read > MCP_MAX_MESSAGE_BYTES {
                            return Err(McpError::Transport(
                                "MCP response exceeds the message size limit".into(),
                            ));
                        }
                        let message = serde_json::from_slice(&buffer).map_err(|error| {
                            McpError::Transport(format!("invalid MCP JSON: {error}"))
                        })?;
                        match self.incoming(message, id, method)? {
                            IncomingMessage::Result(result) => return Ok(result),
                            IncomingMessage::Reply(reply) => write_stdio(stdin, &reply).await?,
                            IncomingMessage::Ignore => {}
                        }
                    }
                }
                Connection::Http(http) => {
                    let mut response = http.send(&payload).await?;
                    let content_type = response
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.split(';').next())
                        .unwrap_or("")
                        .trim();
                    let is_sse = content_type.eq_ignore_ascii_case("text/event-stream");
                    if !is_sse && !content_type.eq_ignore_ascii_case("application/json") {
                        return Err(McpError::Transport(format!(
                            "unsupported MCP response Content-Type '{content_type}'"
                        )));
                    }
                    let mut decoder = kanon_llm::SseDecoder::new();
                    let mut body = Vec::new();
                    let mut received = 0;
                    while let Some(chunk) = response.chunk().await.map_err(|error| {
                        McpError::Transport(format!("failed to read HTTP response: {error}"))
                    })? {
                        received += chunk.len();
                        if received > MCP_MAX_MESSAGE_BYTES {
                            return Err(McpError::Transport(
                                "MCP response exceeds the message size limit".into(),
                            ));
                        }
                        if !is_sse {
                            body.extend_from_slice(&chunk);
                            continue;
                        }
                        for event in decoder.decode(&chunk) {
                            let message = serde_json::from_str(&event.data).map_err(|error| {
                                McpError::Transport(format!("invalid MCP SSE JSON: {error}"))
                            })?;
                            match self.incoming(message, id, method)? {
                                IncomingMessage::Result(result) => return Ok(result),
                                IncomingMessage::Reply(reply) => http.send_one_way(&reply).await?,
                                IncomingMessage::Ignore => {}
                            }
                        }
                    }
                    if !is_sse {
                        let message = serde_json::from_slice(&body).map_err(|error| {
                            McpError::Transport(format!("invalid MCP JSON: {error}"))
                        })?;
                        if let IncomingMessage::Result(result) =
                            self.incoming(message, id, method)?
                        {
                            return Ok(result);
                        }
                    }
                    Err(McpError::Transport(
                        "MCP HTTP response ended without the matching JSON-RPC result".into(),
                    ))
                }
            }
        })
        .await
        .map_err(|_| {
            McpError::Transport(format!(
                "MCP server '{}' did not answer '{method}' in time",
                self.config.id
            ))
        })?
    }

    /// Validates envelopes before interpreting payloads; server and client request IDs are separate.
    pub(super) fn incoming(
        &self,
        mut message: serde_json::Value,
        id: u64,
        method: &str,
    ) -> Result<IncomingMessage, McpError> {
        let invalid = || McpError::Transport("invalid MCP JSON-RPC envelope".into());
        if message.get("jsonrpc").and_then(|value| value.as_str()) != Some("2.0") {
            return Err(invalid());
        }
        if let Some(server_method) = message.get("method") {
            let server_method = server_method.as_str().ok_or_else(invalid)?;
            if message.get("result").is_some() || message.get("error").is_some() {
                return Err(invalid());
            }
            let Some(server_id) = message.get("id") else {
                return Ok(IncomingMessage::Ignore);
            };
            if !server_id.is_string() && !server_id.is_i64() && !server_id.is_u64() {
                return Err(invalid());
            }
            // No sampling/elicitation capability is advertised. Respond explicitly instead of
            // leaving a server blocked waiting on a request this client cannot implement.
            let reply = if server_method == "ping" {
                serde_json::json!({"jsonrpc":"2.0", "id":server_id, "result":{}})
            } else {
                serde_json::json!({"jsonrpc":"2.0", "id":server_id, "error":{"code":-32601, "message":"Method not supported"}})
            };
            return Ok(IncomingMessage::Reply(reply));
        }
        let result = message.get("result");
        let error = message.get("error");
        if result.is_some() == error.is_some()
            || result.is_some_and(|value| !value.is_object())
            || error.is_some_and(|error| {
                error.get("code").and_then(|value| value.as_i64()).is_none()
                    || error
                        .get("message")
                        .and_then(|value| value.as_str())
                        .is_none()
            })
        {
            return Err(invalid());
        }
        let response_id = message.get("id").ok_or_else(invalid)?;
        if !response_id.is_string() && !response_id.is_i64() && !response_id.is_u64() {
            return Err(invalid());
        }
        if response_id.as_u64() != Some(id) {
            // A canceled stdio request may leave a late response. Never use it for another call.
            return Ok(IncomingMessage::Ignore);
        }
        if let Some(error) = error {
            return Err(McpError::Rpc {
                server: self.config.id.clone(),
                method: method.to_string(),
                message: error["message"]
                    .as_str()
                    .expect("validated error message")
                    .to_string(),
            });
        }
        // The envelope is owned here; move its potentially large result without a second copy.
        Ok(IncomingMessage::Result(message["result"].take()))
    }

    /// Sends one JSON-RPC notification (no response expected).
    pub(super) async fn notify(
        &self,
        connection: &mut Connection,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), McpError> {
        let payload = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        tokio::time::timeout(MCP_REQUEST_TIMEOUT, async {
            match connection {
                Connection::Stdio { stdin, .. } => write_stdio(stdin, &payload).await,
                Connection::Http(http) => http.send_one_way(&payload).await,
            }
        })
        .await
        .map_err(|_| {
            McpError::Transport(format!(
                "MCP server '{}' did not accept notification '{method}' in time",
                self.config.id
            ))
        })?
    }
}

/// Host identifier used for MCP servers in tool metadata.
pub fn host_id(server_id: &str) -> String {
    format!("mcp_{server_id}")
}

/// Qualified tool name exposed to the model.
pub fn qualified_tool_name(server_id: &str, tool: &str) -> String {
    format!("mcp__{server_id}__{tool}")
}

/// Recovers the MCP tool name from its qualified form.
pub fn unqualified_tool_name(server_id: &str, qualified: &str) -> Option<String> {
    qualified
        .strip_prefix(&format!("mcp__{server_id}__"))
        .map(str::to_string)
}
