//! Demonstration weather plugin for the Kanon microkernel.
//!
//! Provides weather queries via the `/weather` command and `fetch_weather` tool calling.

use kanon_sdk::prelude::message_segment::Segment;
use kanon_sdk::prelude::*;

/// Weather plugin implementation.
struct WeatherPlugin;

#[async_trait]
impl Plugin for WeatherPlugin {
    /// Returns static metadata describing this weather plugin and its capabilities.
    fn meta(&self) -> PluginMeta {
        let mut properties = std::collections::BTreeMap::new();
        let mut city_prop = std::collections::BTreeMap::new();
        city_prop.insert(
            "type".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue("string".to_string())),
            },
        );
        city_prop.insert(
            "description".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue(
                    "The city to query weather for".to_string(),
                )),
            },
        );
        properties.insert(
            "city".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StructValue(prost_types::Struct {
                    fields: city_prop,
                })),
            },
        );

        let mut param_fields = std::collections::BTreeMap::new();
        param_fields.insert(
            "type".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue("object".to_string())),
            },
        );
        param_fields.insert(
            "properties".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StructValue(prost_types::Struct {
                    fields: properties,
                })),
            },
        );

        PluginMeta {
            id: "org.kanon.plugin.weather".to_string(),
            name: "Demo Weather Plugin".to_string(),
            version: "0.1.0".to_string(),
            author: "Kanon Dev".to_string(),
            description: "Demonstration weather plugin for Kanon".to_string(),
            commands: vec![CommandMeta {
                name: "weather".to_string(),
                description: "Fetch current weather report".to_string(),
                usage: "/weather <city>".to_string(),
                priority: 200,
                ..Default::default()
            }],
            tools: vec![ToolMeta {
                name: "fetch_weather".to_string(),
                description: "Fetch current weather data for a given city".to_string(),
                parameters: Some(prost_types::Struct {
                    fields: param_fields,
                }),
            }],
            ..Default::default()
        }
    }

    /// Handles plugin initialization hook.
    async fn on_load(&mut self, ctx: &mut PluginContext) -> PluginResult<()> {
        println!(
            "Demo Weather Plugin initialized with data directory: {:?}",
            ctx.data_dir
        );
        Ok(())
    }

    /// Executes the `/weather` command.
    async fn on_execute_command(
        &self,
        req: CommandExecuteRequest,
    ) -> PluginResult<CommandExecuteResponse> {
        let reply_text = if req.command == "weather" {
            let city = if req.args.is_empty() {
                "Tokyo".to_string()
            } else {
                req.args.join(" ")
            };
            format!("Weather in {city}: Sunny, 22°C, Humidity: 45%")
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
            ..Default::default()
        })
    }

    /// Executes registered tool calls for `fetch_weather`.
    async fn on_call_tool(&self, req: ToolCallRequest) -> PluginResult<ToolCallResponse> {
        if req.tool_name == "fetch_weather" {
            let mut result_fields = std::collections::BTreeMap::new();
            result_fields.insert(
                "condition".to_string(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::StringValue("Sunny".to_string())),
                },
            );
            result_fields.insert(
                "temperature".to_string(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::NumberValue(22.0)),
                },
            );
            result_fields.insert(
                "unit".to_string(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::StringValue("Celsius".to_string())),
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
    // Launch plugin host listening on assigned socket or IPC channel.
    KanonHost::new(WeatherPlugin).run().await?;
    Ok(())
}
