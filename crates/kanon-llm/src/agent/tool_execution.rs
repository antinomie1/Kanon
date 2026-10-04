//! One tool catalog and execution contract shared by local and external agent runtimes.

use std::collections::HashMap;
use std::sync::Arc;

use kanon_proto::v1::{ToolCallRequest, tool_call_request, tool_call_response};

use super::{AgentHook, AgentTool};
use crate::error::{AgentError, GatewayError};
use crate::gateway::types::{ToolCall, ToolDefinition};
use crate::stop::{StopSignal, unless_stopped};
use crate::tool_router::{
    ExecutedToolCall, ToolAttachment, ToolHost, ensure_unique_tool_names, json_to_prost_struct,
    prost_struct_to_json, resolve_tools,
};

/// The execution target captured with the advertised definition, never resolved again by name.
enum ToolTarget {
    Native(Arc<dyn AgentTool>),
    Plugin {
        host: Arc<dyn ToolHost>,
        plugin_id: String,
        tool_name: String,
    },
}

/// Immutable definitions and their dispatch targets for one turn.
///
/// Owning the targets keeps metadata refreshes from changing who executes an advertised tool.
#[derive(Default)]
pub struct TurnTools {
    /// Model-facing definitions; callers may filter these but cannot replace captured targets.
    pub definitions: Vec<ToolDefinition>,
    targets: HashMap<String, ToolTarget>,
}

/// A settled call, before the agent writes its own durable result.
pub struct ToolExecution {
    /// Redacted model-facing result.
    pub text: String,
    /// Attribution and success for observers.
    pub record: ExecutedToolCall,
    /// Successful attachments in execution order.
    pub attachments: Vec<ToolAttachment>,
    /// Error to propagate if the caller's policy stops on tool failure.
    /// A policy veto is a normal denied result and does not terminate the turn.
    pub failure: Option<AgentError>,
}

