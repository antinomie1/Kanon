# Agent backends: builtin and deepseek-harness

`agent` is the shared selection boundary. `builtin` uses Kanon's local model loop;
`dsh` delegates to the independent `kanon-dsh` library and the external deepseek-harness runtime.
The native tool bridge and shared simulation path are connected; remaining integration work is listed below.

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
- In-conversation `RunAgent` can select DSH without requiring a builtin provider. Unsupported
  builtin overrides and private runs fail explicitly rather than falling back.
- Console completions accept `agent`, including native DSH SSE completion. Caller disconnect
  signals the remote owner to cancel; the reservation remains held through bounded cleanup.
- `/api/v1/agents/dsh/connection`, `/settings`, `/models` and `/sessions` expose native management.
  Session routes support snapshot/history, model/title changes, stop and archive. Settings writes
  pass DSH's revision unchanged. The UI places DSH under Agent, offers connection and native
  settings controls, and hides builtin model/persona controls for a DSH instance.
- DSH turns prepare the native bridge before prompt submission and expose the same permitted
  plugin, MCP, skill and native tools as builtin. Instance identity travels in a trusted execution
  scope, while events and tool requests keep the real DSH session id. Every invocation receives a
  fresh request id, including repeated plugin agent runs for the same inbound event.
- Group simulation selects either backend and shares message batching, participant context,
  deadlines, reply limits and the existing `conversation_say`/`wait`/`leave` implementations.
  A DSH turn contributes its mode and skill instructions to native prompt assembly; DSH owns the
  resulting journal and model context. Full platform simulation verification remains below.
- Feature-enabled IPC publishes an owner-only `core.agent-token` beside the bound endpoint for
  the optional native plugin. The credential rotates at startup, is read on every RPC, and is
  removed when that IPC server exits. Default builds do not publish a bridge or credential file.

## Remaining integration work

1. Verify console and private runs against a complete native SessionController deployment,
   including durable restart, archive/restore, attachment history and concurrent writes.
   Console and private runs now reuse the core bridge and instance tool selector. Private runs
   suppress plugin hooks, permit tool-less execution and native model references, and archive
   their native journal after completion or cancellation. DSH owns step limits; unsupported
   per-request limits fail explicitly. No builtin provider is substituted.
2. Verify shared group participation end to end with an isolated native DSH runtime and platform
   adapter under concurrent arrivals and interruptions, then improve fresh-message handling and quoting. Audit native complete
   prompt overrides and plugin request/response observation so no hook silently loses its effect.
3. Audit cross-platform routing identities, generation races, startup persona restoration for DSH,
   shutdown cancellation completion, archive visibility and legacy history projection against
   native persistence. No claim is made that the bounded cancellation owner fully solves shutdown.
4. Run both builds and all existing tests, then finish the requested final bug audit. Keep every
   source below 1000 lines and add no test functions. The three baseline notice assertions and
   duplicate async-trait attributes remain deliberately deferred to that final phase.

## Verification so far

- Refactor commit `96a93e4`: all scanned source files are below 1000 lines; 939 Rust test functions
  are unchanged in count and identity. Rust workspace check is clean. TypeScript has 39 passing
  tests, Python has 73, and WebUI type checking/build pass. Both locale catalogs retain all 856
  translations unchanged.
- Full Rust run after the refactor: 950 passed, 3 failed. The three failures in
  `notice_pipeline_test` also fail on the original `33d97a6` snapshot: their expected text omits
  the already-present conversation metadata prefix. Repair these existing assertions during the
  final bug phase, without undoing the context behavior or adding test functions.
- Feature-enabled workspace check passes. A temporary local wire probe exercised RPC envelopes,
  correlation failures, remote errors, snapshot isolation, durable reply collection, both inbox
  lanes and a turn stopped before submission. This is not a live DSH/model end-to-end verification.
- A duplicate `#[tonic::async_trait]` in the original supervisor host implementation was observed;
  remove it during the final cleanup after the integration behavior is settled.

- The independent `kanon-dsh` library passes its standalone check. Dependency trees confirm it
  is absent from the default `kanon`/`kanon-dev` build and present with `--features dsh`.
- The current backend work passes default product and feature-enabled workspace checks, the
  existing agent/chat/config transaction suites (13 tests), and WebUI type checking (zero errors
  or warnings). The local wire probe still passes after moving transport into the standalone crate.
- The full feature-enabled workspace run completed with 948 passed and 5 failed (24 ignored).
  Two new regressions (unknown-backend HTTP status and builtin graceful shutdown) were fixed;
  both affected suites then passed, 9/9. The remaining three failures are the pre-existing notice
  assertions described above. WebUI production build passes with its existing bundle-size notice.

- Shared executor: all 211 existing LLM tests pass, including prompt layout and tool failure paths.
  Seven affected core suites pass, 36 tests total; subsequent instance-scope verification also uses
  existing skill/hook suites. No test functions were added.
- The native plugin passes TypeScript checking and builds as a package. A temporary isolated probe
  runs the actual DSH agent loop and tool registry with a deterministic local model adapter, calls
  `kanon/prepare` through the actual native gateway, and verifies scoped dispatch, ordinary-session
  isolation, unowned prompt rejection and stale leases. This does not contact an external model
  or messaging platform and does not establish complete production end-to-end coverage.

- A second temporary probe connects the actual Rust ingress and authenticated core IPC service to
  DSH's real native loop/tool registry through a local protocol shim and deterministic model.
  It verifies native tool dispatch, explicit simulation speech, quote target preservation, silent
  internal final text, no builtin session records, and credential-file cleanup. This exposed and
  fixed empty-journal cursor handling (`-1`, followed by event `0`) and the simulation guard's
  builtin-only session comparison. Both backends now share one session-routing helper.
- The latest existing policy/instance suites pass 31 tests. Default and all-feature workspace
  checks are clean; the bridge plugin builds and typechecks. Repository test counts are unchanged.

- Console/private integration: the existing API chat/agent routes and core plugin-agent suites
  pass 19 tests. A temporary Rust/native-loop probe also covers console tool permissions,
  tool-less turns, private instructions, native model-selection requests and session archival.
  Its protocol shim does not establish complete native persistence coverage. Default and
  all-feature checks and WebUI type checking/build are clean apart from the existing bundle-size
  advisory. No test functions were added, and all source files remain below 1000 lines.
- The console lists DSH's own sessions, pages native history at one immutable cursor, selects
  native models, changes titles, stops work and archives restorable journals. DSH console
  conversations can be resumed from that list. Their IDs carry only a routing target; no model,
  memory, context or settings are persisted in builtin SessionStore. Human history excludes
  model-only replacement copies, following DSH's native transcript convention.
