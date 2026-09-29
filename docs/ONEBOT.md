# OneBot v11

Kanon includes a built-in OneBot v11 adapter for implementations such as NapCat and Lagrange.
It serves one account on the `onebot` platform using a **universal WebSocket**: events and API
requests share one connection. OneBot v12, HTTP webhooks and separate API/Event sockets are
not implemented.

## Configure in the console

1. Open **Plugins & Adapters**, then the **OneBot v11** configuration drawer.
2. Choose a connection direction, enter the WebSocket URL and the matching access token.
3. Enable and save. Saved settings live in the `onebot` section of `data/system.json` and are
   applied immediately to the running adapter.
4. Create or enable a bot instance bound to platform `onebot`. A connected adapter alone does not
   make the pipeline answer messages; the instance, reply policy and model/plugin configuration
   still determine responses.

The token is write-only: leaving the field blank preserves it; use **Clear token** to remove it.
The adapter starts disabled on a fresh node. Its platform identifier and display name cannot be
changed while it is registered; edit those in stored configuration and restart if needed.

## Forward WebSocket

The OneBot implementation listens; Kanon connects. Enable the implementation's forward universal
WebSocket server and use its address, normally `ws://127.0.0.1:6700/`. Do not use its `/api` or
`/event` endpoint, which each carry only one half of the protocol.

For a fresh deployment, writing the section into `data/system.json` before the first start is
equivalent to the console:

```json
{
  "onebot": {
    "enabled": true,
    "transport": "forward_websocket",
    "ws_url": "ws://127.0.0.1:6700/",
    "access_token": "replace-with-your-token"
  }
}
```

`wss://` is supported with normal certificate validation. Kanon sends
`Authorization: Bearer <token>` during the handshake. Credentials belong in the token setting,
not in the URL. Failed connections retry with backoff from 500 ms to 30 seconds.

## Reverse WebSocket

Kanon listens; the OneBot implementation connects. For example, configure Kanon with:

```json
{
  "onebot": {
    "enabled": true,
    "transport": "reverse_websocket",
    "ws_url": "ws://0.0.0.0:6701/onebot",
    "access_token": "replace-with-your-token"
  }
}
```

Configure the implementation's **reverse universal WebSocket** URL as
`ws://<kanon-host>:6701/onebot`, using the same token. `0.0.0.0` is a bind address, not the
destination to put in the remote client. Use `127.0.0.1` for same-machine-only connections.
The listener URL must name a literal IP address and a nonzero port. It is separate from the
management gateway, so do not reuse the port of `startup.api_addr`.

The client must send the standard `X-Client-Role: Universal` and positive `X-Self-ID` headers.
The configured path and bearer token are checked before upgrading. A second simultaneous client
is rejected; an account cannot replace an active connection. After disconnecting, the listener
waits for the implementation to reconnect. For TLS, terminate `wss://` at a reverse proxy and
forward to this `ws://` listener, preserving the path and headers. Set an access token when the
listener is reachable beyond a trusted local connection.

## Messages and status

- Private and group messages map to `private:<user_id>` and `group:<group_id>`. Outbound replies
  call `send_private_msg` or `send_group_msg`, with `echo` correlation and a 15-second API timeout.
- Incoming arrays and CQ strings support text, mentions, replies, images and voice. Other segments
  are preserved as `onebot.<type>` custom payloads. Media without a download URL stays custom;
  a OneBot cache filename is not treated as a URL on the Kanon host.
- Image/audio bytes and local Kanon files are sent as base64. URLs are sent directly. Custom
  `onebot.*` payloads follow the implementation's own wire semantics.
- Sender/account/message IDs, conversation kind, timestamp and bot mention facts accompany the
  message. Group replies follow Kanon's existing reply policy. A reply segment contains only the
  quoted message ID in v11, so a quote alone is not inferred to mention the bot.
- Notices, requests and meta events do not become conversational turns. This adapter does not
  automatically approve friend/group requests or retrieve quoted/forwarded message bodies.

