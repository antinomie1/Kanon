//! Demonstration Rust plugin for the Kanon microkernel.
//!
//! Built with [`Router`]: each command, trigger and tool is declared once, next to its handler.
//! Shows a plain command (`/rustcalc`), a multi-turn conversation started by a trigger
//! (`count to N`), a tool and a pre-filter.

use std::time::Duration;

use kanon_sdk::prelude::*;
use serde_json::json;

fn plugin() -> Router {
    Router::new("org.kanon.plugin.demo_rust", "Demo Rust Plugin", "0.1.0")
        .author("Kanon Dev")
        .description("Demonstration plugin written in Rust")
        .pre_filter(|event| async move {
            // Blocks any message containing `[block]`, answering it directly.
            if !event.text().contains("[block]") {
                return Ok(None);
            }
            Ok(Some(PreFilterResult {
                action: pre_filter_result::Action::Block as i32,
                modified_text: String::new(),
                reply_messages: vec![segment::text(
                    "Message blocked by Demo Rust Plugin pre-filter",
                )],
            }))
        })
        .command(
            CommandSpec::new("rustcalc")
                .description("High-performance calculation command")
                .usage("/rustcalc <expr>")
                .priority(100),
            |event| async move {
                // Returning text is the shortest way to answer.
                let expr = event.args().join(" ");
                Ok(format!(
                    "Rust calculation result for [{expr}]: 42 (fast-path)"
                ))
            },
        )
        .trigger(
            TriggerSpec::new("count", r"^count to (\d)$").description("Counts along with you"),
            |event| async move {
                // A multi-turn conversation: each wait_next sends the replies so far and resumes
                // with the same sender's next message in this channel.
                let target: u32 = event.args()[0].parse()?;
                let mut expected = 1;
                event
                    .reply(format!("Let's count to {target}. You start!"))
                    .await?;
                let mut current = event;
                while expected <= target {
                    let Ok(next) = current.wait_next(Duration::from_secs(60)).await else {
                        current.reply("Too slow — maybe next time.").await?;
                        return Ok(());
                    };
                    if next.text().trim() != expected.to_string() {
                        next.reply(format!("That's not {expected}. Game over."))
                            .await?;
                        return Ok(());
                    }
                    expected += 1;
                    if expected <= target {
                        next.reply(expected.to_string()).await?;
                        expected += 1;
                    }
                    current = next;
                }
                current.reply("Done!").await?;
                Ok(())
            },
        )
        .tool(
            ToolSpec::new("fast_calc")
                .description("High-performance mathematical calculation tool"),
            |_args, _event| async move {
                Ok(json!({
                    "result": 42.0,
                    "summary": "Calculation succeeded via Rust plugin tool",
                }))
            },
        )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Launch plugin host listening on assigned socket.
    KanonHost::new(plugin()).run().await?;
    Ok(())
}
