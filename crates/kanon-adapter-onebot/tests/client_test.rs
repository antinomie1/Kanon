//! Common API wire contracts and response/error handling over real WebSocket peers.

use futures_util::{SinkExt, StreamExt};
use kanon_adapter_onebot::{OneBotAdapter, OneBotConfig, OneBotError, TransportKind, protocol::*};
use kanon_core::{EventIngress, PlatformAdapter};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::timeout,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message as WsMessage, client::IntoClientRequest, http::HeaderValue},
};

type Peer = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn reverse() -> (Arc<OneBotAdapter>, Peer) {
    let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_url = format!("ws://{}/", reserve.local_addr().unwrap());
    drop(reserve);
    let adapter = Arc::new(
        OneBotAdapter::new(OneBotConfig {
            enabled: true,
            transport: TransportKind::ReverseWebsocket,
            ws_url: ws_url.clone(),
            ..Default::default()
        })
        .unwrap(),
    );
    let (sender, _receiver) = mpsc::channel(1);
    adapter.start(EventIngress::new(sender)).await.unwrap();
    let mut request = ws_url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("x-client-role", HeaderValue::from_static("Universal"));
    request
        .headers_mut()
        .insert("x-self-id", HeaderValue::from_static("100"));
    let peer = tokio_tungstenite::connect_async(request).await.unwrap().0;
    wait_connected(&adapter).await;
    (adapter, peer)
}

