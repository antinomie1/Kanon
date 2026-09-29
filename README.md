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
- **Plugin-owned persistence** — plugins read and write their own `./data/plugins/<id>/`, so the core
  never becomes a data proxy.
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
  Each Python/TypeScript plugin declares its packages in its own `pyproject.toml` / `package.json`
  and is installed in its own directory (`uv sync`, `npm install` or `bun install`); Kanon never
  installs packages. A missing runtime or environment marks the affected plugin unavailable; it
  never blocks the node.

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
| `instances.json` | bot instances and their `/new` session generations |
| `personas.json` | the personas you add in the console (the built-in base assistant is not stored) |
| `sessions.db` | conversation history, compaction summaries and session records |

To deploy a preconfigured node (a container image, CI), ship a prepared `data/system.json`.

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

## Guarded Bash tool

The node registers `bash` as a native tool alongside `read_skill`. In **Tools**, configure who may
ask the AI to execute it. Kanon has no node-wide administrator role, so this policy belongs only to
Bash: `allowlist` permits listed senders; `denylist` permits every identified sender except those
listed. Denial always wins. The default empty allowlist denies everyone.

The policy is stored in `data/system.json` under `bash_policy` and can also be read or updated via
`GET`/`PUT /api/v1/tools/bash/policy`:

```json
{
  "bash_policy": {
    "mode": "allowlist",
    "allowlist": [{ "platform": "onebot", "user_id": "123456" }],
    "denylist": []
  }
}
```

Identity comes from the current inbound event before plugin filtering. A session name, quoted
message, model argument or claimed identity cannot grant access. Overlapping turns in a group have
separate caller scopes. Console chat and plugin-originated LLM requests have no verified sender and
are denied, even in denylist mode. The tool definition remains in the model's fixed tool list for
every sender; the host appends its current availability **after history at the request tail**.
Execution rechecks the live policy regardless of what the model requests.

Commands use a conservative static subset of Bash: quoted literal arguments, pipelines, `&&`, `||`,
semicolon-separated lists and newlines. The default executable allowlist is `echo`, `printf`, `pwd`,
`ls`, `cat`, `head`, `tail`, `wc`, `grep`, `rg`, `cut`, `tr`, `du`, `df`, `uname`, `whoami`, `id`, `ps`,
`uptime`, `sleep`, `seq`, `true`, `false`, and Git `status`/`diff`/`log`/`show`/`ls-files`/`rev-parse`.
For example: `ls -la | head -n 20` or `git status --short`. Git pagers, fsmonitor, diff/signature helpers and
ripgrep subprocess helpers are disabled. Programs are resolved only from trusted system directories.
Git accepts an explicit set of common diagnostic flags; unknown options are also rejected.

`rm`, `dd`, `sudo`, permission/ownership changes, disk formatting, arbitrary executables, scripts,
interpreters, launchers, assignments, shell evaluation, expansions, redirection and background jobs
are blocked before **any** command in the request starts. Quote glob/regex characters as literals;
globbing is unavailable. There is no model-supplied safety override. `cwd` defaults to `.` and must
resolve inside the node workspace, including through symlinks. Each call has a 15-second default
timeout (1–120 seconds), a four-process concurrency limit, no stdin, a clean child environment, and
at most 64 KiB retained per stdout/stderr stream. Timeout and cancellation kill the whole process
group. Results report stdout, stderr, exit code, timeout and truncation; nonzero exits are tool
failures. Bash execution currently requires a Unix host with Bash installed.

This is a diagnostic command guard, not an OS sandbox: allowed commands retain the node account's
read access, including paths outside `cwd`. Operators should use an appropriately restricted account
or container when granting access to untrusted users.

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

## License

GPL-3.0-only — see [LICENSE](LICENSE).
