# OneBot v11

Kanon includes a built-in OneBot v11 adapter for implementations such as NapCat and Lagrange.
It serves one account on the `onebot` platform using a **universal WebSocket**: events and API
requests share one connection. OneBot v12, HTTP webhooks and separate API/Event sockets are
not implemented.

## Configure in the console

1. Open **Plugins & Adapters**, then the **OneBot v11** configuration drawer.
2. Choose a connection direction, enter the WebSocket URL and the matching access token.
3. Enable and save. Saved settings live in `data/system.json` and take precedence over environment
   variables at the next startup. Saving applies them immediately to the running adapter.
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

For a fresh deployment, environment configuration is equivalent to the console:

```sh
KANON_ONEBOT_WS_URL=ws://127.0.0.1:6700/ \
KANON_ONEBOT_TRANSPORT=forward_websocket \
KANON_ONEBOT_TOKEN=replace-with-your-token \
./target/release/kanon
```

`wss://` is supported with normal certificate validation. Kanon sends
`Authorization: Bearer <token>` during the handshake. Credentials belong in the token setting,
not in the URL. Failed connections retry with backoff from 500 ms to 30 seconds.

## Reverse WebSocket

Kanon listens; the OneBot implementation connects. For example, configure Kanon with:

```sh
KANON_ONEBOT_WS_URL=ws://0.0.0.0:6701/onebot \
KANON_ONEBOT_TRANSPORT=reverse_websocket \
KANON_ONEBOT_TOKEN=replace-with-your-token \
./target/release/kanon
```

Configure the implementation's **reverse universal WebSocket** URL as
`ws://<kanon-host>:6701/onebot`, using the same token. `0.0.0.0` is a bind address, not the
destination to put in the remote client. Use `127.0.0.1` for same-machine-only connections.
The listener URL must name a literal IP address and a nonzero port. It is separate from the
management gateway, so do not reuse `KANON_API_ADDR`'s port.

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

## Protocol references and verification

The adapter follows the official OneBot v11 specifications for
[forward WebSockets](https://github.com/botuniverse/onebot-11/blob/master/communication/ws.md),
[reverse WebSockets](https://github.com/botuniverse/onebot-11/blob/master/communication/ws-reverse.md),
[authentication](https://github.com/botuniverse/onebot-11/blob/master/communication/authorization.md)
and [message segments](https://github.com/botuniverse/onebot-11/blob/master/message/segment.md).

Run `cargo test -p kanon-adapter-onebot` and
`cargo test -p kanon-api --test onebot_routes_test` to verify mapping, real loopback WebSocket
exchanges and management persistence. These tests use simulated protocol peers; an actual
NapCat/Lagrange account still needs deployment-specific end-to-end testing.
