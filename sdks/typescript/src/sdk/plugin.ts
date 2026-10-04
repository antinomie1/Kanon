/** Plugin lifecycle and dispatch over the declared metadata. */

import {
  CommandEvent,
  Conversations,
  MessageEvent,
  Session,
  Turn,
  WaitTimeoutError,
  runTurn,
} from "./event.js";
import { KV } from "./kv.js";
import { bindArgs } from "./schema.js";
import { MessageSegment, toSegments } from "./segments.js";
import { fromProtoStruct, toProtoStruct } from "./struct.js";
import { HttpMethod, HttpRequest, toHttpResponse } from "./web.js";

import type { CoreHandle, PluginContext } from "./core.js";
import {
  EVENT_KINDS,
  accessValue,
  declarations,
  toolEntry,
  type CommandMeta,
  type Declarations,
  type EventKind,
  type PluginMeta,
  type Reply,
  type ToolEntry,
  type ToolOptions,
} from "./declarations.js";
/** Lines listing a group's subcommands, as `/help` shows them. */
function groupHelp(subcommands: Declarations["subcommands"]): string {
  return subcommands
    .map((sub) => (sub.description ? `${sub.usage} — ${sub.description}` : sub.usage))
    .join("\n");
}

/** The request a subcommand handler sees: the subcommand word removed from the arguments. */
function withoutSubcommand(req: any): any {
  const sub: string = req.args[0];
  const raw: string = (req.raw_args ?? "").trimStart();
  return {
    ...req,
    args: req.args.slice(1),
    raw_args: raw.startsWith(sub) ? raw.slice(sub.length).trimStart() : req.raw_args,
  };
}

/** A tool result as the wire `ToolCallResponse` carries it. */
function toolResult(callId: string, value: any): any {
  if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
    return { call_id: callId, success: true, error_message: "", raw_bytes: value };
  }
  const plain = value && typeof value === "object" && !Array.isArray(value);
  return {
    call_id: callId,
    success: true,
    error_message: "",
    // JSON values keep their shape (a list stays a list); an object is the result itself.
    structured_result: toProtoStruct(plain ? value : { result: value ?? null }),
  };
}

/** Whether a handler's return value is a command response rather than reply content. */
function isCommandResponse(value: any): boolean {
  return (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    ("success" in value || "replies" in value || "error_message" in value)
  );
}

/** Base class for Kanon out-of-process TypeScript plugins. */
export abstract class Plugin {
  id: string = "org.kanon.plugin.base";
  name: string = "Base TS Plugin";
  version: string = "0.1.0";
  author: string = "Kanon Dev";
  description: string = "Default TypeScript plugin";
  priority: number = 500;
  /** Event kinds to subscribe to besides those of `@OnEvent` handlers (for `onEvent` overrides). */
  events: EventKind[] = [];

  /** Set by the host before `onLoad`, so overriding `onLoad` without `super` is fine. */
  context?: PluginContext;

  private readonly conversations = new Conversations();
  /** Tools added with {@link addTool}, beside the declared ones. */
  private readonly runtimeTools = new Map<string, ToolEntry & { handler: Function }>();

  /** The host's Core handle, or `undefined` in standalone mode. */
  get core(): CoreHandle | undefined {
    return this.context?.core;
  }

  /**
   * The plugin's namespace in the node's central key-value store (see `KV`).
   *
   * @throws In standalone mode, where there is no node to store anything in.
   */
  get kv(): KV {
    if (!this.core) {
      throw new Error("no Core connection: the KV store is unavailable in standalone mode");
    }
    return this.core.kv;
  }

