# Kanon Python SDK & Host

Write Kanon plugins in Python. Each plugin runs in its own host process; the node installs its
dependencies into `<plugin>/.venv` with `uv sync` and talks to it over gRPC.

```python
from kanon_sdk import CommandEvent, MessageEvent, Plugin, command, on_llm_request, tool


class Notes(Plugin):
    id = "org.example.notes"
    name = "Notes"
    version = "0.1.0"

    @command("note add", usage="/note add <text>")       # a command group: /note add, /note list
    async def add(self, event: CommandEvent) -> str:
        notes = await self.kv.get(event.channel_id, [])    # the node's KV store, JSON values
        await self.kv.set(event.channel_id, notes + [event.raw_args])
        return "Saved."

    @command("note list")
    async def list_notes(self, event: CommandEvent) -> str:
        return "\n".join(await self.kv.get(event.channel_id, [])) or "No notes."

    @tool                                                  # schema inferred from the signature
    async def search_notes(self, query: str, event: MessageEvent) -> list:
        """Searches this chat's notes.

        Args:
            query: Words to look for.
        """
        return [n for n in await self.kv.get(event.channel_id, []) if query in n]

    @on_llm_request                                        # rewrite the system prompt per chat
    async def style(self, event: MessageEvent, system_prompt: str) -> str:
        return system_prompt + "\nAnswer briefly."
```

Start a new plugin with `kanon-dev create my_plugin --lang python`. The full guide (commands,
tools, events, KV, agents, conversations, rendering, HTTP routes) is
[docs/PLUGIN_GUIDE.md](../../docs/PLUGIN_GUIDE.md).

Tests: `python -m unittest discover -s tests` with the SDK's dependencies installed (for example
the demo plugin's environment: `cd plugins/demo_py_plugin && uv sync`).
