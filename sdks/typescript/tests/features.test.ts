/**
 * Command groups, described and runtime tools, prompt rewriting, HTTP routes, and the core calls
 * behind them (KV, agent runs, conversations, rendering, metadata refresh).
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
  HttpRequest,
  HttpResponse,
  HttpRoute,
  MessageEvent,
  MessageSegment,
  OnEvent,
  OnLlmRequest,
  Plugin,
  Tool,
  fromProtoStruct,
  loadKanonProto,
  s,
  toProtoStruct,
} from "../src/sdk/index.js";

const CONTEXT = {
  event_id: "onebot:1",
  platform: "onebot",
  channel_id: "group:g1",
  sender_id: "u1",
  raw_text: "",
};

/** A command request as Core sends it: arguments split on whitespace. */
function request(command: string, text = "") {
  return {
    plugin_id: "test.features",
    command,
    args: text.split(/\s+/).filter(Boolean),
    raw_args: text,
    continuation: false,
    context: CONTEXT,
  };
}

const texts = (response: any): string[] => response.replies.map((s: any) => s.text?.content);

function toolCall(name: string, args: Record<string, any>, withContext = true) {
  return {
    call_id: "c1",
    tool_name: name,
    structured_args: toProtoStruct(args),
    ...(withContext ? { context: CONTEXT } : {}),
  };
}

const result = (response: any) => fromProtoStruct(response.structured_result);

class Features extends Plugin {
  id = "test.features";
  items: string[] = [];
  agentRuns: string[] = [];

  @Command("todo add", { description: "Add a todo", usage: "/todo add <text>" })
  async add(event: CommandEvent) {
    this.items.push(event.rawArgs);
    return `added ${JSON.stringify(event.args)}`;
  }

  @Command("todo list", { description: "List todos" })
  async list() {
    return this.items.join(", ") || "empty";
  }

  @Command("notes list")
  async notes() {
    return "no notes";
  }

  @Command("notes", { description: "Personal notes", access: "admins" })
  async notesRoot(event: CommandEvent) {
    return `notes root ${JSON.stringify(event.args)}`;
  }

  @Tool("greet", {
    description: "Greets someone.",
    args: {
      name: s.string("Who to greet"),
      times: s.integer().default(1),
      mood: s.enum(["warm", "dry"]).optional(),
    },
  })
  async greet(
    { name, times, mood }: { name: string; times: number; mood?: string },
    event?: MessageEvent,
  ) {
    return `${"hi ".repeat(times)}${name} from ${event?.senderId ?? "nobody"}${mood ? ` (${mood})` : ""}`;
  }

  @Tool({ name: "legacy", parameters: { type: "object", properties: { x: { type: "number" } } } })
  async legacy(args: any) {
    return [args.x, args.x];
  }

  @OnLlmRequest()
  async rules(event: MessageEvent, systemPrompt: string, sessionId: string) {
    return sessionId === "keep" ? undefined : `${systemPrompt}\nRules for ${event.channelId}.`;
  }

  @OnEvent("agent_done")
  async agentDone(done: any) {
    this.agentRuns.push(`${done.success}:${done.tools.join(",")}`);
  }

  @HttpRoute("/stats")
  async stats(req: HttpRequest) {
    return { page: req.arg("page"), items: this.items.length };
  }

  @HttpRoute("/echo", { methods: ["POST"] })
  async echo(req: HttpRequest) {
    return HttpResponse.json(req.json(), 201);
  }

  @HttpRoute("/broken")
  async broken(): Promise<string> {
    throw new Error("secret detail");
  }
}

test("meta lists groups with their subcommands and declares hooks and routes", () => {
  const meta = new Features().meta();
  const commands = Object.fromEntries(meta.commands!.map((c) => [c.name, c]));
  assert.deepEqual(
    commands.todo.subcommands!.map((c) => [c.name, c.usage]),
    [
      ["add", "/todo add <text>"],
      ["list", "/todo list"],
    ],
  );
  assert.equal(commands.notes.description, "Personal notes");
  assert.equal(commands.notes.access, "COMMAND_ACCESS_ADMINS");
  assert.deepEqual(commands.notes.subcommands!.map((c) => c.name), ["list"]);
  assert.equal(meta.rewrites_system_prompt, true);
  assert.equal(meta.serves_http, true);
  assert.ok(meta.events!.includes("EVENT_KIND_AGENT_DONE"));
});