  /** The declarations of this plugin's class, validated. */
  private declared(): Declarations {
    const declared = declarations(Object.getPrototypeOf(this));
    // Core sends commands and triggers through the same RPC, naming either in `command`, so
    // the two share one namespace (a command group is a command).
    const commandNames = new Set([
      ...declared.commands.map((c) => c.name),
      ...declared.subcommands.map((c) => c.group),
    ]);
    const clash = declared.triggers.filter((t) => commandNames.has(t.name)).map((t) => t.name);
    if (clash.length > 0) {
      throw new Error(`names used by both a command and a trigger: ${clash.join(", ")}`);
    }
    const repeated = (keys: string[]) => keys.filter((key, i) => keys.indexOf(key) !== i);
    const twice = [
      ...repeated(declared.subcommands.map((c) => `/${c.group} ${c.name}`)),
      ...repeated(declared.tools.map((t) => `tool ${t.name}`)),
      ...repeated(declared.routes.flatMap((r) => r.methods.map((m) => `${m} ${r.path}`))),
    ];
    if (twice.length > 0) {
      throw new Error(`declared more than once: ${[...new Set(twice)].join(", ")}`);
    }
    return declared;
  }

  /** Every tool the plugin offers right now: the declared ones, then those added at runtime. */
  private tools(): Array<ToolEntry & { handler: Function }> {
    const declared = this.declared().tools.map((t) => ({
      ...t,
      handler: (this as any)[t.methodName] as Function,
    }));
    return [...declared, ...this.runtimeTools.values()];
  }

  /**
   * Offers a new tool to the model while the plugin runs.
   *
   * Use it for tools that depend on configuration or on something discovered at runtime. The
   * node picks the tool up from the next turn on; every change invalidates the provider's prefix
   * cache once, so only call it when something really changed.
   *
   * @param handler Called like a {@link Tool} method, with `this` bound to the plugin.
   * @throws If a tool of that name exists, or if the node could not reread the plugin's tools —
   *   the tool is then not added, so the plugin and the node keep agreeing.
   */
  async addTool(
    name: string,
    options: ToolOptions,
    handler: (args: any, event?: MessageEvent) => any,
  ): Promise<void> {
    const entry = toolEntry(name, options);
    if (this.tools().some((t) => t.name === name)) {
      throw new Error(`tool '${name}' already exists`);
    }
    this.runtimeTools.set(name, { ...entry, handler });
    try {
      await this.core?.refreshMeta();
    } catch (err) {
      this.runtimeTools.delete(name);
      throw err;
    }
  }

  /**
   * Withdraws a tool added with {@link addTool}; resolves whether it existed.
   *
   * @throws If the node could not reread the plugin's tools; the tool then stays, so the plugin
   *   keeps serving what the node still offers. Declared tools cannot be removed.
   */
  async removeTool(name: string): Promise<boolean> {
    const removed = this.runtimeTools.get(name);
    if (removed === undefined) {
      if (this.declared().tools.some((t) => t.name === name)) {
        throw new Error(`tool '${name}' is declared with @Tool and cannot be removed`);
      }
      return false;
    }
    this.runtimeTools.delete(name);
    try {
      await this.core?.refreshMeta();
    } catch (err) {
      this.runtimeTools.set(name, removed);
      throw err;
    }
    return true;
  }