impl TurnTools {
    /// Captures native and plugin/MCP tools, rejecting collisions before publishing the catalog.
    pub fn collect(
        native: &[Arc<dyn AgentTool>],
        hosts: &[Arc<dyn ToolHost>],
    ) -> Result<Self, AgentError> {
        let mut tools = Self::default();
        for tool in native {
            let mut definition = tool.definition();
            definition.parameters = crate::layout::canonical_json(definition.parameters);
            tools
                .targets
                .insert(definition.name.clone(), ToolTarget::Native(tool.clone()));
            tools.definitions.push(definition);
        }
        for resolved in resolve_tools(hosts)? {
            tools.targets.insert(
                resolved.definition.name.clone(),
                ToolTarget::Plugin {
                    host: hosts[resolved.host_index].clone(),
                    plugin_id: resolved.plugin_id,
                    tool_name: resolved.tool_name,
                },
            );
            tools.definitions.push(resolved.definition);
        }
        ensure_unique_tool_names(tools.definitions.iter().map(|tool| tool.name.as_str()))?;
        tools.definitions.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tools)
    }

    /// Executes an advertised call through the same validation, hooks and transport for every
    /// backend. The caller owns memory and failure policy; this executor owns no conversation.
    pub async fn execute(
        &self,
        session_id: &str,
        call: &ToolCall,
        advertised: &[ToolDefinition],
        hooks: &[Arc<dyn AgentHook>],
    ) -> Result<ToolExecution, AgentError> {
        let stop = crate::stop::current();
        if stop.as_ref().is_some_and(StopSignal::is_stopped) {
            return Err(AgentError::Stopped);
        }
        for hook in hooks {
            if !hook.on_before_tool_call(session_id, call).await? {
                return Ok(ToolExecution {
                    text: format!("Tool '{}' execution was denied by agent policy", call.name),
                    record: record(call, "policy", "agent_hook", false),
                    attachments: Vec::new(),
                    failure: None,
                });
            }
        }
        let output = if !call.arguments.is_object() {
            let text = format!(
                "Tool '{}' arguments must be a JSON object; correct the arguments and retry",
                call.name
            );
            ToolExecution {
                failure: Some(GatewayError::InvalidResponse(text.clone()).into()),
                text,
                record: record(call, "validation", "agent", false),
                attachments: Vec::new(),
            }
        } else {
            // Hooks may remove names from the request but cannot redirect a captured target.
            let target = self
                .targets
                .get(&call.name)
                .filter(|_| advertised.iter().any(|tool| tool.name == call.name));
            match target {
                Some(ToolTarget::Native(tool)) => {
                    let result = unless_stopped(
                        stop.as_ref(),
                        tool.call(session_id, call.arguments.clone()),
                    )
                    .await
                    .ok_or(AgentError::Stopped)?;
                    match result {
                        Ok(output) => ToolExecution {
                            text: output.text,
                            record: record(call, "native", "in_process", true),
                            attachments: output.attachments,
                            failure: None,
                        },
                        Err(error) => {
                            let text = format!("Error: {error}");
                            ToolExecution {
                                failure: Some(AgentError::ToolFailed(format!(
                                    "Native tool '{}' failed: {text}",
                                    call.name
                                ))),
                                text,
                                record: record(call, "native", "in_process", false),
                                attachments: Vec::new(),
                            }
                        }
                    }
                }
                Some(ToolTarget::Plugin {
                    host,
                    plugin_id,
                    tool_name,
                }) => {
                    let request = ToolCallRequest {
                        call_id: call.id.clone(),
                        tool_name: tool_name.clone(),
                        session_id: session_id.into(),
                        payload: Some(tool_call_request::Payload::StructuredArgs(
                            json_to_prost_struct(&call.arguments)
                                .expect("validated object arguments"),
                        )),
                        // The host adds the actual platform event from the execution scope.
                        context: None,
                    };
                    let result = unless_stopped(stop.as_ref(), host.call_tool(request))
                        .await
                        .ok_or(AgentError::Stopped)?;
                    match result {
                        Ok(response) => {
                            // Malformed results and failed calls cannot publish attachments.
                            let decoded = if !response.success {
                                Err(response.error_message)
                            } else {
                                match response.payload {
                                    Some(tool_call_response::Payload::StructuredResult(value)) => {
                                        prost_struct_to_json(value)
                                            .map(|value| value.to_string())
                                            .map_err(|error| {
                                                format!("invalid structured tool result: {error}")
                                            })
                                    }
                                    Some(tool_call_response::Payload::RawBytes(bytes)) => {
                                        Ok(String::from_utf8_lossy(&bytes).into_owned())
                                    }
                                    None => Ok("{}".into()),
                                }
                            };
                            let success = decoded.is_ok();
                            let mut text =
                                decoded.unwrap_or_else(|error| format!("Error: {error}"));
                            for path in response
                                .attachments
                                .iter()
                                .filter_map(|a| a.file_path.as_deref())
                            {
                                let path = path.trim();
                                if !path.is_empty() {
                                    text = text.replace(path, "[attachment]");
                                }
                            }
                            ToolExecution {
                                failure: (!success).then(|| {
                                    AgentError::ToolFailed(format!(
                                        "Plugin tool '{}' failed: {text}",
                                        call.name
                                    ))
                                }),
                                text,
                                record: record(call, plugin_id, host.host_id(), success),
                                attachments: if success {
                                    response
                                        .attachments
                                        .into_iter()
                                        .map(ToolAttachment::from_proto)
                                        .collect()
                                } else {
                                    Vec::new()
                                },
                            }
                        }
                        Err(status) => ToolExecution {
                            text: format!("RPC Error: {}", status.message()),
                            record: record(call, plugin_id, host.host_id(), false),
                            attachments: Vec::new(),
                            failure: Some(status.into()),
                        },
                    }
                }
                None => ToolExecution {
                    text: format!("Tool '{}' not registered", call.name),
                    record: record(call, "unknown", "unknown", false),
                    attachments: Vec::new(),
                    failure: Some(AgentError::ToolNotFound(call.name.clone())),
                },
            }
        };
        for hook in hooks {
            hook.on_after_tool_call(session_id, call, &output.text, output.record.success)
                .await?;
        }
        Ok(output)
    }
}

fn record(call: &ToolCall, plugin_id: &str, host_id: &str, success: bool) -> ExecutedToolCall {
    ExecutedToolCall {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        plugin_id: plugin_id.into(),
        host_id: host_id.into(),
        success,
    }
}