test("subcommand handlers see only their own arguments; a bare group lists them", async () => {
  const plugin = new Features();
  assert.deepEqual(texts(await plugin.onExecuteCommand(request("todo", "add buy milk"))), [
    'added ["buy","milk"]',
  ]);
  assert.deepEqual(plugin.items, ["buy milk"]);

  for (const text of ["", "frobnicate"]) {
    const response = await plugin.onExecuteCommand(request("todo", text));
    assert.equal(response.success, true);
    assert.deepEqual(texts(response), ["/todo add <text> — Add a todo\n/todo list — List todos"]);
  }
  // With @Command("notes") declared, it takes what no subcommand matches.
  assert.deepEqual(texts(await plugin.onExecuteCommand(request("notes", "x y"))), [
    'notes root ["x","y"]',
  ]);
});

test("subcommands cannot carry group-level settings", () => {
  assert.throws(() => Command("todo add", { access: "admins" }), /only description and usage/);
  assert.throws(() => Command("a b c"), /must be/);
});

test("described tools get a schema, defaults, and errors the model can act on", async () => {
  const plugin = new Features();
  const greet = plugin.meta().tools!.find((t) => t.name === "greet")!;
  assert.deepEqual(fromProtoStruct(greet.parameters), {
    type: "object",
    properties: {
      name: { type: "string", description: "Who to greet" },
      times: { type: "integer", default: 1 },
      mood: { enum: ["warm", "dry"], type: "string" },
    },
    required: ["name"],
  });

  assert.deepEqual(result(await plugin.onCallTool(toolCall("greet", { name: "Ann", times: 2 }))), {
    result: "hi hi Ann from u1",
  });
  assert.deepEqual(result(await plugin.onCallTool(toolCall("greet", { name: "Bo" }, false))), {
    result: "hi Bo from nobody",
  });

  const unknown = await plugin.onCallTool(toolCall("greet", { name: "A", tone: "x" }));
  assert.equal(unknown.success, false);
  assert.match(unknown.error_message, /unexpected arguments \["tone"\]/);
  const missing = await plugin.onCallTool(toolCall("greet", {}));
  assert.match(missing.error_message, /missing required arguments \["name"\]/);

  // An explicit schema is passed through; a list result keeps its shape.
  assert.deepEqual(result(await plugin.onCallTool(toolCall("legacy", { x: 2 }))), {
    result: [2, 2],
  });
});

test("runtime tools are announced and rolled back when the node refuses", async () => {
  const plugin = new Features();
  let refuse = false;
  let refreshes = 0;
  plugin.context = {
    dataDir: ".",
    config: {},
    core: {
      refreshMeta: async () => {
        if (refuse) throw new Error("UNAVAILABLE");
        refreshes += 1;
        return ["test.features"];
      },
    } as any,
  };
  const names = () => plugin.meta().tools!.map((t) => t.name);

  await plugin.addTool(
    "roll",
    { description: "Rolls a die", args: { sides: s.integer() } },
    async ({ sides }: { sides: number }) => sides,
  );
  assert.ok(names().includes("roll"));
  assert.equal(refreshes, 1);
  assert.deepEqual(result(await plugin.onCallTool(toolCall("roll", { sides: 6 }))), { result: 6 });
  await assert.rejects(plugin.addTool("greet", {}, () => 0), /already exists/);

  refuse = true;
  await assert.rejects(plugin.removeTool("roll"), /UNAVAILABLE/);
  assert.ok(names().includes("roll"), "a refused removal keeps the tool");
  await assert.rejects(plugin.addTool("roll2", {}, () => 0), /UNAVAILABLE/);
  assert.ok(!names().includes("roll2"), "a refused addition leaves no tool behind");

  refuse = false;
  assert.equal(await plugin.removeTool("roll"), true);
  assert.equal(await plugin.removeTool("roll"), false);
  await assert.rejects(plugin.removeTool("greet"), /declared with @Tool/);
});

test("non-finite tool arguments and results fail instead of becoming null", async () => {
  const plugin = new Features();
  plugin.context = {
    dataDir: ".",
    config: {},
    core: { refreshMeta: async () => [plugin.id] } as any,
  };
  let number = 1.5;
  let calls = 0;
  await plugin.addTool("numeric-result", { args: {} }, () => {
    calls++;
    return { nested: [number] };
  });
  for (number of [NaN, Infinity, -Infinity]) {
    const response = await plugin.onCallTool(toolCall("numeric-result", {}));
    assert.equal(response.success, false);
    assert.match(response.error_message, /finite/);
    assert.equal(response.structured_result, undefined);
  }
  for (const invalid of [NaN, Infinity, -Infinity]) {
    const request = toolCall("numeric-result", {});
    request.structured_args = { fields: { nested: { listValue: { values: [{ numberValue: invalid }] } } } };
    const before = calls;
    const response = await plugin.onCallTool(request);
    assert.equal(response.success, false);
    assert.equal(response.call_id, request.call_id);
    assert.match(response.error_message, /finite/);
    assert.equal(calls, before, "invalid arguments must not reach the handler");
  }
  number = 1.5;
  const response = await plugin.onCallTool(toolCall("numeric-result", {}));
  assert.equal(response.success, true);
  assert.deepEqual(result(response), { nested: [1.5] });
});

