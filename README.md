# Kanon

A high-performance, multi-platform chat bot **microkernel** written in Rust 2024, with first-class
**Rust / Python / TypeScript** plugin support.

The node distributes as a single self-contained native binary. Python and Node.js are never required
to run it: the kernel is pure Rust, and those runtimes are probed lazily, only when a plugin that
actually needs them is launched. A plugin that crashes, leaks or blocks cannot take the node down,
because by default every plugin runs in its own supervised child process (shared hosts exist only as
an explicit development mode).

## Highlights

- **Self-contained node** — `kanon` runs on a clean system with no interpreter installed.
- **Physical fault isolation** — plugins run out of process; a watchdog prunes and restarts crashed
  hosts instead of advertising a dead process as healthy.
- **Strict contracts over the wire** — all cross-process calls are gRPC (HTTP/2 + Protobuf). Rich
  media uses typed `oneof`, tool calling uses `google.protobuf.Struct`; no stringly-typed maps.
- **Lockstep-free ingress** — platform events are pushed onto a bounded queue and Fast-ACKed, so the
  pipeline never makes an IM platform wait on a model response.
- **Headless core, decoupled console** — the node exposes a REST + WebSocket control plane and the
  WebUI is an independent frontend that talks to it over HTTP.
- **Plugin persistence** — small state (counters, flags, tokens) goes into the core's key-value
  store `./data/kv.db`, one namespace per plugin; larger or relational data stays in the plugin's
  own `./data/plugins/<id>/`.
- **Conversations survive restarts** — history, summaries and session records live in
  `./data/sessions.db`, so after a restart (or after a bot instance is edited) the next message
  continues the same conversation. A database that cannot be opened stops startup instead of
  silently starting with amnesia.
- **Cache-friendly prompts** — every request is laid out static-first (tools, one system block,
  append-only history, then the current turn), and long conversations are compacted once, using the
  provider's cache, instead of being trimmed message by message. Cache hits are exported as metrics.

## Requirements

- A Rust 2024 edition toolchain (stable).
- Optional, and only for the plugins that need them: Python 3.10+ (with `uv`) and Node.js or Bun.
  Each Python/TypeScript plugin declares its packages in its own `pyproject.toml` / `package.json`;
  before starting it, the node runs the plugin's own tool in its directory (`uv sync`, `bun install`
  or `npm ci`) when its environment is missing or out of date. Set
  `startup.install_dependencies` to `false` in `data/system.json` to install them yourself. A
  missing runtime or a failed install marks the affected plugin unavailable; it never blocks the
  node.

## Quick start

```bash
cargo build --release      # builds exactly the two shipped executables
./target/release/kanon     # start the node
```

The management gateway listens on `127.0.0.1:8080` by default. The node reads no environment
variables: its whole configuration is `data/system.json`. Providers, models, policies and adapters
are edited in the console; the `startup` section is edited by hand and applies on the next start.
Every key is optional, and an unknown key stops startup instead of being ignored:

```json
{
  "startup": {
    "api_addr": "127.0.0.1:8080",
    "log": "info",
    "run_dir": "/run/kanon",
    "typescript_runtime": "/usr/bin/node"
  }
}
```

To send a model answer one line at a time, enable **Send each line separately** under Settings →
Reply, or override it in an instance's reply settings (`reply_policy.split_lines`). It defaults to
off. Empty and whitespace-only lines are skipped; code lines are split too. A quote appears only on
the first message, trailing images stay on the last, and the stored model answer remains intact.
An answer occupies one outbound queue slot regardless of its line count. QQ Official merges excess
lines into the last text message to leave room for attachments and a C2C typing acknowledgement;
guild replies stay in one text message to avoid a burst above the channel rate limit.

| Key | Default | Purpose |
| :--- | :--- | :--- |
| `api_addr` | `127.0.0.1:8080` | management gateway bind address (loopback only) |
| `log` | `info` | `tracing` filter directives |
| `run_dir` | platform runtime dir | where IPC sockets (`core.sock`, `host_<id>.sock`) are created |
| `typescript_runtime` | `bun`, then `node`, from `PATH` | interpreter for TypeScript plugins |

At runtime the node reads plugins from `./plugins`, keeps operator state in `./data/`, and creates its
IPC sockets under the platform runtime directory (`$XDG_RUNTIME_DIR/kanon/run/` on Linux). The state
files under `./data/`:

