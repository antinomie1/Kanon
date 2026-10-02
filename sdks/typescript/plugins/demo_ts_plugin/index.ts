/**
 * Demonstration TypeScript plugin for Kanon microkernel.
 */

import {
  Command,
  CommandEvent,
  HttpRequest,
  HttpRoute,
  MessageEvent,
  MessageSegment,
  OnLlmRequest,
  Plugin,
  PluginContext,
  Tool,
  Trigger,
  WaitTimeoutError,
  s,
} from "../../src/sdk/index.js";

export default class DemoTsPlugin extends Plugin {
  id = "org.kanon.plugin.demo_ts";
  name = "Demo TypeScript Plugin";
  version = "0.1.0";
  author = "Kanon Dev";
  description = "Demonstration plugin written in TypeScript";
  priority = 100;

  async onLoad(ctx: PluginContext): Promise<void> {
    console.log(`Demo TypeScript Plugin initialized with data directory: ${ctx.dataDir}`);
  }

  async onPreFilter(req: any): Promise<any> {
    if (req.raw_text && req.raw_text.includes("[block]")) {
      return {
        action: "BLOCK",
        modified_text: "",
        reply_messages: [
          MessageSegment.text("Message blocked by Demo TypeScript Plugin pre-filter"),
        ],
      };
    }
    return null;
  }

  @Command("tsgreet", {
    description: "TypeScript greeting command",
    usage: "/tsgreet <name>",
    priority: 100,
    aliases: ["tg"],
  })
  async handleGreet(event: CommandEvent, args: string[]): Promise<string> {
    // Returning text is the shortest way to answer.
    const target = args.length > 0 ? args.join(" ") : "World";
    return `Hello from Kanon TypeScript plugin, ${target}!`;
  }

  @Command("tsname", { description: "Asks for your name, then greets you" })
  async handleName(event: CommandEvent): Promise<void> {
    // A multi-turn conversation: waitNext sends the replies so far and resumes with the same
    // sender's next message in this channel.
    await event.reply("What is your name?");
    try {
      const answer = await event.waitNext(60);
      await answer.reply(`Nice to meet you, ${answer.text.trim()}!`);
    } catch (err) {
      if (!(err instanceof WaitTimeoutError)) throw err;
      await event.reply("Never mind.");
    }
  }

  @Trigger("^(?:hi|hello) ts$", { description: "Greets back" })
  async greetBack(event: CommandEvent) {
    return [MessageSegment.quote(event.eventId), "Hello from TypeScript!"];
  }

  @Tool({
    name: "ts_calc",
    description: "TypeScript mathematical calculation tool",
    parameters: {
      type: "object",
      properties: {
        expr: { type: "string", description: "Expression to evaluate" },
      },
    },
  })
  async handleTsCalc(params: any): Promise<any> {
    return {
      result: 42,
      summary: "Calculated via TypeScript plugin tool",
    };
  }

  // --- Command groups backed by the node's KV store --------------------------------------------
  // "/note add <text>" and "/note list" form one group; "/note" alone lists its subcommands.

  @Command("note add", { description: "Save a note for this chat", usage: "/note add <text>" })
  async noteAdd(event: CommandEvent): Promise<string> {
    if (!event.rawArgs) {
      return "Usage: /note add <text>";
    }
    const key = `notes:${event.channelId}`;
    const notes: string[] = await this.kv.get(key, []);
    notes.push(event.rawArgs);
    await this.kv.set(key, notes);
    return `Saved note #${notes.length}.`;
  }

  @Command("note list", { description: "Show this chat's notes" })
  async noteList(event: CommandEvent): Promise<string> {
    const notes: string[] = await this.kv.get(`notes:${event.channelId}`, []);
    return notes.map((note, i) => `${i + 1}. ${note}`).join("\n") || "No notes yet.";
  }

  // --- A tool with described arguments ---------------------------------------------------------

  @Tool("dice", {
    description: "Rolls dice for the user.",
    args: {
      sides: s.integer("Faces on each die.").default(6),
      count: s.integer("How many dice to roll, at most 20.").default(1),
      mode: s.enum(["sum", "each"], "Return the total, or every roll.").default("sum"),
    },
  })
  async dice({ sides, count, mode }: { sides: number; count: number; mode: "sum" | "each" }) {
    if (count < 1 || count > 20 || sides < 2) {
      throw new Error("count must be 1-20 and sides at least 2");
    }
    const rolls = Array.from({ length: count }, () => 1 + Math.floor(Math.random() * sides));
    return mode === "sum" ? rolls.reduce((a, b) => a + b, 0) : rolls;
  }

  // --- Rewriting the system prompt for one chat ------------------------------------------------

  @OnLlmRequest()
  async chatRules(event: MessageEvent, systemPrompt: string): Promise<string | undefined> {
    const rules = await this.kv.get(`rules:${event.channelId}`);
    // Undefined keeps the prompt unchanged.
    return rules ? `${systemPrompt}\n\nRules for this chat:\n${rules}` : undefined;
  }

  @Command("rules", {
    description: "Set extra instructions for the assistant in this chat",
    usage: "/rules [text]",
    access: "admins_in_groups",
  })
  async setRules(event: CommandEvent): Promise<string> {
    if (event.rawArgs) {
      await this.kv.set(`rules:${event.channelId}`, event.rawArgs);
      return "Rules saved; the assistant follows them from the next message.";
    }
    await this.kv.delete(`rules:${event.channelId}`);
    return "Rules cleared.";
  }

  // --- An HTTP route under /api/v1/plugins/org.kanon.plugin.demo_ts/http/ -----------------------

  @HttpRoute("/notes")
  async notesApi(request: HttpRequest) {
    const channel = request.arg("channel", "");
    return { channel, notes: await this.kv.get(`notes:${channel}`, []) };
  }
}
