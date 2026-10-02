# Kanon TypeScript SDK & Host

Write Kanon plugins in TypeScript. Each plugin runs in its own host process (`bun`, or `node`);
the node installs its dependencies into `<plugin>/node_modules` and talks to it over gRPC.

```ts
import {
  Command,
  CommandEvent,
  MessageEvent,
  OnLlmRequest,
  Plugin,
  Tool,
  s,
} from "@kanon/sdk-and-host";

export default class Notes extends Plugin {
  id = "org.example.notes";
  name = "Notes";
  version = "0.1.0";

  @Command("note add", { usage: "/note add <text>" }) // a command group: /note add, /note list
  async add(event: CommandEvent) {
    const notes: string[] = await this.kv.get(event.channelId, []); // the node's KV store
    await this.kv.set(event.channelId, [...notes, event.rawArgs]);
    return "Saved.";
  }

  @Command("note list")
  async list(event: CommandEvent) {
    return (await this.kv.get(event.channelId, [])).join("\n") || "No notes.";
  }

  @Tool("search_notes", {
    description: "Searches this chat's notes.",
    args: { query: s.string("Words to look for.") }, // the schema the model sees
  })
  async search({ query }: { query: string }, event?: MessageEvent) {
    const notes: string[] = await this.kv.get(event?.channelId ?? "", []);
    return notes.filter((note) => note.includes(query));
  }

  @OnLlmRequest() // rewrite the system prompt per chat
  async style(event: MessageEvent, systemPrompt: string) {
    return `${systemPrompt}\nAnswer briefly.`;
  }
}
```

Start a new plugin with `kanon-dev create my_plugin --lang ts`. The full guide (commands, tools,
events, KV, agents, conversations, rendering, HTTP routes) is
[docs/PLUGIN_GUIDE.md](../../docs/PLUGIN_GUIDE.md).

Tests: `npm test` (or `./node_modules/.bin/tsc && node --test "dist/tests/**/*.test.js"`).
