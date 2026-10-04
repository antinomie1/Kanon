# Agent backends: builtin and deepseek-harness

`agent` is the shared selection boundary. `builtin` uses Kanon's local model loop;
`dsh` delegates to the independent `kanon-dsh` library and the external deepseek-harness runtime.
The optional backend shares Kanon's tool executor and simulation pipeline while retaining native DSH state.

## Ownership and build contract

- Default product builds contain the built-in agent and do not compile `kanon-dsh`.
- `cargo check -p kanon-dsh` checks the standalone client library. It has no Kanon crate
  dependencies, no executable, and no builtin model, persona, session or memory implementation.
- `cargo build -p kanon --features dsh` includes the optional backend. `kanon-dev` has the same
  explicit feature; the API/core crates forward it to the LLM crate.
- DSH is a separate agent runtime, not an OpenAI-compatible provider for the built-in tool loop.
- Kanon owns platform ingress, outbound delivery, access policy and the instance-to-chat route.
  DSH owns its settings, model selection, durable sessions, history, memory and compaction.
- Connection settings come from the node configuration file. No Kanon configuration comes from
  DSH environment variables, and the node must not install or launch DSH implicitly.
- Selecting DSH must never fall back to the built-in agent after a connection or configuration
  failure. A build without the feature must reject an explicit DSH selection.
- Only routing identity may be retained locally. DSH history and model/session metadata must not
  be shadowed in the built-in `SessionStore` or replayed into a second model loop.

## Protocol boundary

