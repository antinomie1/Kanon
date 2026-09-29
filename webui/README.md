# Kanon WebUI Console

Modern, decoupled web console and AI playground for Kanon microkernel node.

## Tech Stack
- **Framework**: Svelte 5 (Runes `$state`, `$derived`, `$effect`) + TypeScript
- **Build Tool**: Vite
- **Styling**: Tailwind CSS v4
- **Icons**: lucide-svelte
- **Tooling**: Bun + Biome (Rust-based ultra-fast linter & formatter)

## Features
- **Overview**: Node health, uptime, Supervisor host counters, Prometheus exposition.
- **Instances**: Create bot instances, choose their adapters, persona and optional model, and edit
  them without losing their conversations.
- **Chat**: Interactive streaming chat via SSE with live tool execution audit breakdown.
- **Pipeline & Logs**:
  - Live pipeline stage transitions from `/ws/v1/events` (Ingested -> PreFilter -> Command -> LLM -> Tool -> Outbound).
  - High-density scrolling log terminal from `/ws/v1/logs` with level filtering (`DEBUG`, `INFO`, `WARN`, `ERROR`), search, and auto-scroll lock.
- **Plugins & Adapters**: Inspect active plugin hosts, view & edit configuration schemas with CAS concurrency protection, restart hosts, and configure the platform adapters.
- **Sessions**: Tracked conversations with turn and token counters, per-session persona binding
  (or none, which means the base assistant), and history reset. Sessions are stored on the node and
  are still listed after it restarts.
- **Personas**: Add, edit and delete persona presets (fixed prompt text). Only a read-only base
  assistant ships with the node; a persona an instance still uses cannot be deleted.
- **Model Providers**: Manage provider endpoints (connectivity tests use the key stored on the node,
  which the browser never receives), discover each endpoint's models, and choose the single **global
  default model**. There is no "default provider".
- **System Settings**: Reply and context policies plus the node's runtime paths and environment.
- **Command Palette**: Press `Cmd + K` or `Ctrl + K` anytime to switch views or execute actions.
- **Appearance**: The theme switcher at the bottom of the sidebar offers system (default), light and
  dark modes; the language switcher toggles Chinese and English.

## Development

```bash
# Install dependencies
bun install

# Start Vite dev server (proxies /api and /ws to http://127.0.0.1:8080)
bun run dev

# Run typecheck
bun run check

# Lint & format with Biome
bun run lint
bun run format

# Production build (outputs to dist/)
bun run build
```