`connected` means a universal socket is open, not proof that the QQ account can deliver a message.
Successful delivery requires an API response with `status: ok`, `retcode: 0`, and a message ID.
Missing acknowledgements fail explicitly with an unknown delivery outcome; the adapter never
replays an in-flight send after reconnecting. Core queue saturation rejects ingress without
blocking WebSocket events or API responses. Ping/pong checks detect dead sockets.

The management API exposes `GET` and `PUT /api/v1/adapters/onebot/config`. The response contains
`config` without the token and `status` with `connection_state`, `connected`, `self_id`,
`token_configured` and `last_error`. Reverse mode reports `listening` until a client connects.
Refresh the console to obtain the current status.

## Common API client

`OneBotAdapter::client()` returns a cloneable `OneBotClient` for in-process Rust callers, following
the Milky client's typed-call model. It shares the existing forward or reverse connection, and a
retained handle resolves the current session on each new call. The pipeline's outbound delivery
also uses this client, so there is only one API request/response path.

The client wraps these 29 standard actions with typed arguments and response structs from
`kanon_adapter_onebot::protocol`:

| Category | Methods |
| :--- | :--- |
| Messages | `send_private_msg`, `send_group_msg`, `delete_msg`, `get_msg`, `get_forward_msg` |
| Account and friends | `get_login_info`, `get_stranger_info`, `get_friend_list`, `send_like` |
| Groups and members | `get_group_info`, `get_group_list`, `get_group_member_info`, `get_group_member_list` |
| Group management | `set_group_kick`, `set_group_ban`, `set_group_whole_ban`, `set_group_admin`, `set_group_card`, `set_group_name`, `set_group_leave`, `set_group_special_title` |
| Requests | `set_friend_add_request`, `set_group_add_request` |
| Media | `get_record`, `get_image`, `can_send_image`, `can_send_record` |
| Implementation | `get_status`, `get_version_info` |

```rust,no_run
use kanon_adapter_onebot::{OneBotAdapter, OneBotError};

async fn inspect_account(adapter: &OneBotAdapter) -> Result<(), OneBotError> {
    let client = adapter.client();
    let login = client.get_login_info().await?;
    let groups = client.get_group_list().await?;
    println!("{} belongs to {} groups", login.nickname, groups.len());
    Ok(())
}
```

Methods take explicit flags, such as `no_cache`, `approve` and `enable`. Message arguments support
CQ strings or arrays through `protocol::Message`; `auto_escape` controls CQ parsing for strings.
The generic `call::<_, Response>(action, &params)` and `call_void(action, &params)` methods support
additional implementation APIs over the same transport. Typed calls require valid response data;
void calls accept null or omitted `data` when the envelope reports `status: ok`, `retcode: 0`.
An `async` acknowledgement is not treated as completed success. Errors preserve the API action
and numeric return code without relaying arbitrary peer wording. Calls still time out after
15 seconds and are never replayed after disconnection.

These wrappers perform an operation only when explicitly called. They do not add automatic request
approval, notification dispatch, a group-management UI or cross-process plugin RPCs. Request
handling requires the original event's flag and subtype supplied by the caller. `get_msg` and
`get_forward_msg` are explicit queries, not automatic enrichment of every incoming message.
`get_image` and `get_record` return paths on the OneBot host; Kanon does not read those paths locally.
Implementation support and account permissions determine whether a particular API succeeds.

## Protocol references and verification

The adapter follows the official OneBot v11 specifications for
[forward WebSockets](https://github.com/botuniverse/onebot-11/blob/master/communication/ws.md),
[reverse WebSockets](https://github.com/botuniverse/onebot-11/blob/master/communication/ws-reverse.md),
[authentication](https://github.com/botuniverse/onebot-11/blob/master/communication/authorization.md)
and [message segments](https://github.com/botuniverse/onebot-11/blob/master/message/segment.md).
The typed client follows the [public API contracts](https://github.com/botuniverse/onebot-11/blob/master/api/public.md).

Run `cargo test -p kanon-adapter-onebot` and
`cargo test -p kanon-api --test onebot_routes_test` to verify mapping, real loopback WebSocket
exchanges, all 29 client action contracts and management persistence. These tests use simulated protocol peers; an actual
NapCat/Lagrange account still needs deployment-specific end-to-end testing.
