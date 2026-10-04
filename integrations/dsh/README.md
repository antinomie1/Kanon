# Native deepseek-harness bridge

This optional DSH plugin connects a Kanon instance's tools and conversation participation to
DSH's native agent loop. Kanon keeps platform permissions and delivery; DSH owns models,
settings, sessions, durable history, context assembly and compaction.

Build Kanon with `cargo build -p kanon --features dsh`. Default builds contain no DSH client.
The plugin targets DSH **0.1.7-rc.2**; install it into the same DSH profile as its peer packages.
Do not load a checkout with a separate copy of DSH's services: their scope registries must be
shared with the host.

From this directory, run `bun install --frozen-lockfile`, `bun run check`, `bun run build`, then
`npm pack --ignore-scripts`. Install the resulting package using the DSH profile's native
package manager. The generated package includes the authoritative Protobuf schemas and the
shared TypeScript SDK codec; no second hand-maintained wire model is used.

Add a loader entry in the DSH profile configuration:

```yaml
- id: kanon-bridge
  name: '@kanon/dsh-bridge'
  config:
    coreSocket: './run/core.sock'
    tokenFile: './run/core.agent-token'
    timeoutSeconds: 600
```

Resolve these paths relative to the DSH process working directory. Point them at Kanon's actual
configured runtime directory, which may be the OS runtime directory rather than `./run`.
Kanon publishes `core.agent-token` with owner-only access beside its bound IPC endpoint only in
a DSH-enabled build, rotates it each start, and removes it at server shutdown. The plugin rereads
it for each call. On Windows, `coreSocket` is Kanon's loopback address file and `tokenFile` is
required. No connection settings come from environment variables.

Configure Kanon's DSH connection in Agent settings and select `dsh` on an instance. Before
submitting each prompt, Kanon calls the native `kanon/prepare` endpoint. The plugin verifies
current turn ownership over gRPC, installs that turn's tool catalog in `agent.ctx`, and returns
the exact capability lease. Missing plugins, busy sessions and expired leases fail explicitly.

The native plugin correlates incoming prompts using DSH's real message source and uses
`execution.agent.session.id` for tool calls. Session or caller identity is never a tool argument.
Platform sessions admit only the active Kanon prompt. Ordinary native DSH sessions are unaffected.
Kanon's platform turn tools replace inherited DSH tools so global shell, MCP or skill registrations
cannot bypass instance permissions. The native agent still owns the reasoning and tool loop.

Group simulation uses the same `conversation_say`, `conversation_wait` and `conversation_leave`
implementations as builtin. Tool attachments return through Kanon's delivery pipeline; local
file paths are not sent to DSH. Tool events, Bash caller scope and cancellation use the common
executor. The bridge retains only the active turn's routing, call identities and pending media.

Console chat, private plugin runs and full native model/session UI integration are still being
completed; see [the integration record](../../docs/AGENT_BACKENDS.md).
