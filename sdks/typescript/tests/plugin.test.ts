/**
 * Decorators, event-style dispatch, multi-turn conversations and the Core API helpers.
 */

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import * as grpc from "@grpc/grpc-js";

import {
  Command,
  CommandEvent,
  CoreHandle,
  DecorateReply,
  MessageSegment,
  OnEvent,
  Plugin,
  Reply,
  Trigger,
  WaitTimeoutError,
  fromProtoStruct,
  loadKanonProto,
} from "../src/sdk/index.js";

/** A command request as Core sends it, from sender u1 in group g1. */
function request(command: string, text = "", args: string[] = [], continuation = false) {
  return {
    plugin_id: "test.plugin",
    command,
    args,
    raw_args: text,
    continuation,
    context: {
      event_id: `onebot:${text}`,
      platform: "onebot",
      channel_id: "group:g1",
      sender_id: "u1",
      raw_text: text,
    },
  };
}

const texts = (response: any): string[] => response.replies.map((s: any) => s.text?.content);

class Demo extends Plugin {
  id = "test.plugin";
  seen: string[] = [];

  @Command("echo", { aliases: ["/e"], access: "admins_in_groups" })
  async echo(event: CommandEvent, args: string[]) {
    return args.join(" ");
  }

  @Command("ask")
  async ask(event: CommandEvent) {
    await event.reply("name?");
    try {
      const answer = await event.waitNext(30);
      await answer.reply(`hello ${answer.text}`);
    } catch (err) {
      if (!(err instanceof WaitTimeoutError)) throw err;
    }
  }

  @Command("boom")
  async boom() {
    throw new Error("bad input");
  }

  @Trigger("^ping$")
  async ping(event: CommandEvent) {
    return [MessageSegment.quote(event.eventId), "pong"];
  }

  @OnEvent("llm_response")
  async onAnswer(event: any) {
    this.seen.push(event.content);
  }

  @DecorateReply()
  async sign(reply: Reply) {
    return reply.source === "llm" ? [...reply.segments, " — bot"] : undefined;
  }
}

test("meta lists commands, triggers and hooks", () => {
  const meta = new Demo().meta();
  const echo = meta.commands!.find((c) => c.name === "echo")!;
  assert.deepEqual(echo.aliases, ["e"]);
  assert.equal(echo.access, "COMMAND_ACCESS_ADMINS_IN_GROUPS");
  assert.deepEqual(
    meta.triggers!.map((t) => [t.name, t.pattern]),
    [["ping", "^ping$"]],
  );
  assert.deepEqual(meta.events, ["EVENT_KIND_LLM_RESPONSE"]);
  assert.equal(meta.decorates_replies, true);
  // Another plugin class must not inherit Demo's declarations.
  class Empty extends Plugin {}
  assert.deepEqual(new Empty().meta().commands, []);
});

test("a command and a trigger cannot share a name", () => {
  class Clash extends Plugin {
    @Command("x") async a() {}
    @Trigger("^x$", { name: "x" }) async b() {}
  }
  assert.throws(() => new Clash().meta(), /both a command and a trigger/);
});

test("return values become replies and failures are reported", async () => {
  const plugin = new Demo();
  assert.deepEqual(texts(await plugin.onExecuteCommand(request("echo", "a b", ["a", "b"]))), ["a b"]);

  const pinged = await plugin.onExecuteCommand(request("ping", "ping"));
  assert.equal(pinged.replies[0].reply.target_message_id, "onebot:ping");
  assert.equal(pinged.replies[1].text.content, "pong");

  const failed = await plugin.onExecuteCommand(request("boom"));
  assert.equal(failed.success, false);
  assert.match(failed.error_message, /bad input/);
});

test("waitNext spans two RPCs", async () => {
  const plugin = new Demo();
  const first = await plugin.onExecuteCommand(request("ask"));
  // The first turn ends at waitNext: its reply goes out and Core is asked to capture.
  assert.deepEqual(texts(first), ["name?"]);
  assert.equal(first.capture_seconds, 30);

  const second = await plugin.onExecuteCommand(request("ask", "Ann", [], true));
  assert.equal(second.success, true);
  assert.deepEqual(texts(second), ["hello Ann"]);
  assert.equal(second.capture_seconds, 0);
});

test("a continuation nobody waits for calls the handler afresh", async () => {
  const response = await new Demo().onExecuteCommand(request("echo", "late", ["late"], true));
  assert.deepEqual(texts(response), ["late"]);
});

test("a newer waitNext supersedes the older one", async () => {
  const plugin = new Demo();
  await plugin.onExecuteCommand(request("ask"));
  await plugin.onExecuteCommand(request("ask"));
  const response = await plugin.onExecuteCommand(request("ask", "Bo", [], true));
  assert.deepEqual(texts(response), ["hello Bo"]);
});

test("events dispatch by kind and the decorator rewrites only model replies", async () => {
  const plugin = new Demo();
  await plugin.onEvent({ detail: "llm_response", llm_response: { content: "answer" } });
  await plugin.onEvent({ detail: "message_sent", message_sent: {} });
  assert.deepEqual(plugin.seen, ["answer"]);

  const llm = await plugin.onDecorateReply({
    segments: [MessageSegment.text("hi")],
    source: "REPLY_SOURCE_LLM",
  });
  assert.equal(llm.modified, true);
  assert.deepEqual(llm.segments.map((s: any) => s.text.content), ["hi", " — bot"]);

  const cmd = await plugin.onDecorateReply({ source: "REPLY_SOURCE_COMMAND", command: "echo" });
  assert.equal(cmd.modified, false);
});

test("callPlatformApi round-trips JSON over the wire", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "kanon-api-"));
  const socket = path.join(dir, "core.sock");
  const kanonV1 = (loadKanonProto() as any).kanon.plugin.v1;
  let received: any;

  const server = new grpc.Server();
  server.addService(kanonV1.BotApiService.service, {
    CallPlatformApi: (call: any, callback: any) => {
      received = call.request;
      callback(null, {
        result: { listValue: { values: [{ stringValue: "member" }, { numberValue: 2 }] } },
      });
    },
  });
  await new Promise<void>((resolve, reject) =>
    server.bindAsync(`unix:${socket}`, grpc.ServerCredentials.createInsecure(), (err) =>
      err ? reject(err) : resolve(),
    ),
  );

  const core = new CoreHandle(socket);
  try {
    const result = await core.callPlatformApi("onebot", "get_group_member_list", { group_id: 1 });
    assert.deepEqual(result, ["member", 2]);
    assert.equal(received.action, "get_group_member_list");
    assert.deepEqual(fromProtoStruct(received.params), { group_id: 1 });
  } finally {
    core.close();
    server.forceShutdown();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
