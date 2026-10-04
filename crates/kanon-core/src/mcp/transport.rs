//! HTTP session headers and bounded stdio message writes.

use super::*;

impl HttpConnection {
    /// Sends one message without consuming its body. The caller owns the overall deadline.
    pub(super) async fn send(
        &mut self,
        payload: &serde_json::Value,
    ) -> Result<reqwest::Response, McpError> {
        let mut request = self.client.post(&self.url);
        for (key, value) in &self.headers {
            if ![
                "accept",
                "content-type",
                "mcp-session-id",
                "mcp-protocol-version",
            ]
            .iter()
            .any(|reserved| key.eq_ignore_ascii_case(reserved))
            {
                request = request.header(key, value);
            }
        }
        request = request
            .json(payload)
            .header("accept", "application/json, text/event-stream");
        if let Some(session_id) = &self.session_id {
            request = request.header("mcp-session-id", session_id);
        }
        if let Some(version) = &self.protocol_version {
            request = request.header("mcp-protocol-version", version);
        }
        let response = request
            .send()
            .await
            .map_err(|error| McpError::Transport(format!("HTTP request failed: {error}")))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND && self.session_id.is_some() {
            self.session_id = None;
            self.protocol_version = None;
            // The caller drops this connection. A subsequent operation initializes afresh;
            // never replay a possibly side-effecting tools/call behind the model's back.
            return Err(McpError::Transport(
                "MCP HTTP session expired; reconnect before retrying".into(),
            ));
        }
        if !response.status().is_success() {
            return Err(McpError::Transport(format!(
                "MCP HTTP request rejected with {}",
                response.status()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MCP_MAX_MESSAGE_BYTES as u64)
        {
            return Err(McpError::Transport(
                "MCP response exceeds the message size limit".into(),
            ));
        }
        if payload.get("method").and_then(|value| value.as_str()) == Some("initialize") {
            self.session_id = response
                .headers()
                .get("mcp-session-id")
                .map(|value| value.to_str().map(str::to_string))
                .transpose()
                .map_err(|error| {
                    McpError::Transport(format!("invalid MCP session header: {error}"))
                })?;
            if self.session_id.as_ref().is_some_and(|id| {
                id.is_empty() || !id.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
            }) {
                return Err(McpError::Transport("invalid MCP session identifier".into()));
            }
        }
        Ok(response)
    }

    /// Notifications and responses to server requests are acknowledged without an RPC body.
    pub(super) async fn send_one_way(
        &mut self,
        payload: &serde_json::Value,
    ) -> Result<(), McpError> {
        let response = self.send(payload).await.map_err(|error| {
            McpError::Transport(format!("MCP notification or reply failed: {error}"))
        })?;
        if response.status() != reqwest::StatusCode::ACCEPTED {
            return Err(McpError::Transport(format!(
                "MCP notification or reply expected HTTP 202, received {}",
                response.status()
            )));
        }
        Ok(())
    }
}

/// A validated inbound message either completes our request or needs a separate server reply.
pub(super) enum IncomingMessage {
    Result(serde_json::Value),
    Reply(serde_json::Value),
    Ignore,
}

/// Writes complete JSON-RPC lines through the same path for all stdio message kinds.
pub(super) async fn write_stdio(
    stdin: &mut tokio::process::ChildStdin,
    payload: &serde_json::Value,
) -> Result<(), McpError> {
    let mut line = serde_json::to_vec(payload)
        .map_err(|error| McpError::Transport(format!("failed to encode message: {error}")))?;
    line.push(b'\n');
    stdin
        .write_all(&line)
        .await
        .map_err(|error| McpError::Transport(format!("failed to write message: {error}")))?;
    stdin
        .flush()
        .await
        .map_err(|error| McpError::Transport(format!("failed to flush message: {error}")))
}