  /** Returns static metadata describing this plugin's identity, commands, and tools. */
  meta(): PluginMeta {
    const declared = this.declared();
    const strip = <T extends { methodName: unknown }>({ methodName, ...rest }: T) => rest;
    const kinds = new Set<EventKind>([...declared.events.map((e) => e.kind), ...this.events]);
    // A group is listed once, where it first appears; declaring @Command("<group>") gives it its
    // description, access and scope, otherwise it gets the defaults.
    const commands: CommandMeta[] = declared.commands.map(strip);
    for (const sub of declared.subcommands) {
      let group = commands.find((c) => c.name === sub.group);
      if (!group) {
        group = {
          name: sub.group,
          description: "",
          usage: "",
          priority: 500,
          aliases: [],
          access: accessValue("everyone"),
          platforms: [],
          conversation_kinds: [],
        };
        commands.push(group);
      }
      group.subcommands = [
        ...(group.subcommands ?? []),
        { name: sub.name, description: sub.description, usage: sub.usage },
      ];
    }
    return {
      id: this.id,
      name: this.name,
      version: this.version,
      author: this.author,
      description: this.description,
      commands,
      triggers: declared.triggers.map(strip),
      tools: this.tools().map((t) => ({
        name: t.name,
        description: t.description,
        parameters: t.parameters ? toProtoStruct(t.parameters) : undefined,
      })),
      events: [...kinds].map((kind) => {
        if (!EVENT_KINDS[kind]) {
          throw new Error(`unknown event kind '${kind}' in ${this.id}.events`);
        }
        return EVENT_KINDS[kind];
      }),
      decorates_replies: declared.decorator !== undefined,
      prepares_turns: declared.preparer !== undefined,
      rewrites_system_prompt: declared.rewriter !== undefined,
      serves_http: declared.routes.length > 0,
    };
  }

  /** Lifecycle hook invoked when the plugin host loads the plugin. */
  async onLoad(ctx: PluginContext): Promise<void> {
    this.context = ctx;
  }

  /**
   * Lifecycle hook invoked when the operator saves a new configuration.
   *
   * The host has already replaced `context.config`; throwing rejects the reload, which is
   * reported back to the console and keeps the Core from persisting the configuration.
   */
  async onConfigReload(config: Record<string, any>): Promise<void> {}

  /** Lifecycle hook invoked prior to plugin unload and host process shutdown. */
  async onUnload(): Promise<void> {}

  /**
   * Pre-filter interceptor hook invoked before command parsing and LLM dispatching.
   * Return null/undefined or { action: 'PASS' } to allow downstream flow.
   * Return { action: 'BLOCK', reply_messages: [...] } to block message processing.
   */
  async onPreFilter(req: any): Promise<any> {
    return null;
  }

  /**
   * Runs a command, trigger or continuation and answers once the handler yields.
   *
   * A continuation resumes the handler suspended in `waitNext` for this conversation; if none
   * is waiting (the plugin captured with an explicit `capture_seconds`, or the host restarted),
   * the handler named by `req.command` is called with `continuation` set.
   * The host supplies the RPC cancellation signal; handlers receive the session's shared signal
   * as `event.signal` and may use it to cancel their own asynchronous work cooperatively.
   */
  async onExecuteCommand(req: any, signal?: AbortSignal): Promise<any> {
    if (req.continuation) {
      const key = new MessageEvent(req.context ?? {}).conversationKey;
      const waiting = this.conversations.take(key);
      if (waiting) {
        const [session, next] = waiting;
        if (signal?.aborted) {
          // Core already consumed this capture, so its suspended handler must be released.
          session.cancel();
          throw session.signal.reason;
        }
        const turn = (session.turn = new Turn());
        next.resolve(new CommandEvent(req, this.core, session));
        return runTurn(turn, session, signal);
      }
    }
    signal?.throwIfAborted();

    const declared = this.declared();
    const subcommands = declared.subcommands.filter((c) => c.group === req.command);
    const sub = subcommands.find((c) => c.name === req.args?.[0]);
    if (sub) {
      // Core routes `/todo add x` to the group `todo`; the subcommand handler sees only `x`.
      req = withoutSubcommand(req);
    }
    const entry =
      sub ??
      declared.commands.find((c) => c.name === req.command) ??
      declared.triggers.find((t) => t.name === req.command);
    if (!entry && subcommands.length > 0) {
      // A bare `/todo`, or an unknown subcommand, with no `@Command("todo")` to take it.
      return {
        success: true,
        replies: [MessageSegment.text(groupHelp(subcommands))],
        error_message: "",
        capture_seconds: 0,
      };
    }
    const handler = entry && (this as any)[entry.methodName];
    if (typeof handler !== "function") {
      return {
        success: false,
        replies: [],
        error_message: `Unknown command: ${req.command}`,
        capture_seconds: 0,
      };
    }

    const session = new Session(this.conversations);
    const turn = (session.turn = new Turn());
    const event = new CommandEvent(req, this.core, session);
    const response = runTurn(turn, session, signal);
    // Not awaited: the handler may outlive this RPC by suspending in waitNext. runHandler never
    // rejects, so nothing is left unhandled.
    void this.runHandler(handler.bind(this), event, session);
    return response;
  }

