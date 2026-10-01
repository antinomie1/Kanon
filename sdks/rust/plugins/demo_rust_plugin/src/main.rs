//! Demonstration Rust plugin for the Kanon microkernel.
//!
//! Provides an example out-of-process plugin that exposes static metadata,
//! responds to `GetPluginMeta`, and processes the `/rustcalc` command.

use kanon_sdk::prelude::message_segment::Segment;
use kanon_sdk::prelude::*;

/// Demonstration plugin implementation.
struct DemoPlugin;

#[async_trait]
impl Plugin for DemoPlugin {
    /// Returns the static metadata declaring plugin identity and supported commands.
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            id: "org.kanon.plugin.demo_rust".to_string(),
            name: "Demo Rust Plugin".to_string(),
            version: "0.1.0".to_string(),
            author: "Kanon Dev".to_string(),
            description: "Demonstration plugin written in Rust".to_string(),
            commands: vec![CommandMeta {
                name: "rustcalc".to_string(),
                description: "High-performance calculation command".to_string(),
                usage: "/rustcalc <expr>".to_string(),
                priority: 100,
                ..Default::default()
            }],
            tools: vec![ToolMeta {
                name: "fast_calc".to_string(),
                description: "High-performance mathematical calculation tool".to_string(),
                parameters: None,
            }],
            ..Default::default()
        }
    }

    /// Lifecycle hook called when the plugin is loaded by the host runner.
    async fn on_load(&mut self, ctx: &mut PluginContext) -> PluginResult<()> {
        println!(
            "Demo Rust Plugin initialized with data directory: {:?}",
            ctx.data_dir
        );
        Ok(())
    }

    /// Intercepts inbound events before command dispatch.
    ///
    /// Demonstrates pre-filter blocking if the message contains `[block]`.
    async fn on_pre_filter(
        &self,
        req: PipelineEventRequest,
    ) -> PluginResult<Option<PreFilterResult>> {
        if req.raw_text.contains("[block]") {
            let block_reply = MessageSegment {
                segment: Some(Segment::Text(TextSegment {
                    content: "Message blocked by Demo Rust Plugin pre-filter".to_string(),
                })),
            };
            return Ok(Some(PreFilterResult {
                action: pre_filter_result::Action::Block as i32,
                modified_text: String::new(),
                reply_messages: vec![block_reply],
            }));
        }
        Ok(None)
    }

    /// Handles command execution for the `/rustcalc` command.
    async fn on_execute_command(
        &self,
        req: CommandExecuteRequest,
    ) -> PluginResult<CommandExecuteResponse> {
        let reply_text = if req.command == "rustcalc" {
            let expr = req.args.join(" ");
            format!("Rust calculation result for [{expr}]: 42 (fast-path)")
        } else {
            format!("Unknown command: {}", req.command)
        };

        let reply = MessageSegment {
            segment: Some(Segment::Text(TextSegment {
                content: reply_text,
            })),
        };

        Ok(CommandExecuteResponse {
            success: true,
            replies: vec![reply],
            error_message: String::new(),
        })
    }

    /// Executes a registered tool call requested by the LLM state machine.
    async fn on_call_tool(&self, req: ToolCallRequest) -> PluginResult<ToolCallResponse> {
        if req.tool_name == "fast_calc" {
            let mut result_fields = std::collections::BTreeMap::new();
            result_fields.insert(
                "result".to_string(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::NumberValue(42.0)),
                },
            );
            result_fields.insert(
                "summary".to_string(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::StringValue(
                        "Calculation succeeded via Rust plugin tool".to_string(),
                    )),
                },
            );

            return Ok(ToolCallResponse {
                call_id: req.call_id,
                success: true,
                error_message: String::new(),
                payload: Some(tool_call_response::Payload::StructuredResult(
                    prost_types::Struct {
                        fields: result_fields,
                    },
                )),
                attachments: Vec::new(),
            });
        }

        Ok(ToolCallResponse {
            call_id: req.call_id,
            success: false,
            error_message: format!("Unknown tool: {}", req.tool_name),
            payload: None,
            attachments: Vec::new(),
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Launch plugin host listening on assigned socket.
    KanonHost::new(DemoPlugin).run().await?;
    Ok(())
}