test("the rewriter replaces or keeps the system prompt", async () => {
  const plugin = new Features();
  assert.deepEqual(
    await plugin.onLlmRequest({ context: CONTEXT, session_id: "s1", system_prompt: "Be kind." }),
    { system_prompt: "Be kind.\nRules for group:g1." },
  );
  assert.deepEqual(
    await plugin.onLlmRequest({ context: CONTEXT, session_id: "keep", system_prompt: "Be kind." }),
    {},
  );
});

test("agent events reach their subscribers", async () => {
  const plugin = new Features();
  await plugin.onEvent({ detail: "agent_done", agent_done: { success: true, tools: ["a", "b"] } });
  assert.deepEqual(plugin.agentRuns, ["true:a,b"]);
});

test("routes answer by path and method and hide handler errors", async () => {
  const plugin = new Features();
  const body = (response: any) => JSON.parse(Buffer.from(response.body).toString());

  const ok = await plugin.onHttpRequest({ method: "get", path: "/stats", query: "page=2" });
  assert.equal(ok.status, 200);
  assert.deepEqual(body(ok), { page: "2", items: 0 });

  const created = await plugin.onHttpRequest({
    method: "POST",
    path: "/echo",
    body: Buffer.from('{"a":1}'),
  });
  assert.deepEqual([created.status, body(created)], [201, { a: 1 }]);

  const wrong = await plugin.onHttpRequest({ method: "GET", path: "/echo" });
  assert.equal(wrong.status, 405);
  assert.deepEqual(wrong.headers, [{ name: "allow", value: "POST" }]);

  assert.equal((await plugin.onHttpRequest({ method: "GET", path: "/nope" })).status, 404);

  const failed = await plugin.onHttpRequest({ method: "GET", path: "/broken" });
  assert.equal(failed.status, 500);
  assert.ok(!Buffer.from(failed.body).toString().includes("secret"));
});