  /** Runs one command handler to completion, across as many turns as it takes. */
  private async runHandler(
    handler: (event: CommandEvent, args: string[]) => any,
    event: CommandEvent,
    session: Session,
  ): Promise<void> {
    let result: any;
    try {
      result = await handler(event, event.args);
      if (isCommandResponse(result)) {
        // An explicit response: its replies join the turn and its fields are honoured.
        await event.reply(result.replies ?? []);
        if (result.pass_to_model) {
          event.passToModel(result.model_text ?? undefined);
        }
        session.turn?.finish(
          Number(result.capture_seconds ?? 0),
          result.success ?? true,
          result.error_message ?? "",
        );
        return;
      }
      if (result !== undefined && result !== null) {
        await event.reply(result);
      }
      session.turn?.finish();
    } catch (err: any) {
      if (session.signal.aborted) {
        // The RPC already ended. SDK operations reject late work; user promises may still settle.
        return;
      } else if (err instanceof WaitTimeoutError) {
        // The user never answered a waitNext; nothing is waiting for this handler anymore.
        session.turn?.finish();
      } else if (session.turn) {
        session.turn.finish(0, false, err?.message ?? String(err));
      } else {
        console.error(`[kanon-sdk] command '${event.command}' failed:`, err);
      }
    }
  }

  /** Executes an LLM tool call dispatched by the Core microkernel. */
  async onCallTool(req: any): Promise<any> {
    const tool = this.tools().find((t) => t.name === req.tool_name);
    if (!tool || typeof tool.handler !== "function") {
      return {
        call_id: req.call_id,
        success: false,
        error_message: `Unknown tool: ${req.tool_name}`,
      };
    }

    const event = req.context ? new MessageEvent(req.context, this.core) : undefined;
    try {
      let args = req.structured_args ? fromProtoStruct(req.structured_args) : {};
      if (tool.args !== undefined) {
        args = bindArgs(tool.args, args);
      }
      return toolResult(req.call_id, await tool.handler.call(this, args, event));
    } catch (err: any) {
      // The model is told the tool failed and can explain or retry.
      return { call_id: req.call_id, success: false, error_message: err?.message ?? String(err) };
    }
  }

  /**
   * Dispatches a control-plane management action to its `@Action` handler.
   *
   * Unknown actions and handler failures are reported as structured errors rather than
   * thrown through the gRPC layer.
   */
  async onInvokeAction(action: string, parameters: Record<string, any>): Promise<any> {
    const declared = this.declared();
    const entry = declared.actions.find((a) => a.name === action);
    const handler = entry && (this as any)[entry.methodName];
    if (typeof handler !== "function") {
      return {
        success: false,
        error_message:
          `Unknown action '${action}' for plugin '${this.id}'; declared actions: ` +
          JSON.stringify(declared.actions.map((a) => a.name)),
      };
    }
    try {
      const result = await handler.call(this, parameters);
      return {
        success: true,
        error_message: "",
        result: toProtoStruct(result && typeof result === "object" ? result : {}),
      };
    } catch (err: any) {
      return { success: false, error_message: `Action failed: ${err?.message ?? err}` };
    }
  }

