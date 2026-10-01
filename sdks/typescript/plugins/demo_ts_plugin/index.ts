/**
 * Demonstration TypeScript plugin for Kanon microkernel.
 */

import {
  Command,
  CommandEvent,
  MessageSegment,
  Plugin,
  PluginContext,
  Tool,
  Trigger,
  WaitTimeoutError,
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
}
