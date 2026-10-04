//! `Router` features beyond plain commands: command groups, HTTP routes, system prompt
//! rewriting and the agent's lifecycle events — through the `Plugin` trait and, for the hook
//! and HTTP RPCs, through a running host.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kanon_proto::v1::message_pipeline_service_client::MessagePipelineServiceClient;
use kanon_sdk::prelude::*;
use kanon_transport::connect_ipc;
use tokio::sync::oneshot;

/// A command request as Core sends it, from sender u1 in group g1.
fn request(command: &str, raw_args: &str, continuation: bool) -> CommandExecuteRequest {
    CommandExecuteRequest {
        plugin_id: "test.plugin".into(),
        command: command.into(),
        args: raw_args.split_whitespace().map(str::to_string).collect(),
        raw_args: raw_args.into(),
        continuation,
        context: Some(message("onebot", raw_args)),
    }
}

fn message(platform: &str, text: &str) -> PipelineEventRequest {
    PipelineEventRequest {
        event_id: format!("{platform}:{text}"),
        platform: platform.into(),
        channel_id: "group:g1".into(),
        sender_id: "u1".into(),
        raw_text: text.into(),
        ..Default::default()
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

fn admin() -> Router {
    Router::new("test.plugin", "Test", "0.1.0").command_group(
        CommandSpec::new("admin")
            .description("Moderation")
            .access(CommandAccess::Admins),
        |group| {
            group
                .command(
                    CommandSpec::new("ban")
                        .description("Ban a user")
                        .usage("/admin ban <user>")
                        .alias("b"),
                    |event| async move {
                        Ok(format!(
                            "{} {} [{}] '{}'",
                            event.command(),
                            event.subcommand().unwrap_or("-"),
                            event.args().join(","),
                            event.raw_args()
                        ))
                    },
                )
                .command("rename", |event| async move {
                    event.reply("new name?").await?;
                    let answer = event.wait_next(Duration::from_secs(30)).await?;
                    Ok(format!("renamed to {}", answer.text()))
                })
        },
    )
}

#[test]
fn a_group_is_one_command_listing_its_subcommands() {
    let meta = admin().meta();
    assert_eq!(meta.commands.len(), 1);
    let group = &meta.commands[0];
    assert_eq!(group.name, "admin");
    assert_eq!(group.usage, "/admin <subcommand>");
    assert_eq!(group.access, CommandAccess::Admins as i32);
    let subs: Vec<_> = group
        .subcommands
        .iter()
        .map(|sub| (sub.name.as_str(), sub.usage.as_str(), sub.aliases.clone()))
        .collect();
    assert_eq!(
        subs,
        [
            ("ban", "/admin ban <user>", vec!["b".to_string()]),
            ("rename", "/admin rename", vec![]),
        ]
    );
}

#[tokio::test]
async fn subcommands_see_arguments_without_their_own_name() {
    let router = admin();
    let banned = router
        .on_execute_command(request("admin", "ban  alice  for spam", false))
        .await
        .unwrap();
    assert!(banned.success);
    assert_eq!(
        texts(&banned),
        ["admin ban [alice,for,spam] 'alice  for spam'"]
    );

    // An alias reaches the subcommand under its canonical name.
    let aliased = router
        .on_execute_command(request("admin", "b bob", false))
        .await
        .unwrap();
    assert_eq!(texts(&aliased), ["admin ban [bob] 'bob'"]);
}

#[tokio::test]
async fn a_bare_group_or_unknown_subcommand_answers_with_help() {
    let router = admin();
    let help =
        "/admin <subcommand> — Moderation\n  /admin ban <user> — Ban a user\n  /admin rename";

    let bare = router
        .on_execute_command(request("admin", "", false))
        .await
        .unwrap();
    assert!(bare.success);
    assert_eq!(texts(&bare), [help]);

    let unknown = router
        .on_execute_command(request("admin", "kick carol", false))
        .await
        .unwrap();
    assert_eq!(
        texts(&unknown),
        [format!("Unknown subcommand: kick\n{help}")]
    );
}

#[tokio::test]
async fn subcommands_can_wait_for_the_next_message() {
    let router = admin();
    let asked = router
        .on_execute_command(request("admin", "rename", false))
        .await
        .unwrap();
    assert_eq!(texts(&asked), ["new name?"]);
    assert!(asked.capture_seconds > 0);

    let answered = router
        .on_execute_command(request("admin", "kanon", true))
        .await
        .unwrap();
    assert_eq!(texts(&answered), ["renamed to kanon"]);
    assert_eq!(answered.capture_seconds, 0);
}

#[test]
#[should_panic(expected = "apply to the whole group")]
fn subcommand_access_is_refused_because_core_cannot_enforce_it() {
    let _ = Router::new("test.plugin", "Test", "0.1.0").command_group("admin", |group| {
        group.command(
            CommandSpec::new("ban").access(CommandAccess::Admins),
            |_event| async move { Ok(()) },
        )
    });
}

#[test]
#[should_panic(expected = "declares no subcommand")]
fn an_empty_group_is_refused() {
    let _ = Router::new("test.plugin", "Test", "0.1.0").command_group("admin", |group| group);
}

fn http_router() -> Router {
    #[derive(Deserialize)]
    struct Note {
        text: String,
    }
    Router::new("test.plugin", "Test", "0.1.0")
        .http_route("GET", "/status", |request| async move {
            Ok(json!({
                "ok": true,
                "name": request.query_param("name"),
                "agent": request.header("User-Agent"),
            }))
        })
        .http_route("post", "/notes", |request| async move {
            let note: Note = request.json()?;
            Ok(http::Response::json(&json!({ "saved": note.text }))?
                .status(201)
                .header("x-note", "1"))
        })
        .http_route("GET", "/fail", |_request| async move {
            Err::<String, _>("database is locked".into())
        })
}

fn http_request(method: &str, path: &str, query: &str, body: &str) -> HttpRequest {
    HttpRequest {
        plugin_id: "test.plugin".into(),
        method: method.into(),
        path: path.into(),
        query: query.into(),
        headers: vec![HttpHeader {
            name: "user-agent".into(),
            value: "tests".into(),
        }],
        body: body.as_bytes().to_vec(),
    }
}

fn header<'a>(response: &'a HttpResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|header| header.name == name)
        .map(|header| header.value.as_str())
}