| File | Holds |
| :--- | :--- |
| `system.json` | provider endpoints (with credentials, mode `0600`), the model catalog, the **global default model**, reply and context policies, adapter settings |
| `instances.json` | bot instances and the current conversation of each chat |
| `personas.json` | the personas you add in the console (the built-in base assistant is not stored) |
| `sessions.db` | conversation history, compaction summaries and session records |
| `kv.db` | the plugins' key-value store |

To deploy a preconfigured node (a container image, CI), ship a prepared `data/system.json`.

For custom OpenAI-compatible endpoints that require `reasoning_content` in assistant history,
select **OpenAI Compatible + reasoning_content replay** (`protocol: "openai_reasoning"`).
The default `openai` mode omits this extension for other endpoints, while automatically retaining
it for `https://api.deepseek.com` (including `/v1`) so existing official DeepSeek configurations
keep working. This only controls history replay; it does not disable thinking or discard stored
reasoning. Choose the extension only when the endpoint documents support for it.

In **Models → provider settings → Replay historical reasoning**, `replay_reasoning` controls
whether compatible endpoints receive all assistant reasoning in the retained context, across user
turns. It defaults to `true` for new and older configurations to preserve supported-endpoint replay
(including DeepSeek thinking + tools). Ordinary OpenAI endpoints still omit the non-standard field.
Set `providers[].replay_reasoning` in `data/system.json`, or send it to `POST /api/v1/providers`;
omitting it from an update preserves the saved preference. Changes apply to the next conversation
request. Turning it off only changes temporary API messages: old reasoning and newly generated
reasoning stay in `sessions.db`, survive restart and can be replayed after re-enabling. Existing
history compaction and clearing policies still apply. This is independent of `send_reasoning`,
which controls display on chat platforms. Disabling replay can cause thinking-mode tool requests
to fail on endpoints that require it; see the [DeepSeek thinking-mode contract](https://api-docs.deepseek.com/zh-cn/guides/thinking_mode/).

Reasoning and answer text are distinguished by their source, following AstrBot's separate
[reasoning/text channels](https://github.com/AstrBotDevs/AstrBot/blob/9d4f523464644554e0e8e50fa2a65f146e320cd1/astrbot/core/agent/runners/tool_loop_agent_runner.py#L180-L200). An explicit `reasoning_content` (including an empty string or null) is authoritative:
`content` remains answer text, even if it prints `<think>` delimiters. Native streaming deltas follow
the same rule. Without an explicit channel, only a leading exact `<think>…</think>` envelope is
recognized for legacy compatibility. It must be a complete standard block; nested, repeated or
malformed model output gets no special recovery. Inline prose, Markdown code, lone closing tags and whitespace variants such as
`<think >` / `</think >` are not reclassified at platform delivery. A bare leading standard envelope
without a separate channel is inherently ambiguous; quote or fence a literal example in that case.
Unparsed tool markup is also left as answer text; only successfully parsed tool calls leave the
answer channel. Display never deletes stored reasoning or changes the replay preference.

## Console

The WebUI (`webui/`, built with Svelte 5 and served by the node) manages everything above:

- **Model Providers** — add endpoints (with connectivity tests that use the stored key server-side)
  and pick the one **global default model**. Providers are only endpoints; there is no "default
  provider". Instances may still override the model for themselves.
- **Personas** — add, edit and delete persona presets. A persona is fixed prompt text placed at the
  top of every request; only a minimal base assistant ships with the node.
- **Sessions** — tracked conversations with turn and token counters; bind a persona to one session
  or reset it.
- **Instances**, **Plugins & Adapters**, **Pipeline & Logs**, **Chat** and **System Settings** cover
  the rest of the node.

OneBot v11 can also be configured under **Plugins & Adapters → OneBot v11** in the console.
Both forward and reverse universal WebSockets are supported. See [the OneBot setup guide](docs/ONEBOT.md)
for connection examples, account binding, message support and the typed client covering 29 common APIs.

The QQ Official Bot adapter is built in as well: open **Plugins & Adapters → QQ 官方机器人** and either
scan the QR code with mobile QQ or enter the AppID and AppSecret. It answers group @-messages, C2C
chats, guild @-messages and guild DMs, and brings quoted messages (text, images, voice transcripts,
stickers, files) into the model's context.

## Bash tool: persistent container or local host

The operator switches Bash on and chooses the execution mode under **System Settings → Bash
tool**:

- **Persistent container** (default): reuse one managed container for the workspace across tool
  calls and node restarts. Normal completion leaves it running, including background processes.
- **Local host**: run native Bash with the Kanon account's host permissions. Docker is unnecessary.
  Optional **AI review before execution** is enabled by default for this mode.

Bash is off until the operator enables it, and then only the administrators listed by ID in the
serving instance's command policy may use it: the instance's own override (**Instances → edit →
Command permissions**), or the node's (**System Settings → Command permissions**,
`command_policy.admins` as `<platform>:<user id>`). Group owners and admins reported by a platform
never qualify: a shell on the node is granted by the operator, not by a group. Identity comes from
the original inbound event, never model arguments, message text or a session name. A notice (a
join or poke is not a request), console chat and plugin-originated LLM requests have no verified
sender and never run Bash. Permissions are rechecked after queueing and after review.

Each instance also chooses where its administrators may use Bash (**Instances → edit → Bash
tool**, `bash` in `data/instances.json`): `disabled`, `own_context` (default: private chats and
per-member group sessions of groups it does not observe) or `shared_context`. A shared or observed
group session carries other members' words, which could steer an administrator's shell, so it
needs the explicit `shared_context` opt-in; otherwise the model is told exactly which setting
refuses the turn.

The tool definition remains fixed for every sender. Availability and selected execution mode are
appended once inside the originating user message, before persistence, preserving multimodal parts
and the existing history/compaction prefix.

`data/system.json` stores the operator settings under `bash_policy`; the console uses
`GET`/`PUT /api/v1/tools/bash/policy`. For example:

```json
{
  "command_policy": { "admins": ["onebot:123456"] },
  "bash_policy": {
    "enabled": true,
    "execution_mode": "local",
    "local": {
      "working_dir": "./data/bash/workspace",
      "auto_review": true,
      "review_model": null
    },
    "sandbox": { "image": "kanon-bash-sandbox:2", "network": true }
  }
}
```

Local automatic review uses a separate request to the selected provider-qualified `review_model`,
or the node's default model when unset. It receives the exact command and canonical working directory,
with no tools or conversation history. Only a strict, explicit JSON approval starts execution.
Rejection, missing/invalid output, unavailable models or a 30-second review timeout deny the command.
Turning review off skips that model request; administrator checks and execution limits still apply.
Review is risk screening, not a sandbox or a guarantee about opaque scripts and files they load.
Local execution currently requires Unix Bash. The local starting directory defaults to the Bash
workspace `./data/bash/workspace`, away from `data/system.json` and the session database, and can
be changed by the operator.

The intended operating model is an authorized user working with a basically benign AI. Automatic
review helps catch accidental broad damage; the implementation does not try to classify every shell
syntax or defend against deliberately disguised commands. Normal Bash, Python, Node, scripts, package
managers, file cleanup, pipes and redirection are passed to Bash unchanged. There is no command
blacklist or custom shell parser. Basic input validation, administrator checks and runtime limits remain.
For local execution, enable automatic review when an extra mistake-prevention check is wanted.

For container mode, prepare the trusted runtime image separately:

```bash
docker build -t kanon-bash-sandbox:2 sandbox/bash
```

The image supplies Bash, Python 3.12, Node 24/npm, Git and ripgrep. The native Docker API client
requires a local Linux Docker daemon with seccomp and memory/CPU/PID controls. Docker unavailability
never silently switches container mode to host execution. The default endpoint is
`unix:///var/run/docker.sock` on Unix and `npipe:////./pipe/docker_engine` on Windows; remote TCP/SSH
endpoints are rejected.

The persistent container exposes only `./data/bash/workspace` at `/workspace`. Its HOME is
`/workspace/.home`, so user-installed packages and caches survive container resets as well as node
restarts. Container temporary files and background processes survive normal calls. Each call starts
a new Bash process: shell-local variables and `cd` do not carry into the next call; use `cwd`, scripts
or environment files for that state. All administrators and sessions share the workspace and its
container.

A stable workspace identity locates the container after a node restart. Calls are serialized within
that runtime. Image or isolation-setting changes require the explicit **Reset container** action
(`POST /api/v1/tools/bash/reset`); the tool refuses to silently discard the existing environment.
API changes to the Docker endpoint are refused while the old endpoint still owns a container; reset
it first. Reset before hand-editing the endpoint while the node is stopped as well.
Reset removes the container and its temporary state, preserving workspace/HOME files. There is no
idle expiry. On timeout, caller cancellation or abnormal termination, the container is restarted to
terminate detached children; this also stops background jobs and clears temporary state.

User code runs non-root with no capabilities and no-new-privileges. The system filesystem is read-only
and PID/IPC/cgroup namespaces stay private. Networking defaults to public IPv4 access, with private,
host and metadata ranges blocked except configured DNS; no ports are published. Operators can disable
networking. Default limits are 512 MiB RAM/swap, one CPU, 128 processes/threads, 512 MiB per file and
128 MiB temporary storage. Each execution has a 15-second default budget (1–120 seconds) and retains
at most 64 KiB per output stream. The writable workspace has no aggregate disk quota.

Custom images must preserve the trusted bootstrap/exec helpers and version-2 runtime contract.
The model cannot choose the backend, reviewer, image, mounts or resource limits. Existing workspace
ownership is not silently changed; root-run nodes should prepare existing workspace ownership for
UID/GID 65534. Container isolation relies on a trusted Docker daemon/image and shares its Linux kernel.
Use one active node per workspace; the container is a shared environment for its administrators.

Run the real-container integration tests after building the image:

```bash
cargo test -p kanon-core --test bash_tool_test --test bash_sandbox_test -- --ignored --test-threads=1
```

## Build outputs

A default build produces exactly two executables:

| Binary | Crate | Role |
| :--- | :--- | :--- |
| `kanon` | `crates/kanon` | the node: microkernel engine, process supervisor and management gateway |
| `kanon-dev` | `crates/kanon-dev` | developer CLI: scaffold, lint, pack and offline sandbox testing |

Every other crate is a library, and the example plugin hosts are fixtures rather than node binaries —
build them explicitly when you need to exercise the plugin IPC loop:

```bash
cargo build -p demo-weather -p demo-rust-plugin   # example plugin hosts (test fixtures)
cargo build --workspace                           # everything, including the fixtures
```

## Workspace at a glance

| Crate | Responsibility |
| :--- | :--- |
| `crates/kanon` | node entrypoint — assembles the libraries into a running process |
| `crates/kanon-core` | event loop, message pipeline, host supervisor, adapter contract |
| `crates/kanon-api` | REST + WebSocket management gateway |
| `crates/kanon-llm` | model gateway, static-first prompt layout, append-only session memory with cache-safe compaction, and the tool-calling state machine |
| `crates/kanon-transport` | cross-platform IPC (Unix sockets, authenticated loopback TCP) |
| `crates/kanon-storage` | embedded KV storage and data directory isolation |
| `crates/kanon-proto` | Protobuf/gRPC contract and generated stubs |
| `crates/kanon-adapter-milky` | Milky protocol platform adapter |
| `crates/kanon-adapter-onebot` | OneBot v11 forward/reverse WebSocket platform adapter |
| `crates/kanon-adapter-qqofficial` | QQ Official Bot (QQ Open Platform) platform adapter |
| `crates/kanon-dev` | developer CLI |
| `sdks/{rust,python,typescript}` | plugin SDKs and language hosts |
| `webui` | independent web console |

## Development

```bash
cargo check --workspace --all-targets   # must stay free of errors and warnings
cargo test --workspace                  # full suite; build the example plugin hosts first
```

`cargo build` and `cargo test` without `--workspace` cover only the two product binaries, because the
root manifest restricts `default-members` to them. Use `--workspace` for full-repository verification,
and build the example plugin hosts first when running the plugin-host tests. Project conventions are
documented in [AGENTS.md](AGENTS.md) and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
Plugin authors should start with [docs/PLUGIN_GUIDE.md](docs/PLUGIN_GUIDE.md); the protocol reference
is [docs/PLUGIN_API.md](docs/PLUGIN_API.md).

## License

GPL-3.0-only — see [LICENSE](LICENSE).