/** Serves a fake `BotApiService` on a fresh socket for the duration of `body`. */
async function withCore(
  handlers: Record<string, (request: any) => any>,
  body: (core: CoreHandle) => Promise<void>,
): Promise<void> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "kanon-core-"));
  const socket = path.join(dir, "core.sock");
  const kanonV1 = (loadKanonProto() as any).kanon.plugin.v1;
  const server = new grpc.Server();
  const implementation: Record<string, any> = {};
  for (const [method, handle] of Object.entries(handlers)) {
    implementation[method] = (call: any, callback: any) => {
      try {
        callback(null, handle(call.request));
      } catch (err: any) {
        callback({ code: grpc.status.UNAVAILABLE, message: err.message });
      }
    };
  }
  server.addService(kanonV1.BotApiService.service, implementation);
  await new Promise<void>((resolve, reject) =>
    server.bindAsync(`unix:${socket}`, grpc.ServerCredentials.createInsecure(), (err) =>
      err ? reject(err) : resolve(),
    ),
  );
  const core = new CoreHandle(socket, undefined, { hostId: "host_t", pluginId: "test.features" });
  try {
    await body(core);
  } finally {
    core.close();
    server.forceShutdown();
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

test("kv stores JSON in the plugin's namespace and reports foreign bytes", async () => {
  const store = new Map<string, Buffer>();
  const sets: any[] = [];
  await withCore(
    {
      SetStorage: (req) => {
        sets.push(req);
        store.set(req.key, req.value);
        return { success: true };
      },
      GetStorage: (req) => ({ found: store.has(req.key), value: store.get(req.key) }),
      DeleteStorage: (req) => ({ deleted: store.delete(req.key) }),
      ListStorage: (req) => ({ keys: [...store.keys()].filter((k) => k.startsWith(req.prefix)) }),
    },
    async (core) => {
      const plugin = new Features();
      plugin.context = { dataDir: ".", config: {}, core };

      assert.equal(await plugin.kv.get("visits", 0), 0);
      await plugin.kv.set("visits", { u1: 3 }, { ttl: 60 });
      assert.deepEqual(await plugin.kv.get("visits"), { u1: 3 });
      assert.equal(sets[0].plugin_id, "test.features");
      assert.equal(String(sets[0].ttl_seconds), "60");
      assert.deepEqual(await plugin.kv.keys("vis"), ["visits"]);
      assert.equal(await plugin.kv.delete("visits"), true);

      store.set("raw", Buffer.from([0xff, 0x00]));
      await assert.rejects(plugin.kv.get("raw"), /'raw' is not JSON/);
      await assert.rejects(plugin.kv.set("x", 1, { ttl: 0 }), RangeError);
      await assert.rejects(plugin.kv.set("x", undefined), TypeError);

      await plugin.kv.set("numeric", { nested: [1.5, null] });
      const before = sets.length;
      for (const number of [NaN, Infinity, -Infinity]) {
        await assert.rejects(plugin.kv.set("numeric", { nested: [number] }), /finite/);
        await assert.rejects(core.callPlatformApi("test", "action", { nested: [number] }), /finite/);
      }
      assert.equal(sets.length, before, "invalid numbers must not reach Core");
      assert.deepEqual(await plugin.kv.get("numeric"), { nested: [1.5, null] });
      for (const encoded of ["NaN", '{"nested":[Infinity]}', "-Infinity", "1e400"]) {
        store.set("foreign", Buffer.from(encoded));
        await assert.rejects(plugin.kv.get("foreign"), /'foreign' is not JSON/);
      }

      for (const ttl of [1.5, 0, -1, Number.MAX_SAFE_INTEGER + 1, 2 ** 63, 2 ** 64]) {
        await assert.rejects(plugin.kv.set("numeric", 1, { ttl }), /ttl/);
      }
      assert.equal(sets.length, before, "invalid TTLs must not reach Core");
      for (const ttl of [1, Number.MAX_SAFE_INTEGER, undefined]) {
        await plugin.kv.set("numeric", 1, { ttl });
        assert.equal(String(sets.at(-1).ttl_seconds), String(ttl ?? 0));
      }
    },
  );
  assert.throws(() => new Features().kv, /standalone/);
});

test("agent runs, conversations, rendering and refresh speak the wire contract", async () => {
  const seen: Record<string, any> = {};
  await withCore(
    {
      RunAgent: (req) => {
        seen.agent = req;
        return { content: "done", tools: ["search"], session_id: "s9" };
      },
      SwitchConversation: (req) => {
        seen.switch = req;
        return {
          conversations: [
            { session_id: "a", title: "first", message_count: 2, last_active_at: "1700000000" },
            { session_id: req.session_id, current: true },
          ],
        };
      },
      AppendConversation: (req) => {
        seen.append = req;
        return { session_id: "s1" };
      },
      RenderImage: (req) => {
        seen.render = req;
        return { file_path: "/render/card.png", width: 720, height: 100 };
      },
      RefreshPluginMeta: (req) => {
        seen.refresh = req;
        return { plugin_ids: ["test.features"] };
      },
    },
    async (core) => {
      const event = new MessageEvent(CONTEXT, core);

      const run = await core.runAgent("summarize", {
        event,
        inConversation: true,
        images: [MessageSegment.imageUrl("https://x/y.png")],
      });
      assert.deepEqual(run, { content: "done", attachments: [], tools: ["search"], sessionId: "s9" });
      assert.equal(seen.agent.plugin_id, "test.features");
      assert.equal(seen.agent.context.sender_id, "u1");
      assert.equal(seen.agent.in_conversation, true);
      assert.equal(seen.agent.use_tools, true);
      assert.equal(seen.agent.images[0].url, "https://x/y.png");

      const listed = await core.switchConversation(event, "b");
      assert.deepEqual(listed[0], {
        sessionId: "a",
        current: false,
        title: "first",
        messageCount: 2,
        lastActiveAt: 1700000000,
      });
      assert.equal(listed[1].current, true);
      assert.equal(seen.switch.context.channel_id, "group:g1");

      const session = await core.appendConversation(event, [
        { role: "user", text: "q" },
        { role: "assistant", text: "a" },
      ]);
      assert.equal(session, "s1");
      assert.deepEqual(
        seen.append.messages.map((m: any) => [m.role, m.text]),
        [
          ["LLM_ROLE_USER", "q"],
          ["LLM_ROLE_ASSISTANT", "a"],
        ],
      );
      await assert.rejects(
        core.appendConversation(event, [{ role: "system" as any, text: "no" }]),
        /role must be/,
      );

      const card = await core.renderText("# Title\nbody", 400);
      assert.deepEqual(card, MessageSegment.imageFile("/render/card.png", "image/png"));
      assert.equal(seen.render.text, "# Title\nbody");
      assert.equal(seen.render.width, 400);

      assert.deepEqual(await core.refreshMeta(), ["test.features"]);
      assert.equal(seen.refresh.host_id, "host_t");
    },
  );
});