fn body_json(response: &HttpResponse) -> serde_json::Value {
    serde_json::from_slice(&response.body).expect("JSON body")
}

#[tokio::test]
async fn http_routes_dispatch_by_method_and_path() {
    let router = http_router();
    assert!(router.meta().serves_http);
    assert!(!admin().meta().serves_http);

    let status = router
        .on_http_request(http_request("GET", "/status", "name=a%20b+c&x", ""))
        .await
        .unwrap();
    assert_eq!(status.status, 200);
    assert_eq!(header(&status, "content-type"), Some("application/json"));
    assert_eq!(
        body_json(&status),
        json!({ "ok": true, "name": "a b c", "agent": "tests" })
    );

    let created = router
        .on_http_request(http_request("POST", "/notes", "", r#"{"text":"hi"}"#))
        .await
        .unwrap();
    assert_eq!(created.status, 201);
    assert_eq!(header(&created, "x-note"), Some("1"));
    assert_eq!(body_json(&created), json!({ "saved": "hi" }));
}

#[tokio::test]
async fn http_failures_answer_with_status_codes() {
    let router = http_router();

    let missing = router
        .on_http_request(http_request("GET", "/nope", "", ""))
        .await
        .unwrap();
    assert_eq!(missing.status, 404);
    assert_eq!(body_json(&missing), json!({ "error": "not found" }));

    let wrong_method = router
        .on_http_request(http_request("DELETE", "/notes", "", ""))
        .await
        .unwrap();
    assert_eq!(wrong_method.status, 405);
    assert_eq!(header(&wrong_method, "allow"), Some("POST"));

    let failed = router
        .on_http_request(http_request("GET", "/fail", "", ""))
        .await
        .unwrap();
    assert_eq!(failed.status, 500);
    // The handler's error is logged, never shown to whoever called the route.
    assert_eq!(body_json(&failed), json!({ "error": "internal error" }));

    // A body that is not the declared JSON is the handler's error, hence a 500.
    let malformed = router
        .on_http_request(http_request("POST", "/notes", "", "{"))
        .await
        .unwrap();
    assert_eq!(malformed.status, 500);
}

#[test]
#[should_panic(expected = "is already declared")]
fn http_routes_must_be_unique() {
    let _ = Router::new("test.plugin", "Test", "0.1.0")
        .http_route("GET", "/a", |_request| async move { Ok("a") })
        .http_route("get", "/a", |_request| async move { Ok("b") });
}

fn hook_request(platform: &str) -> LlmRequestHookRequest {
    LlmRequestHookRequest {
        plugin_id: "test.plugin".into(),
        context: Some(message(platform, "hello")),
        session_id: "s1".into(),
        system_prompt: "You are Kanon.".into(),
    }
}

fn prompt_router() -> Router {
    Router::new("test.plugin", "Test", "0.1.0").rewrite_system_prompt(|prompt| async move {
        Ok(match prompt.event.platform() {
            "discord" => Some(format!(
                "{}\nUse Markdown. ({})",
                prompt.prompt, prompt.session_id
            )),
            "empty" => Some(String::new()),
            _ => None,
        })
    })
}

#[tokio::test]
async fn the_prompt_rewriter_sees_the_turn_and_may_replace_the_prompt() {
    let router = prompt_router();
    assert!(router.meta().rewrites_system_prompt);
    assert!(!admin().meta().rewrites_system_prompt);

    let rewritten = router
        .on_llm_request(hook_request("discord"))
        .await
        .unwrap();
    assert_eq!(
        rewritten.as_deref(),
        Some("You are Kanon.\nUse Markdown. (s1)")
    );
    assert_eq!(
        router.on_llm_request(hook_request("onebot")).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn agent_events_reach_their_subscribers() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = |seen: Arc<Mutex<Vec<String>>>| {
        move |event: Event| {
            let seen = seen.clone();
            async move {
                let line = match event {
                    Event::AgentBegin(begin) => format!(
                        "begin {} from {}",
                        begin.session_id,
                        begin
                            .event
                            .map(|e| e.sender_id().to_string())
                            .unwrap_or_default()
                    ),
                    Event::ToolCall(call) => {
                        format!("call {} {}", call.tool_name, call.arguments["city"])
                    }
                    Event::ToolResult(result) => {
                        format!(
                            "result {} {} {}",
                            result.tool_name, result.success, result.result
                        )
                    }
                    Event::AgentDone(done) => format!(
                        "done {} '{}' {:?} {}",
                        done.success,
                        done.content,
                        done.tools,
                        done.event.is_some()
                    ),
                    other => format!("unexpected {other:?}"),
                };
                seen.lock().unwrap().push(line);
                Ok(())
            }
        }
    };
    let router = Router::new("test.plugin", "Test", "0.1.0")
        .subscribe(EventKind::AgentBegin, record(seen.clone()))
        .subscribe(EventKind::ToolCall, record(seen.clone()))
        .subscribe(EventKind::ToolResult, record(seen.clone()))
        .subscribe(EventKind::AgentDone, record(seen.clone()));
    let mut kinds = router.meta().events;
    kinds.sort();
    let mut expected = vec![
        EventKind::AgentBegin as i32,
        EventKind::ToolCall as i32,
        EventKind::ToolResult as i32,
        EventKind::AgentDone as i32,
    ];
    expected.sort();
    assert_eq!(kinds, expected);

    let serde_json::Value::Object(arguments) = json!({ "city": "Paris" }) else {
        unreachable!()
    };
    let context = Some(message("onebot", "weather?"));
    let details = [
        event_notification::Detail::AgentBegin(AgentBeginEvent {
            context: context.clone(),
            session_id: "s1".into(),
        }),
        event_notification::Detail::ToolCall(ToolCallEvent {
            context: context.clone(),
            session_id: "s1".into(),
            tool_name: "forecast".into(),
            arguments: Some(kanon_sdk::json::to_struct(arguments)),
        }),
        event_notification::Detail::ToolResult(ToolResultEvent {
            context: context.clone(),
            session_id: "s1".into(),
            tool_name: "forecast".into(),
            success: true,
            result: "sunny".into(),
        }),
        event_notification::Detail::AgentDone(AgentDoneEvent {
            context: None,
            session_id: "s1".into(),
            success: true,
            content: "It is sunny.".into(),
            error: String::new(),
            tools: vec!["forecast".into()],
        }),
    ];
    for detail in details {
        router
            .on_event(EventNotification {
                detail: Some(detail),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    // A notification without its detail is a contract violation, reported rather than dropped.
    assert!(router.on_event(EventNotification::default()).await.is_err());

    // Invalid arguments must not be delivered as a plausible null to event subscribers.
    let error = router
        .on_event(EventNotification {
            detail: Some(event_notification::Detail::ToolCall(ToolCallEvent {
                arguments: Some(nonfinite_parameters()),
                ..Default::default()
            })),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("protobuf number must be finite"));

    assert_eq!(
        *seen.lock().unwrap(),
        [
            "begin s1 from u1",
            "call forecast \"Paris\"",
            "result forecast true sunny",
            "done true 'It is sunny.' [\"forecast\"] false",
        ]
    );
}

fn temp_socket(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("kanon-sdk-tests");
    std::fs::create_dir_all(&dir).expect("temp socket dir");
    dir.join(format!("{name}-{}.sock", std::process::id()))
}

fn nonfinite_parameters() -> prost_types::Struct {
    prost_types::Struct {
        fields: [(
            "value".into(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::NumberValue(f64::NAN)),
            },
        )]
        .into(),
    }
}

#[tokio::test]
async fn invalid_action_parameters_never_reach_the_handler() {
    use kanon_proto::v1::plugin_host_service_client::PluginHostServiceClient;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let calls = Arc::new(AtomicUsize::new(0));
    let recorded = calls.clone();
    let plugin = Router::new("test.plugin", "Test", "0.1.0").action("inspect", move |params| {
        recorded.fetch_add(1, Ordering::SeqCst);
        async move { Ok(params) }
    });
    let socket = temp_socket("invalid-action-parameters");
    let host = KanonHost::new(plugin).with_socket_path(socket.clone());
    let (stop, stopped) = oneshot::channel::<()>();
    let running = tokio::spawn(host.run_with_shutdown(async move {
        let _ = stopped.await;
    }));
    let mut channel = None;
    for _ in 0..50 {
        if let Ok(connected) = connect_ipc(socket.clone()).await {
            channel = Some(connected);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut client = PluginHostServiceClient::new(channel.expect("host is reachable"));
    let response = client
        .invoke_action(PluginActionRequest {
            plugin_id: "test.plugin".into(),
            action: "inspect".into(),
            parameters: Some(nonfinite_parameters()),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(!response.success);
    assert!(
        response
            .error_message
            .contains("protobuf number must be finite")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let response = client
        .invoke_action(PluginActionRequest {
            plugin_id: "test.plugin".into(),
            action: "inspect".into(),
            parameters: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.success);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(client);
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
}

/// The hook and HTTP RPCs reach the router through a running host, and an empty rewrite is
/// refused there instead of being sent to the core as a prompt.
#[tokio::test]
async fn the_host_serves_the_prompt_hook_and_http_rpcs() {
    let socket = temp_socket("router-features-host");
    let _ = std::fs::remove_file(&socket);
    let plugin = prompt_router().http_route("GET", "/status", |_request| async move {
        Ok(json!({ "ok": true }))
    });
    let host = KanonHost::new(plugin).with_socket_path(socket.clone());
    let (stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = host
            .run_with_shutdown(async move {
                let _ = stopped.await;
            })
            .await;
    });
    let mut channel = None;
    for _ in 0..50 {
        if let Ok(connected) = connect_ipc(socket.clone()).await {
            channel = Some(connected);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut client = MessagePipelineServiceClient::new(channel.expect("host is reachable"));

    let rewritten = client
        .on_llm_request(hook_request("discord"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        rewritten.system_prompt.as_deref(),
        Some("You are Kanon.\nUse Markdown. (s1)")
    );
    let kept = client
        .on_llm_request(hook_request("onebot"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(kept.system_prompt, None);
    let empty = client
        .on_llm_request(hook_request("empty"))
        .await
        .expect_err("an empty rewrite is refused");
    assert_eq!(empty.code(), tonic::Code::Internal);

    let status = client
        .on_http_request(http_request("GET", "/status", "", ""))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(status.status, 200);
    assert_eq!(body_json(&status), json!({ "ok": true }));

    let _ = stop.send(());
    let _ = std::fs::remove_file(&socket);
}
