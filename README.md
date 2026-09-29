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

## Sandboxed Bash tool

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
    "denylist": [],
    "sandbox": { "network": true }
  }
}
```

Identity comes from the current inbound event before plugin filtering. A session name, quoted
message, model argument or claimed identity cannot grant access. Overlapping turns in a group have
separate caller scopes. Console chat and plugin-originated LLM requests have no verified sender and
are denied, even in denylist mode. The tool definition remains in the model's fixed tool list for
every sender; the host appends availability **inside the originating user message**, before it is
stored. Tool-loop requests and compaction reuse that history without adding synthetic user turns.
Execution rechecks the live policy regardless of what the model requests.

Commands run with normal Bash semantics, including **Python, Node, scripts, assignments, loops,
expansions, globbing, pipes and redirection**. There is no executable or Git-option allowlist. The
lightweight guard checks recognizable static command heads before execution: `rm`, `rmdir`, `dd`,
`sudo`/`su`/`doas`, disk formatting/partitioning/wiping, mounting, shutdown/reboot and `killall`.
It also rejects `find -delete`, destructive `git reset --hard` / forced `git clean`, and option-position
`printf -v` assignments (whose indexed targets can evaluate shell code). Normal formatting with
`printf '%s' '-v'` or `printf -- '-v'` remains available. Git and ripgrep helpers, including `rg -z`,
are allowed like other scripts and subprocesses.

Every command, including Python/Node and external subprocesses, runs in a mandatory **Docker
container sandbox**. No host Bash execution path or unsafe fallback remains. Only the dedicated
`./data/bash/workspace` directory is mounted at `/workspace`; node configuration, provider keys,
plugins, the host home directory and Docker socket are not exposed. `cwd` is relative to that
workspace. Writable workspace files persist between calls and are shared by authorized senders.
The container system filesystem is read-only and user code runs as a non-root UID with no
capabilities and `no-new-privileges`. PID, IPC and cgroup namespaces remain private.

Networking defaults to **public IPv4 Internet access**, as selected by the operator. Host/LAN and
cloud metadata address ranges are blocked; only DNS to configured resolvers and established replies
are exempted. New inbound connections are refused, IPv6 is disabled, and no ports are published.
Set `bash_policy.sandbox.network` to `false` or disable it in Tools for a completely disconnected
container. Model arguments cannot change network mode, mounts, resource ceilings or runtime images.

Defaults are 512 MiB RAM (including swap ceiling), one CPU, 128 processes/threads, 512 MiB per file,
a 128 MiB temporary filesystem, bounded container logs and 64 KiB retained per stdout/stderr stream.
Each call has a 15-second default execution budget (1–120 seconds), plus bounded container setup and
cleanup. Timeout and caller cancellation remove the entire container, including detached child
processes. An image-side watchdog limits execution if the node disappears. Resource values can be
configured under `bash_policy.sandbox`; the writable workspace has no aggregate disk quota, so its
host filesystem should have appropriate capacity or an operator-applied quota.

Prepare the trusted image separately (Kanon does not pull images or install packages):

```bash
docker build -t kanon-bash-sandbox:1 sandbox/bash
```

The image provides Bash, Python 3.12, Node 24, npm, Git and ripgrep. An operator can extend it with
additional dependencies, preserving the trusted bootstrap and runtime label. The node uses a native
Docker API client, not a Docker CLI subprocess. It requires a **local Linux Docker daemon** with
seccomp and memory/CPU/PID controls; Linux Docker Engine and Docker Desktop's Linux mode provide the
runtime. The default socket is `unix:///var/run/docker.sock` on Unix and
`npipe:////./pipe/docker_engine` on Windows. Override `bash_policy.sandbox.endpoint` for a local
rootless/custom socket; remote TCP/SSH endpoints are rejected. Docker or image unavailability denies
execution explicitly while the node and other tools remain available.

The trusted image bootstrap briefly uses only network/identity setup capabilities to install the
firewall, then drops the entire capability bounding set before launching user code. Custom images
must be operator-trusted. The image is pinned by immutable id for each call, health checks are
disabled, and images declaring extra volumes are rejected. Existing workspaces are not silently
chowned; when running the node as root, prepare existing workspace ownership for UID/GID 65534.

The sender permission gate remains authoritative even when the model ignores an unavailable hint.
The container boundary protects host files and processes; it shares the Docker daemon's Linux kernel
and intentionally permits access to the selected workspace and public network. It is not a separate
virtual machine, and host kernel/Docker security remains part of the deployment boundary.

Container integration tests are separate from the runtime-free workspace suite:

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
