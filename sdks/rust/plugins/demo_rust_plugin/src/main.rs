//! Demonstration Rust plugin for the Kanon microkernel.
//!
//! Built with [`Router`]: each command, trigger, tool and hook is declared once, next to its
//! handler. Shows a plain command (`/rustcalc`), a command group backed by the node's KV store
//! (`/note add`, `/note list`), a multi-turn conversation started by a trigger (`count to N`), a
//! raw-JSON tool and a typed tool (`dice`), a per-chat system prompt rewrite (`/rules`), an HTTP
//! route and a pre-filter.

use std::time::Duration;

use kanon_sdk::prelude::*;

/// Arguments of the `dice` tool. The schema the model sees is generated from this struct, and
/// the doc comments become the parameters' descriptions.
#[derive(Deserialize, JsonSchema)]
struct DiceArgs {
    /// Faces on each die (default 6).
    sides: Option<u32>,
    /// How many dice to roll, 1-20 (default 1).
    count: Option<u32>,
}

/// The KV key holding a chat's notes.
fn notes_key(channel_id: &str) -> String {
    format!("notes:{channel_id}")
}

/// The KV key holding a chat's extra instructions for the assistant.
fn rules_key(channel_id: &str) -> String {
    format!("rules:{channel_id}")
}

/// One roll of a `sides`-faced die.
fn roll(sides: u32) -> u32 {
    use std::hash::BuildHasher;
    // Every `RandomState` gets fresh random keys, which is randomness enough for a demo and
    // saves a dependency on `rand`.
    let random = std::collections::hash_map::RandomState::new().hash_one(());
    (random % u64::from(sides)) as u32 + 1
}

fn plugin() -> Router {
    let router = Router::new("org.kanon.plugin.demo_rust", "Demo Rust Plugin", "0.1.0")
        .author("Kanon Dev")
        .description("Demonstration plugin written in Rust");
    // Handlers without an event (HTTP routes, actions) reach the core through the context slot.
    let context = router.context();

    router
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
        // "/note add <text>" and "/note list" form one group; "/note" alone lists them.
        .command_group(
            CommandSpec::new("note").description("Notes for this chat"),
            |group| {
                group
                    .command(
                        CommandSpec::new("add")
                            .description("Save a note for this chat")
                            .usage("/note add <text>"),
                        |event| async move {
                            if event.raw_args().is_empty() {
                                return Ok("Usage: /note add <text>".to_string());
                            }
                            let core = event.core()?;
                            let key = notes_key(event.channel_id());
                            let mut notes: Vec<String> =
                                core.kv_get(&key).await?.unwrap_or_default();
                            notes.push(event.raw_args().to_string());
                            core.kv_set(&key, &notes).await?;
                            Ok(format!("Saved note #{}.", notes.len()))
                        },
                    )
                    .command(
                        CommandSpec::new("list").description("Show this chat's notes"),
                        |event| async move {
                            let notes: Vec<String> = event
                                .core()?
                                .kv_get(&notes_key(event.channel_id()))
                                .await?
                                .unwrap_or_default();
                            if notes.is_empty() {
                                return Ok("No notes yet.".to_string());
                            }
                            let lines: Vec<String> = notes
                                .iter()
                                .enumerate()
                                .map(|(index, note)| format!("{}. {note}", index + 1))
                                .collect();
                            Ok(lines.join("\n"))
                        },
                    )
            },
        )
        .command(
            CommandSpec::new("rules")
                .description("Set extra instructions for the assistant in this chat")
                .usage("/rules [text]")
                .access(CommandAccess::AdminsInGroups),
            |event| async move {
                let core = event.core()?;
                let key = rules_key(event.channel_id());
                if event.raw_args().is_empty() {
                    core.kv_delete(&key).await?;
                    return Ok("Rules cleared.");
                }
                core.kv_set(&key, event.raw_args()).await?;
                Ok("Rules saved; the assistant follows them from the next message.")
            },
        )
        // The rewrite depends only on the chat, never on the message, so the prompt stays the
        // same for every turn and the provider's prompt cache keeps working.
        .rewrite_system_prompt(|prompt| async move {
            let rules: Option<String> = prompt
                .event
                .core()?
                .kv_get(&rules_key(prompt.event.channel_id()))
                .await?;
            Ok(rules.map(|rules| format!("{}\n\nRules for this chat:\n{rules}", prompt.prompt)))
        })
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
        .tool(
            ToolSpec::typed::<DiceArgs>("dice").description("Rolls dice for the user."),
            |args, _event| async move {
                let sides = args.sides.unwrap_or(6);
                let count = args.count.unwrap_or(1);
                // An error reaches the model as a failed call, which it can explain or retry.
                if !(1..=20).contains(&count) || sides < 2 {
                    return Err("count must be 1-20 and sides at least 2".into());
                }
                let rolls: Vec<u32> = (0..count).map(|_| roll(sides)).collect();
                Ok(json!({ "rolls": rolls, "total": rolls.iter().sum::<u32>() }))
            },
        )
        // GET /api/v1/plugins/org.kanon.plugin.demo_rust/http/notes?channel=<channel id>
        .http_route("GET", "/notes", move |request| {
            let context = context.clone();
            async move {
                let channel = request.query_param("channel").unwrap_or_default();
                let notes: Vec<String> = context
                    .core()?
                    .kv_get(&notes_key(&channel))
                    .await?
                    .unwrap_or_default();
                Ok(json!({ "channel": channel, "notes": notes }))
            }
        })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Launch plugin host listening on assigned socket.
    KanonHost::new(plugin()).run().await?;
    Ok(())
}
