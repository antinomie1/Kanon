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
  MessageEvent,
  OnEvent,
  Plugin,
  PrepareTurn,
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
  commandSignals: Array<AbortSignal | undefined> = [];

  @Command("echo", { aliases: ["/e"], access: "admins_in_groups" })
  async echo(event: CommandEvent, args: string[]) {
    return args.join(" ");
  }

  @Command("ask")
  async ask(event: CommandEvent) {
    this.commandSignals.push(event.signal);
    await event.reply("name?");
    try {
      const answer = await event.waitNext(30);
      this.commandSignals.push(answer.signal);
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
  const firstRpc = new AbortController();
  const first = await plugin.onExecuteCommand(request("ask"), firstRpc.signal);
  // The first turn ends at waitNext: its reply goes out and Core is asked to capture.
  assert.deepEqual(texts(first), ["name?"]);
  assert.equal(first.capture_seconds, 30);
  firstRpc.abort();
  assert.equal(plugin.commandSignals[0]?.aborted, false, "the completed RPC listener is detached");

  const secondRpc = new AbortController();
  const second = await plugin.onExecuteCommand(request("ask", "Ann", [], true), secondRpc.signal);
  assert.equal(second.success, true);
  assert.deepEqual(texts(second), ["hello Ann"]);
  assert.equal(second.capture_seconds, 0);
  assert.equal(plugin.commandSignals[0], plugin.commandSignals[1], "continuations share one command signal");
  secondRpc.abort();
  assert.equal(plugin.commandSignals[0]?.aborted, false);
});

for (const continuation of [false, true]) {
  test(`cancelling ${continuation ? "a continued" : "an initial"} RPC blocks later command publication`, {
    timeout: 2000,
  }, async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    let enter!: (event: CommandEvent) => void;
    const entered = new Promise<CommandEvent>((resolve) => { enter = resolve; });
    let finish!: () => void;
    const finished = new Promise<void>((resolve) => { finish = resolve; });
    let sent = 0;

    class Waiting extends Plugin {
      @Command("wait")
      async wait(event: CommandEvent) {
        if (continuation) event = await event.waitNext(30);
        enter(event);
        try {
          // JavaScript promises cannot be preempted. This intentionally ignores event.signal;
          // once released, the SDK must still prevent its late reply from reaching Core.
          await gate;
          await event.reply("late reply");
        } finally {
          finish();
        }
      }
    }

    const plugin = new Waiting();
    plugin.context = {
      dataDir: ".",
      config: {},
      core: { replyTo: async () => { sent += 1; } } as unknown as CoreHandle,
    };
    if (continuation) {
      const captured = await plugin.onExecuteCommand(request("wait"));
      assert.equal(captured.capture_seconds, 30);
    }
    const rpc = new AbortController();
    const pending = plugin.onExecuteCommand(request("wait", "next", [], continuation), rpc.signal);
    try {
      const event = await entered;
      rpc.abort();
      await assert.rejects(pending, /command RPC was cancelled/);
      assert.equal(event.signal?.aborted, true);
      await assert.rejects(event.reply("reply"), /command RPC was cancelled/);
      await assert.rejects(event.send("send"), /command RPC was cancelled/);
      await assert.rejects(event.waitNext(600), /command RPC was cancelled/);
      assert.throws(() => event.passToModel(), /command RPC was cancelled/);
      assert.equal((plugin as any).conversations.waiting.size, 0);
    } finally {
      rpc.abort();
      release();
      await finished;
    }
    assert.equal(sent, 0);
    assert.equal((plugin as any).conversations.waiting.size, 0);
  });
}

for (const preCancelledContinuation of [false, true]) {
  test(`cancellation cleans ${preCancelledContinuation ? "an already cancelled continuation" : "a capture before its RPC response"}`, {
    timeout: 2000,
  }, async () => {
    const rpc = new AbortController();
    let resumed = false;
    let finish!: () => void;
    const finished = new Promise<void>((resolve) => { finish = resolve; });
    class Capturing extends Plugin {
      @Command("capture")
      async capture(event: CommandEvent) {
        const next = event.waitNext(600);
        if (!preCancelledContinuation) rpc.abort();
        try {
          await next;
          resumed = true;
        } finally {
          finish();
        }
      }
    }
    const plugin = new Capturing();
    if (preCancelledContinuation) {
      const captured = await plugin.onExecuteCommand(request("capture"));
      assert.equal(captured.capture_seconds, 600);
      rpc.abort();
    }
    await assert.rejects(
      plugin.onExecuteCommand(request("capture", "next", [], preCancelledContinuation), rpc.signal),
      /command RPC was cancelled/,
    );
    await finished;
    assert.equal(resumed, false);
    assert.equal((plugin as any).conversations.waiting.size, 0);
  });
}

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

class Handover extends Plugin {
  id = "test.handover";

  @Command("note", { platforms: ["onebot"], conversationKinds: ["group"] })
  async note(event: CommandEvent, args: string[]) {
    await event.reply("noted");
    event.passToModel(`remember: ${args.join(" ")}`);
  }

  @Command("hold")
  async hold(event: CommandEvent) {
    event.passToModel();
    try {
      await event.waitNext(10);
    } catch (err) {
      if (!(err instanceof WaitTimeoutError)) throw err;
    }
  }

  @Command("explicit")
  async explicit() {
    return { success: true, pass_to_model: true };
  }

  @PrepareTurn()
  async prepare(event: MessageEvent, sessionId: string) {
    return event.text === "skip" ? undefined : `[${sessionId}] likes tea`;
  }
}

test("commands pass messages to the model and preparers add context", async () => {
  const plugin = new Handover();
  const meta = plugin.meta();
  assert.equal(meta.prepares_turns, true);
  const note = meta.commands!.find((c) => c.name === "note")!;
  assert.deepEqual(note.platforms, ["onebot"]);
  assert.deepEqual(note.conversation_kinds, ["CONVERSATION_KIND_GROUP"]);
  assert.throws(
    () => Command("x", { conversationKinds: ["room" as any] }),
    /unknown conversation kind/,
  );

  const noted = await plugin.onExecuteCommand(request("note", "tea", ["tea"]));
  assert.deepEqual(texts(noted), ["noted"]);
  assert.equal(noted.pass_to_model, true);
  assert.equal(noted.model_text, "remember: tea");

  // A capture wins: the handler is waiting for the next message, so this one stays its own.
  const held = await plugin.onExecuteCommand(request("hold"));
  assert.equal(held.capture_seconds, 10);
  assert.equal(held.pass_to_model, false);

  const explicit = await plugin.onExecuteCommand(request("explicit"));
  assert.equal(explicit.pass_to_model, true);
  assert.equal(explicit.model_text, undefined);

  const context = { platform: "onebot", sender_id: "u1", raw_text: "hi" };
  assert.deepEqual(await plugin.onPrepareTurn({ context, session_id: "s1" }), {
    text: "[s1] likes tea",
  });
  assert.deepEqual(
    await plugin.onPrepareTurn({ context: { ...context, raw_text: "skip" }, session_id: "s1" }),
    { text: "" },
  );
  assert.deepEqual(await new Demo().onPrepareTurn({ context, session_id: "s1" }), { text: "" });
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