  /** Dispatches a lifecycle event to the plugin's `@OnEvent` handlers. */
  async onEvent(req: any): Promise<void> {
    const kind = req?.detail as EventKind | undefined;
    if (!kind) {
      return;
    }
    const detail = kind === "notice" ? new MessageEvent(req.notice, this.core) : req[kind];
    for (const entry of this.declared().events.filter((e) => e.kind === kind)) {
      try {
        await (this as any)[entry.methodName](detail);
      } catch (err) {
        // One failing subscriber must not stop the others.
        console.error(`[kanon-sdk] ${kind} handler failed:`, err);
      }
    }
  }

  /** Runs the plugin's `@DecorateReply` handler on one reply. */
  async onDecorateReply(req: any): Promise<any> {
    const method = this.declared().decorator;
    if (method === undefined) {
      return { modified: false, segments: [] };
    }
    const reply: Reply = {
      event: new MessageEvent(req.context ?? {}, this.core),
      segments: req.segments ?? [],
      source:
        req.source === "REPLY_SOURCE_LLM"
          ? "llm"
          : req.source === "REPLY_SOURCE_COMMAND"
            ? "command"
            : "",
      command: req.command ?? "",
    };
    const result = await (this as any)[method](reply);
    if (result === undefined || result === null) {
      return { modified: false, segments: [] };
    }
    return { modified: true, segments: toSegments(result) };
  }

  /** Runs the plugin's `@PrepareTurn` handler for the turn the model is about to answer. */
  async onPrepareTurn(req: any): Promise<any> {
    const method = this.declared().preparer;
    if (method === undefined) {
      return { text: "" };
    }
    const text = await (this as any)[method](
      new MessageEvent(req.context ?? {}, this.core),
      req.session_id ?? "",
    );
    return { text: text ?? "" };
  }

  /** Runs the plugin's `@OnLlmRequest` handler; an unset `system_prompt` keeps the prompt. */
  async onLlmRequest(req: any): Promise<any> {
    const method = this.declared().rewriter;
    if (method === undefined) {
      return {};
    }
    const result = await (this as any)[method](
      new MessageEvent(req.context ?? {}, this.core),
      req.system_prompt ?? "",
      req.session_id ?? "",
    );
    if (result === undefined || result === null) {
      return {};
    }
    if (typeof result !== "string") {
      throw new TypeError(`@OnLlmRequest must return a string or undefined, got ${typeof result}`);
    }
    return { system_prompt: result };
  }

  /** Routes a gateway request to the plugin's `@HttpRoute` handler for its path and method. */
  async onHttpRequest(req: any): Promise<any> {
    const request = HttpRequest.fromProto(req);
    const routes = this.declared().routes.filter((r) => r.path === request.path);
    if (routes.length === 0) {
      return { status: 404, headers: [], body: Buffer.alloc(0) };
    }
    const route = routes.find((r) => r.methods.includes(request.method as HttpMethod));
    if (!route) {
      const allow = [...new Set(routes.flatMap((r) => r.methods))].sort().join(", ");
      return { status: 405, headers: [{ name: "allow", value: allow }], body: Buffer.alloc(0) };
    }
    try {
      return toHttpResponse(await (this as any)[route.methodName](request)).toProto();
    } catch (err) {
      // The caller learns only that it failed; details may be private and go to the host's log.
      console.error(`[kanon-sdk] HTTP ${request.method} ${request.path} failed:`, err);
      return { status: 500, headers: [], body: Buffer.alloc(0) };
    }
  }

  /**
   * Delivers an outbound message to a target platform.
   *
   * The default implementation is deliberately honest: a plugin with no platform
   * adapter must not report a delivered message that never left the process, because
   * the Core would then record a successful send for a platform that has no outbound
   * path at all. Reporting `success: false` with an explicit reason lets the Core
   * surface the missing adapter to operators. Override this hook to implement
   * outbound delivery for a concrete platform.
   */
  async onDeliverMessage(req: any): Promise<any> {
    return {
      success: false,
      message_id: "",
      error_message:
        `Plugin '${this.id}' does not implement outbound delivery ` +
        `for platform '${req?.platform ?? "unknown"}'`,
    };
  }
}