The implementation follows DSH's slash RPCs and `/api/remote.mux` session streams. Protocol shapes
were inspected in the installed `@deepseek-ai/dsh` 0.1.7-rc.2 packages and compared with
[qq-bridge](https://github.com/Derpyu520/qq-bridge)'s current DSH integration.

The client uses a configured HTTP(S) origin and an optional file containing the Cookie header
issued by DSH. Credentials are read lazily and never sent through redirects. Unary responses
require matching RPC ids; stream frames and response bodies are bounded. Snapshot history is
separate from new events, and prompt completion is correlated through `user/message.source.rpcId`.
Stopping a session removes its pending inbox items before cancelling active work: cancellation
alone leaves queued work alive. Archive uses DSH's restorable archive operation, not deletion of
Kanon's unrelated session database.

The native DSH MCP client does not forward its agent/session identity to MCP tools. The optional
[DSH plugin](../integrations/dsh/README.md) uses the actual execution agent and a per-turn capability
lease over the core's existing authenticated gRPC endpoint. Preparation is required before prompt
submission. The plugin checks the real prompt source and rejects unrelated native UI prompts in a
platform-owned session. Model-supplied arguments never choose the session, caller or platform event.

`agent::tool_execution::TurnTools` owns one captured tool catalog and the shared execution path.
Builtin and DSH use the same argument checks, dispatch targets, plugin/native calls, lifecycle hooks,
attachment handling and errors. Only each runtime's own loop records history. DSH's native plugin
restricts inherited tools to preserve Kanon's instance policy, then registers the authorized Kanon
surface in `agent.ctx`; ordinary DSH sessions retain their native tool environment. The packaged
plugin shares DSH peer dependencies with its host instead of instantiating a second scope registry.

## Connected paths

- Feature-gated agent catalog and configuration publication. A binary without `dsh` rejects a
  configured DSH backend. Preparing a client does not connect or read a credential.
- Ingress selects the backend before builtin model lookup. DSH receives only
  current user input and transport context. Shared filtering, reply decoration, delivery,
  lifecycle events and local routing locks remain in the pipeline.
- `/new`, `/ls`, `/switch`, `/del` and `/model` operate on remote sessions for DSH instances.
  `/del` reports archive rather than claiming irreversible deletion. Local generation pointers
  are separately namespaced for builtin and DSH; remote history is never written to SessionStore.
- Plugin conversation reads use a bounded, read-only text projection of the remote journal.
  Native journal records remain available through the paginated agent API. Completed builtin
  turn import is explicitly rejected because DSH exposes no equivalent append contract.
- In-conversation `RunAgent` can select DSH without requiring a builtin provider. Private runs support native
  model references, instructions and tool-less execution; unsupported builtin overrides fail explicitly.
- Console completions accept `agent`, including native DSH SSE completion. Caller disconnect
  signals the remote owner to cancel; the reservation remains held through bounded cleanup.
- `/api/v1/agents/dsh/connection`, `/settings`, `/models` and `/sessions` expose native management.
  Session routes support snapshot/history, attachments, model/title changes, stop, archive and restore. Settings writes
  pass DSH's revision unchanged. The UI places DSH under Agent, offers connection and native
  settings controls, and hides builtin model/persona controls for a DSH instance.
- DSH turns prepare the native bridge before prompt submission and expose the same permitted
  plugin, MCP, skill and native tools as builtin. Instance identity travels in a trusted execution
  scope, while events and tool requests keep the real DSH session id. Every invocation receives a
  fresh request id, including repeated plugin agent runs for the same inbound event.
- Group simulation selects either backend and shares message batching, participant context,
  deadlines, reply limits and the existing `conversation_say`/`wait`/`leave` implementations.
  A DSH turn contributes its mode and skill instructions to native prompt assembly; DSH owns the
  resulting journal and model context. Native complete prompts retain Kanon policy through DSH's
  context snapshots; suppressing both system and runtime contributions fails explicitly.
- Feature-enabled IPC publishes an owner-only `core.agent-token` beside the bound endpoint for
  the optional native plugin. The credential rotates at startup, is read on every RPC, and is
  removed when that IPC server exits. Default builds do not publish a bridge or credential file.

## Lifecycle and transcript rules

A bounded remote owner holds the session reservation from creation through preparation, prompt
consumption and cleanup. Caller disconnect signals cancellation without abandoning that owner.
An in-flight preparation settles before retirement, preventing a late model/create commit from
racing archive. Private runs archive their journal after preparation failure, completion or stop.
Input conversion and native model reference validation precede remote creation.

Archive membership is read from the native Workspace baseline, because `session/list` includes
retired journals. Archives remain visible in the native UI and reserve their generation numbers.
Chat management operations share a fail-fast routing lock; publication checks its instance snapshot
under the catalog write lock. Startup restores builtin personas only for builtin instances.

Human history retains original append messages and excludes model-only context replacement copies.
Neither the legacy text projection nor the console transcript is used as model context. Images are
read through DSH's session-authorized attachment endpoint, using the durable `attachmentId`.

Kanon's static prompt hooks operate on its mode/skill contribution. DSH owns the complete persona,
model request, response and compaction hooks. Tool lifecycle hooks and pipeline reply events remain
shared. Native frozen model requests are never replaced with a builtin context approximation.

## Verification

- All source files remain below 1000 lines after the module splits. No repository test functions
  were added. Default and all-feature workspace checks pass without compiler warnings.
- A complete temporary native deployment runs SessionController, WorkspaceController, JSONL
  persistence, SQLite session queries, attachment storage, browser authentication, the actual
  agent loop/tool registry and the HTTP/WebSocket gateway. A deterministic local model adapter
  connects to the actual Rust ingress, authenticated gRPC bridge and delivery pipeline.
- That deployment verifies durable cold reads and restart continuation, console tool permissions,
  tool-less and private runs, native model selection, complete prompt contributions, archive and
  restore, durable image bytes, foreign-session attachment rejection and absence of builtin history.
- Failure probes cover preparation rejection, stop during preparation, caller disconnect and
  reservation retention until cleanup. Group probes deliver a second sender while the first
  model call runs, then check fresh-message delivery and exact quote targets. Only explicit speech
  tools reach the adapter; internal final text stays private.
- Existing API agent/chat, core conversation/instance and plugin-agent suites pass. The shared
  executor previously passed all 211 LLM tests, including stable prompt layout. TypeScript SDK
  tests and native bridge checking/build pass; WebUI checking reports zero errors or warnings,
  and its production build retains the existing bundle-size advisory.

The native probes do not contact external models or real IM platforms. Final workspace tests and
the general bug audit remain in progress, including the three baseline notice assertions and
cross-platform assistant routing. Windows runtime behavior requires execution on Windows.
