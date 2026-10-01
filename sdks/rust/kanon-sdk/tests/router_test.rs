//! `Router`: declarations become metadata, return values become replies, and `wait_next` carries
//! one handler across several `OnExecuteCommand` calls.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kanon_sdk::prelude::*;
use serde_json::json;

/// A command request as Core sends it, from sender u1 in group g1.
fn request(command: &str, text: &str, args: &[&str], continuation: bool) -> CommandExecuteRequest {
    CommandExecuteRequest {
        plugin_id: "test.plugin".into(),
        command: command.into(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
        raw_args: text.into(),
        continuation,
        context: Some(PipelineEventRequest {
            event_id: format!("onebot:{text}"),
            platform: "onebot".into(),
            channel_id: "group:g1".into(),
            sender_id: "u1".into(),
            raw_text: text.into(),
            ..Default::default()
        }),
    }
}

fn texts(response: &CommandExecuteResponse) -> Vec<String> {
    response
        .replies
        .iter()
        .map(|segment| match &segment.segment {
            Some(message_segment::Segment::Text(text)) => text.content.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

fn demo(seen: Arc<Mutex<Vec<String>>>) -> Router {
    Router::new("test.plugin", "Test", "0.1.0")
        .command(
            CommandSpec::new("echo")
                .alias("/e")
                .access(CommandAccess::AdminsInGroups),
            |event| async move { Ok(event.args().join(" ")) },
        )
        .command(CommandSpec::new("ask"), |event| async move {
            event.reply("name?").await?;
            if let Ok(answer) = event.wait_next(Duration::from_secs(30)).await {
                answer.reply(format!("hello {}", answer.text())).await?;
            }
            Ok(())
        })
        .command(CommandSpec::new("boom"), |_event| async move {
            Err::<(), _>("bad input".into())
        })
        .trigger(TriggerSpec::new("ping", "^ping$"), |event| async move {
            Ok(vec![
                segment::quote(event.event_id()),
                segment::text("pong"),
            ])
        })
        .tool(ToolSpec::new("who"), |args, event| async move {
            Ok(json!({
                "asked": args["q"],
                "sender": event.map(|event| event.sender_id().to_string()),
            }))
        })
        .subscribe(EventKind::LlmResponse, move |event| {
            let seen = seen.clone();
            async move {
                if let Event::LlmResponse(answer) = event {
                    seen.lock().unwrap().push(answer.content);
                }
                Ok(())
            }
        })
        .decorate_reply(|reply| async move {
            Ok((reply.source == ReplySource::Llm).then(|| {
                let mut segments = reply.segments;
                segments.push(segment::text(" — bot"));
                segments
            }))
        })
}

#[test]
fn meta_lists_commands_triggers_and_hooks() {
    let meta = demo(Arc::default()).meta();
    let echo = meta.commands.iter().find(|c| c.name == "echo").unwrap();
    assert_eq!(echo.aliases, vec!["e"]);
    assert_eq!(echo.access, CommandAccess::AdminsInGroups as i32);
    assert_eq!(meta.triggers[0].pattern, "^ping$");
    assert_eq!(meta.events, vec![EventKind::LlmResponse as i32]);
    assert!(meta.decorates_replies);
}

#[test]
#[should_panic(expected = "already declared")]
fn command_and_trigger_names_must_not_collide() {
    let _ = Router::new("x", "x", "0")
        .command(CommandSpec::new("x"), |_e| async move { Ok(()) })
        .trigger(TriggerSpec::new("x", "^x$"), |_e| async move { Ok(()) });
}

#[tokio::test]
async fn return_values_become_replies_and_errors_are_reported() {
    let plugin = demo(Arc::default());
    let echoed = plugin
        .on_execute_command(request("echo", "a b", &["a", "b"], false))
        .await
        .unwrap();
    assert!(echoed.success);
    assert_eq!(texts(&echoed), ["a b"]);

    let pinged = plugin
        .on_execute_command(request("ping", "ping", &[], false))
        .await
        .unwrap();
    assert!(matches!(
        &pinged.replies[0].segment,
        Some(message_segment::Segment::Reply(quote)) if quote.target_message_id == "onebot:ping"
    ));

    let failed = plugin
        .on_execute_command(request("boom", "", &[], false))
        .await
        .unwrap();
    assert!(!failed.success);
    assert_eq!(failed.error_message, "bad input");
}

#[tokio::test]
async fn wait_next_spans_two_calls() {
    let plugin = demo(Arc::default());
    let first = plugin
        .on_execute_command(request("ask", "", &[], false))
        .await
        .unwrap();
    // The first turn ends at wait_next: its reply goes out and Core is asked to capture.
    assert_eq!(texts(&first), ["name?"]);
    assert_eq!(first.capture_seconds, 30);

    let second = plugin
        .on_execute_command(request("ask", "Ann", &["Ann"], true))
        .await
        .unwrap();
    assert!(second.success);
    assert_eq!(texts(&second), ["hello Ann"]);
    assert_eq!(second.capture_seconds, 0);
}

#[tokio::test]
async fn continuation_without_waiter_runs_the_handler_afresh() {
    // E.g. the host restarted while Core still held the capture.
    let response = demo(Arc::default())
        .on_execute_command(request("echo", "late", &["late"], true))
        .await
        .unwrap();
    assert_eq!(texts(&response), ["late"]);
}

#[tokio::test]
async fn newer_wait_supersedes_the_older_one() {
    let plugin = demo(Arc::default());
    plugin
        .on_execute_command(request("ask", "", &[], false))
        .await
        .unwrap();
    plugin
        .on_execute_command(request("ask", "", &[], false))
        .await
        .unwrap();
    let response = plugin
        .on_execute_command(request("ask", "Bo", &["Bo"], true))
        .await
        .unwrap();
    assert_eq!(texts(&response), ["hello Bo"]);
}

#[tokio::test]
async fn tools_receive_json_and_the_asking_message() {
    let response = demo(Arc::default())
        .on_call_tool(ToolCallRequest {
            call_id: "c1".into(),
            tool_name: "who".into(),
            payload: Some(tool_call_request::Payload::StructuredArgs(
                kanon_sdk::json::to_struct(json!({ "q": "me" }).as_object().unwrap().clone()),
            )),
            context: request("x", "", &[], false).context,
            ..Default::default()
        })
        .await
        .unwrap();
    let Some(tool_call_response::Payload::StructuredResult(result)) = response.payload else {
        panic!("expected a structured result");
    };
    assert_eq!(
        serde_json::Value::Object(kanon_sdk::json::from_struct(result)),
        json!({ "asked": "me", "sender": "u1" })
    );
}

#[tokio::test]
async fn events_dispatch_by_kind_and_the_decorator_sees_the_source() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let plugin = demo(seen.clone());
    plugin
        .on_event(EventNotification {
            detail: Some(event_notification::Detail::LlmResponse(LlmResponseEvent {
                content: "answer".into(),
                context: None,
            })),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), ["answer"]);

    let decorated = plugin
        .on_decorate_reply(DecorateReplyRequest {
            segments: vec![segment::text("hi")],
            source: ReplySource::Llm as i32,
            ..Default::default()
        })
        .await
        .unwrap()
        .expect("model replies are decorated");
    assert_eq!(decorated.len(), 2);

    let untouched = plugin
        .on_decorate_reply(DecorateReplyRequest {
            source: ReplySource::Command as i32,
            command: "echo".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(untouched.is_none());
}