async fn wait_connected(adapter: &OneBotAdapter) {
    timeout(Duration::from_secs(2), async {
        while !adapter.is_connected() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn receive<S>(peer: &mut WebSocketStream<S>) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    timeout(Duration::from_secs(3), async {
        loop {
            match peer.next().await.unwrap().unwrap() {
                WsMessage::Text(raw) => return serde_json::from_str(&raw).unwrap(),
                WsMessage::Ping(_) => peer.flush().await.unwrap(),
                other => panic!("unexpected frame: {other:?}"),
            }
        }
    })
    .await
    .unwrap()
}

/// Each fixture is specified in wire terms, independently from the client's Rust types.
async fn serve(mut peer: Peer, cases: Vec<(&str, Value, Value)>) -> Peer {
    for (action, params, data) in cases {
        let request = receive(&mut peer).await;
        assert_eq!(request["action"], action);
        assert_eq!(request["params"], params, "wrong params for {action}");
        peer.send(WsMessage::Text(
            json!({"status":"ok","retcode":0,"data":data,"echo":request["echo"]}).to_string(),
        ))
        .await
        .unwrap();
    }
    peer
}

fn member() -> Value {
    json!({"group_id":20,"user_id":30,"nickname":"member","card":"card","sex":"unknown","age":0,
        "join_time":1,"last_sent_time":2,"level":"1","role":"member","unfriendly":false,"card_changeable":true})
}

#[tokio::test]
async fn account_group_and_media_queries_decode_typed_results() {
    let (adapter, peer) = reverse().await;
    let client = adapter.client();
    let group = json!({"group_id":20,"group_name":"group","member_count":2,"max_member_count":200});
    let server = tokio::spawn(serve(
        peer,
        vec![
            (
                "get_login_info",
                json!({}),
                json!({"user_id":9007199254740993_i64,"nickname":"bot"}),
            ),
            (
                "get_stranger_info",
                json!({"user_id":30,"no_cache":true}),
                json!({"user_id":30,"nickname":"user","sex":"unknown","age":0}),
            ),
            (
                "get_friend_list",
                json!({}),
                json!([{"user_id":30,"nickname":"user","remark":"friend"}]),
            ),
            (
                "get_group_info",
                json!({"group_id":20,"no_cache":true}),
                group.clone(),
            ),
            ("get_group_list", json!({}), json!([group])),
            (
                "get_group_member_info",
                json!({"group_id":20,"user_id":30,"no_cache":false}),
                member(),
            ),
            (
                "get_group_member_list",
                json!({"group_id":20}),
                json!([member()]),
            ),
            (
                "get_record",
                json!({"file":"voice.silk","out_format":"mp3"}),
                json!({"file":"/remote/voice.mp3"}),
            ),
            (
                "get_image",
                json!({"file":"image.jpg"}),
                json!({"file":"/remote/image.jpg"}),
            ),
            ("can_send_image", json!({}), json!({"yes":true})),
            ("can_send_record", json!({}), json!({"yes":false})),
            (
                "get_status",
                json!({}),
                json!({"online":null,"good":false,"implementation_detail":{"state":2}}),
            ),
            (
                "get_version_info",
                json!({}),
                json!({"app_name":"fake","app_version":"1","protocol_version":"v11","build":123}),
            ),
        ],
    ));
    assert_eq!(
        client.get_login_info().await.unwrap().user_id,
        9007199254740993
    );
    assert_eq!(
        client.get_stranger_info(30, true).await.unwrap().nickname,
        "user"
    );
    assert_eq!(client.get_friend_list().await.unwrap()[0].remark, "friend");
    assert_eq!(
        client.get_group_info(20, true).await.unwrap().member_count,
        2
    );
    assert_eq!(client.get_group_list().await.unwrap()[0].group_id, 20);
    assert_eq!(
        client
            .get_group_member_info(20, 30, false)
            .await
            .unwrap()
            .area,
        None
    );
    assert_eq!(
        client.get_group_member_list(20).await.unwrap()[0].title,
        None
    );
    assert_eq!(
        client.get_record("voice.silk", "mp3").await.unwrap().file,
        "/remote/voice.mp3"
    );
    assert_eq!(
        client.get_image("image.jpg").await.unwrap().file,
        "/remote/image.jpg"
    );
    assert!(client.can_send_image().await.unwrap().yes);
    assert!(!client.can_send_record().await.unwrap().yes);
    let status = client.get_status().await.unwrap();
    assert_eq!(status.online, None);
    assert_eq!(status.extras["implementation_detail"]["state"], 2);
    assert_eq!(
        client.get_version_info().await.unwrap().extras["build"],
        123
    );
    let _peer = server.await.unwrap();
    adapter.stop().await.unwrap();
}

#[tokio::test]
async fn messages_send_query_and_recall_use_standard_wire_fields() {
    let (adapter, peer) = reverse().await;
    let client = adapter.client();
    let segments = vec![MessageSegment {
        kind: "text".into(),
        data: json!({"text":"hello"}).as_object().unwrap().clone(),
    }];
    let message = Message::Segments(segments.clone());
    let forward = json!([{"type":"node","data":{"user_id":"30","nickname":"user","content":[{"type":"text","data":{"text":"quoted"}}]}}]);
    let server = tokio::spawn(serve(
        peer,
        vec![
            (
                "send_private_msg",
                json!({"user_id":30,"message":"[CQ:face,id=1]","auto_escape":true}),
                json!({"message_id":-1}),
            ),
            (
                "send_group_msg",
                json!({"group_id":20,"message":segments,"auto_escape":false}),
                json!({"message_id":-2}),
            ),
            (
                "get_msg",
                json!({"message_id":-2}),
                json!({"time":1,"message_type":"group","message_id":-2,"real_id":4,"sender":{},"message":[{"type":"text","data":{"text":"hello"}}]}),
            ),
            (
                "get_forward_msg",
                json!({"id":"forward-1"}),
                json!({"message":forward}),
            ),
            ("delete_msg", json!({"message_id":-2}), Value::Null),
        ],
    ));
    assert_eq!(
        client
            .send_private_msg(30, &Message::Text("[CQ:face,id=1]".into()), true)
            .await
            .unwrap()
            .message_id,
        -1
    );
    assert_eq!(
        client
            .send_group_msg(20, &message, false)
            .await
            .unwrap()
            .message_id,
        -2
    );
    let result = client.get_msg(-2).await.unwrap();
    assert_eq!(result.message, message);
    assert_eq!(result.sender.user_id, None);
    assert_eq!(
        client.get_forward_msg("forward-1").await.unwrap().message[0].kind,
        "node"
    );
    client.delete_msg(-2).await.unwrap();
    let _peer = server.await.unwrap();
    adapter.stop().await.unwrap();
}

#[tokio::test]
async fn group_management_and_request_actions_accept_empty_results() {
    let (adapter, peer) = reverse().await;
    let client = adapter.client();
    let server = tokio::spawn(serve(
        peer,
        vec![
            ("send_like", json!({"user_id":30,"times":2}), Value::Null),
            (
                "set_group_kick",
                json!({"group_id":20,"user_id":30,"reject_add_request":true}),
                Value::Null,
            ),
            (
                "set_group_ban",
                json!({"group_id":20,"user_id":30,"duration":0}),
                Value::Null,
            ),
            (
                "set_group_whole_ban",
                json!({"group_id":20,"enable":false}),
                Value::Null,
            ),
            (
                "set_group_admin",
                json!({"group_id":20,"user_id":30,"enable":false}),
                Value::Null,
            ),
            (
                "set_group_card",
                json!({"group_id":20,"user_id":30,"card":""}),
                Value::Null,
            ),
            (
                "set_group_name",
                json!({"group_id":20,"group_name":"新群名"}),
                Value::Null,
            ),
            (
                "set_group_leave",
                json!({"group_id":20,"is_dismiss":false}),
                Value::Null,
            ),
            (
                "set_group_special_title",
                json!({"group_id":20,"user_id":30,"special_title":"title","duration":-1}),
                Value::Null,
            ),
            (
                "set_friend_add_request",
                json!({"flag":"friend-flag","approve":true,"remark":"friend"}),
                Value::Null,
            ),
            (
                "set_group_add_request",
                json!({"flag":"group-flag","sub_type":"add","approve":false,"reason":"declined"}),
                Value::Null,
            ),
            (
                "set_group_add_request",
                json!({"flag":"invite-flag","sub_type":"invite","approve":true,"reason":""}),
                Value::Null,
            ),
        ],
    ));
    client.send_like(30, 2).await.unwrap();
    client.set_group_kick(20, 30, true).await.unwrap();
    client.set_group_ban(20, 30, 0).await.unwrap();
    client.set_group_whole_ban(20, false).await.unwrap();
    client.set_group_admin(20, 30, false).await.unwrap();
    client.set_group_card(20, 30, "").await.unwrap();
    client.set_group_name(20, "新群名").await.unwrap();
    client.set_group_leave(20, false).await.unwrap();
    client
        .set_group_special_title(20, 30, "title", -1)
        .await
        .unwrap();
    client
        .set_friend_add_request("friend-flag", true, "friend")
        .await
        .unwrap();
    client
        .set_group_add_request("group-flag", GroupRequestType::Add, false, "declined")
        .await
        .unwrap();
    client
        .set_group_add_request("invite-flag", GroupRequestType::Invite, true, "")
        .await
        .unwrap();
    let _peer = server.await.unwrap();
    adapter.stop().await.unwrap();
}

#[tokio::test]
async fn void_typed_and_extension_calls_distinguish_invalid_or_failed_responses() {
    let (adapter, mut peer) = reverse().await;
    let client = adapter.client();
    let server = tokio::spawn(async move {
        for mut response in [
            json!({"status":"ok","retcode":0}),
            json!({"status":"failed","retcode":100,"wording":"sensitive peer text"}),
            json!({"status":"async","retcode":1}),
            json!({"status":"ok","retcode":0,"data":null}),
            json!({"status":"ok","retcode":0,"data":{"user_id":"not a number","nickname":"bot"}}),
            json!({"status":"ok","retcode":0,"data":{"extension":true}}),
        ] {
            let request = receive(&mut peer).await;
            response["echo"] = request["echo"].clone();
            peer.send(WsMessage::Text(response.to_string()))
                .await
                .unwrap();
        }
        peer
    });
    client.delete_msg(1).await.unwrap(); // No data key is valid for a void action.
    let error = client.set_group_ban(20, 30, 60).await.unwrap_err();
    assert!(matches!(
        error,
        OneBotError::Api {
            retcode: Some(100),
            ..
        }
    ));
    assert!(!error.to_string().contains("sensitive"));
    assert!(matches!(
        client.delete_msg(1).await,
        Err(OneBotError::Api {
            retcode: Some(1),
            ..
        })
    ));
    assert!(matches!(
        client.get_login_info().await,
        Err(OneBotError::Payload { .. })
    ));
    assert!(matches!(
        client.get_login_info().await,
        Err(OneBotError::Payload { .. })
    ));
    let result: Value = client
        .call("implementation_extension", &json!({"flag":true}))
        .await
        .unwrap();
    assert_eq!(result["extension"], true);
    let _peer = server.await.unwrap();
    adapter.stop().await.unwrap();
    assert!(matches!(
        client.get_login_info().await,
        Err(OneBotError::NotConnected)
    ));
}

#[tokio::test]
async fn retained_client_uses_forward_reconnection_and_correlates_concurrent_queries() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let adapter = Arc::new(
        OneBotAdapter::new(OneBotConfig {
            enabled: true,
            ws_url: format!("ws://{}", listener.local_addr().unwrap()),
            ..Default::default()
        })
        .unwrap(),
    );
    let client = adapter.client(); // Obtain the handle before the first connection exists.
    assert!(matches!(
        client.get_login_info().await,
        Err(OneBotError::NotConnected)
    ));
    let (tx, _rx) = mpsc::channel(1);
    adapter.start(EventIngress::new(tx)).await.unwrap();
    let mut peer = tokio_tungstenite::accept_async(listener.accept().await.unwrap().0)
        .await
        .unwrap();
    wait_connected(&adapter).await;
    let login = tokio::spawn({
        let c = client.clone();
        async move { c.get_login_info().await }
    });
    let login_request = receive(&mut peer).await;
    let status = tokio::spawn({
        let c = client.clone();
        async move { c.get_status().await }
    });
    let status_request = receive(&mut peer).await;
    peer.send(WsMessage::Text(json!({"status":"ok","retcode":0,"data":{"online":true,"good":true},"echo":status_request["echo"]}).to_string())).await.unwrap();
    peer.send(WsMessage::Text(json!({"status":"ok","retcode":0,"data":{"user_id":100,"nickname":"bot"},"echo":login_request["echo"]}).to_string())).await.unwrap();
    assert!(status.await.unwrap().unwrap().good);
    assert_eq!(login.await.unwrap().unwrap().user_id, 100);
    let pending = tokio::spawn({
        let c = client.clone();
        async move { c.delete_msg(1).await }
    });
    receive(&mut peer).await;
    peer.close(None).await.unwrap();
    drop(peer);
    assert!(matches!(
        timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap(),
        Err(OneBotError::Transport(_))
    ));
    let stream = timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap()
        .0;
    let mut peer = tokio_tungstenite::accept_async(stream).await.unwrap();
    wait_connected(&adapter).await;
    let login = tokio::spawn({
        let c = client.clone();
        async move { c.get_login_info().await }
    });
    let request = receive(&mut peer).await;
    assert_eq!(request["action"], "get_login_info"); // The old delete must not be replayed.
    peer.send(WsMessage::Text(json!({"status":"ok","retcode":0,"data":{"user_id":100,"nickname":"reconnected"},"echo":request["echo"]}).to_string())).await.unwrap();
    assert_eq!(login.await.unwrap().unwrap().nickname, "reconnected");
    adapter.stop().await.unwrap();
}
