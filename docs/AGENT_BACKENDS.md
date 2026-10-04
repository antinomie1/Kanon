# Agent backends: builtin and deepseek-harness

`agent` is the shared selection boundary. `builtin` uses Kanon's local model loop;
`dsh` delegates to the independent `kanon-dsh` library and the external deepseek-harness runtime.
Integration is in progress; the bridge and simulation work below are not complete.

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

The native DSH MCP client does not forward its agent/session identity to MCP tools. A bridge
plugin must use the actual execution session identity when calling Kanon. Model-supplied session
ids alone are insufficient to route concurrent chats safely.

## Connected paths

- Feature-gated agent catalog and configuration publication. A binary without `dsh` rejects a
  configured DSH backend. Preparing a client does not connect or read a credential.
- Assistant-mode ingress selects the backend before builtin model lookup. DSH receives only
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

## Remaining integration work

1. Connect a DSH bridge plugin to the existing Kanon tool, plugin, MCP, skill and platform paths;
   retain per-instance access rules, cancellation, attachments and observability.
2. Reuse group participation machinery for both backends. DSH simulation currently fails
   explicitly until its native tool bridge is connected, instead of sending conventional replies.
   Improve interruption, fresh-message handling, quoting and turn ownership without replaying
   stale group history or assistant text.
3. Complete backend-aware console session/model views and private plugin agent runs, including
   remote settings and tool policy semantics. Audit cross-platform routing identities, generation
   races, private-run request correlation, shutdown cancellation completion and native archive
   visibility against a live DSH deployment.
4. Verify remote tools, reconnects, settings, media and cancellation with an isolated native DSH
   runtime. Run both builds and the existing tests, then finish the requested final bug audit.
   Keep every source below 1000 lines and add no test functions.

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
