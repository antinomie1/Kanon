/** Plugin metadata and decorators shared by every plugin instance. */

import { CommandEvent, MessageEvent } from "./event.js";
import { ArgsSpec, JsonSchema, objectSchema } from "./schema.js";
import { MessageSegmentItem } from "./segments.js";
import { HTTP_METHODS, HttpMethod, HttpRequest } from "./web.js";

import type { CoreHandle } from "./core.js";
/** Access levels a command or trigger may declare; the operator's command policy overrides them. */
export type CommandAccess = "everyone" | "admins_in_groups" | "admins";

const ACCESS_LEVELS: Record<CommandAccess, string> = {
  everyone: "COMMAND_ACCESS_EVERYONE",
  admins_in_groups: "COMMAND_ACCESS_ADMINS_IN_GROUPS",
  admins: "COMMAND_ACCESS_ADMINS",
};

/** Lifecycle events a plugin may subscribe to with {@link OnEvent}. */
export type EventKind =
  | "message_sent"
  | "notice"
  | "llm_response"
  | "agent_begin"
  | "agent_done"
  | "tool_call"
  | "tool_result";

/** Maps SDK lifecycle names to the protobuf enum used by the host. */
export const EVENT_KINDS: Record<EventKind, string> = {
  message_sent: "EVENT_KIND_MESSAGE_SENT",
  notice: "EVENT_KIND_NOTICE",
  llm_response: "EVENT_KIND_LLM_RESPONSE",
  agent_begin: "EVENT_KIND_AGENT_BEGIN",
  agent_done: "EVENT_KIND_AGENT_DONE",
  tool_call: "EVENT_KIND_TOOL_CALL",
  tool_result: "EVENT_KIND_TOOL_RESULT",
};

/** Conversation kinds a command or trigger may be limited to. */
export type ConversationKind = "private" | "group" | "channel";

const CONVERSATION_KINDS: Record<ConversationKind, string> = {
  private: "CONVERSATION_KIND_PRIVATE",
  group: "CONVERSATION_KIND_GROUP",
  channel: "CONVERSATION_KIND_CHANNEL",
};

/** Platform and conversation-kind limits of a command or trigger; empty lists allow all. */
export interface ScopeOptions {
  /** Platforms the handler answers on. Elsewhere Core treats it as undeclared. */
  platforms?: string[];
  /** Conversation kinds the handler answers in. */
  conversationKinds?: ConversationKind[];
}

/** Validates scope options and converts them to their wire form. */
function scopeValue(options?: ScopeOptions): {
  platforms: string[];
  conversation_kinds: string[];
} {
  return {
    platforms: [...(options?.platforms ?? [])],
    conversation_kinds: (options?.conversationKinds ?? []).map((kind) => {
      const value = CONVERSATION_KINDS[kind];
      if (!value) {
        throw new Error(`unknown conversation kind '${kind}'`);
      }
      return value;
    }),
  };
}

/** Wire shape of `CommandMeta`. */
export interface CommandMeta {
  name: string;
  description?: string;
  usage?: string;
  priority?: number;
  aliases?: string[];
  access?: string;
  platforms?: string[];
  conversation_kinds?: string[];
  /** A command group's subcommands (name, description and usage only), listed by `/help`. */
  subcommands?: CommandMeta[];
}

/** Wire shape of `TriggerMeta`. */
export interface TriggerMeta {
  name: string;
  description?: string;
  pattern: string;
  priority?: number;
  access?: string;
  platforms?: string[];
  conversation_kinds?: string[];
}

/** Wire shape of `ToolMeta`. */
export interface ToolMeta {
  name: string;
  description?: string;
  parameters?: Record<string, any>;
}

/** Wire shape of `PluginMeta`. */
export interface PluginMeta {
  id: string;
  name: string;
  version: string;
  author?: string;
  description?: string;
  commands?: CommandMeta[];
  tools?: ToolMeta[];
  triggers?: TriggerMeta[];
  events?: string[];
  decorates_replies?: boolean;
  prepares_turns?: boolean;
  rewrites_system_prompt?: boolean;
  serves_http?: boolean;
}

/** A model conversation as returned by {@link CoreHandle.conversationHistory}. */
export interface ConversationHistory {
  /** The session the conversation is stored under (stable until `/new`). */
  sessionId: string;
  /** Summary of compacted older turns; `""` if never compacted. */
  summary: string;
  /** Oldest first. */
  messages: Array<{ role: "user" | "assistant"; text: string }>;
}

/** A reply about to be delivered, as seen by a {@link DecorateReply} handler. */
export interface Reply {
  /** The message being answered. */
  event: MessageEvent;
  /** The reply's segments. */
  segments: MessageSegmentItem[];
  /** `"llm"` for a model answer, `"command"` for a command or trigger answer. */
  source: "llm" | "command" | "";
  /** The command or trigger name when `source === "command"`. */
  command: string;
}

/** A tool's declaration; `args` is set when the arguments were described with `s`. */
export interface ToolEntry {
  name: string;
  description: string;
  parameters?: JsonSchema;
  args?: ArgsSpec;
}

/** Options of {@link Tool} and {@link Plugin.addTool}; give `args` or `parameters`, not both. */
export interface ToolOptions {
  /** What the tool does, for the model. */
  description?: string;
  /**
   * The arguments, built with `s` (see `schema.ts`). The SDK fills in defaults and rejects unknown
   * or missing arguments before the handler runs.
   */
  args?: ArgsSpec;
  /** A hand-written JSON Schema of the argument object, passed through unchecked. */
  parameters?: JsonSchema;
}

/** Validates tool options and resolves them into a declaration. */
export function toolEntry(name: string, options: ToolOptions = {}): ToolEntry {
  if (!name) {
    throw new Error("a tool needs a name");
  }
  if (options.args !== undefined && options.parameters !== undefined) {
    throw new Error(`tool '${name}': give args or parameters, not both`);
  }
  return {
    name,
    description: options.description ?? "",
    parameters: options.args !== undefined ? objectSchema(options.args) : options.parameters,
    args: options.args,
  };
}

/** Declarations collected by the decorators, kept per class. */
export interface Declarations {
  commands: Array<CommandMeta & { methodName: string | symbol }>;
  /** Subcommands of command groups, in declaration order. */
  subcommands: Array<{
    group: string;
    name: string;
    description: string;
    usage: string;
    methodName: string | symbol;
  }>;
  triggers: Array<TriggerMeta & { methodName: string | symbol }>;
  tools: Array<ToolEntry & { methodName: string | symbol }>;
  actions: Array<{ name: string; methodName: string | symbol }>;
  events: Array<{ kind: EventKind; methodName: string | symbol }>;
  routes: Array<{ path: string; methods: HttpMethod[]; methodName: string | symbol }>;
  decorator?: string | symbol;
  preparer?: string | symbol;
  rewriter?: string | symbol;
}

const DECLARATIONS = Symbol("kanon.declarations");

/**
 * Returns the declarations of `prototype`'s own class, starting from a copy of its parent's.
 *
 * Copying instead of sharing keeps a subclass's decorators from leaking into its base class
 * and into sibling subclasses.
 */
export function declarations(prototype: any): Declarations {
  if (!Object.prototype.hasOwnProperty.call(prototype, DECLARATIONS)) {
    const inherited: Declarations | undefined = prototype[DECLARATIONS];
    prototype[DECLARATIONS] = {
      commands: [...(inherited?.commands ?? [])],
      subcommands: [...(inherited?.subcommands ?? [])],
      triggers: [...(inherited?.triggers ?? [])],
      tools: [...(inherited?.tools ?? [])],
      actions: [...(inherited?.actions ?? [])],
      events: [...(inherited?.events ?? [])],
      routes: [...(inherited?.routes ?? [])],
      decorator: inherited?.decorator,
      preparer: inherited?.preparer,
      rewriter: inherited?.rewriter,
    };
  }
  return prototype[DECLARATIONS];
}

export function accessValue(level: CommandAccess | undefined): string {
  const value = ACCESS_LEVELS[level ?? "everyone"];
  if (!value) {
    throw new Error(`unknown access level '${level}'`);
  }
  return value;
}

/**
 * Declares a slash command handler, or a subcommand of a command group.
 *
 * The handler receives a {@link CommandEvent} and its arguments, and answers by returning text
 * or segments, or with `await event.reply(...)`.
 *
 * A name with a space declares a subcommand: `@Command("todo add")` answers `/todo add milk`
 * with `event.args` = `["milk"]`. `/help` lists a group's subcommands under it, and `/todo` alone
 * (or with an unknown subcommand) answers with that list — unless the plugin also declares
 * `@Command("todo")`, which then handles those cases and carries the group's description,
 * aliases, access level, scope and priority. Subcommands take only `description` and `usage`:
 * the node routes and checks access for the group as a whole.
 *
 * @param name Command name without the slash, or `"<group> <subcommand>"`.
 * @param options.aliases Other names that invoke the command; the handler always sees `name`.
 * @param options.access Default access level, which the operator can override.
 * @param options.priority Lower wins when several plugins declare the same name.
 * @param options.platforms Platforms the command answers on; empty means all. Elsewhere Core
 *   treats it as undeclared, so another plugin's command of the same name may answer.
 * @param options.conversationKinds Conversation kinds the command answers in; empty means all.
 */
export function Command(
  name: string,
  options?: {
    description?: string;
    usage?: string;
    priority?: number;
    aliases?: string[];
    access?: CommandAccess;
  } & ScopeOptions,
): MethodDecorator {
  const words = name.replace(/^\//, "").trim().split(/\s+/);
  if (words.length > 2 || !words[0]) {
    throw new Error(`command name '${name}' must be "<name>" or "<group> <subcommand>"`);
  }
  if (words.length === 2) {
    const [group, sub] = words;
    const extra = Object.keys(options ?? {}).filter((k) => k !== "description" && k !== "usage");
    if (extra.length > 0) {
      throw new Error(
        `subcommand '${group} ${sub}' takes only description and usage; set ${extra.join(", ")} ` +
          `on @Command("${group}")`,
      );
    }
    return (target: any, propertyKey: string | symbol) => {
      declarations(target).subcommands.push({
        group,
        name: sub,
        description: options?.description ?? "",
        usage: options?.usage ?? `/${group} ${sub}`,
        methodName: propertyKey,
      });
    };
  }
  const access = accessValue(options?.access);
  const scope = scopeValue(options);
  return (target: any, propertyKey: string | symbol) => {
    const canonical = words[0];
    declarations(target).commands.push({
      methodName: propertyKey,
      name: canonical,
      description: options?.description ?? "",
      usage: options?.usage ?? `/${canonical}`,
      priority: options?.priority ?? 500,
      aliases: (options?.aliases ?? []).map((alias) => alias.replace(/^\//, "")),
      access,
      ...scope,
    });
  };
}

/**
 * Declares a handler for plain messages matching a regular expression.
 *
 * Core matches the pattern (Rust `regex` syntax, which shares JavaScript's common subset)
 * against the message text; the handler's `event.args` holds the capture groups, with `""` for
 * a group that did not participate. Triggers run after slash commands and before the model.
 *
 * @param pattern Regular expression; anchor it (`^...$`) unless it may match anywhere.
 * @param options.name Name used for routing and logs; defaults to the method name.
 * @param options.description Shown in `/help`; leave empty to keep the trigger unlisted.
 * @param options.platforms As for {@link Command}; elsewhere the trigger never matches.
 * @param options.conversationKinds As for {@link Command}.
 */
export function Trigger(
  pattern: string,
  options?: {
    name?: string;
    description?: string;
    priority?: number;
    access?: CommandAccess;
  } & ScopeOptions,
): MethodDecorator {
  const access = accessValue(options?.access);
  const scope = scopeValue(options);
  // Validate early: a pattern JavaScript rejects is almost certainly a mistake, and an invalid
  // pattern would otherwise only show up as a warning in Core's log.
  new RegExp(pattern);
  return (target: any, propertyKey: string | symbol) => {
    declarations(target).triggers.push({
      methodName: propertyKey,
      name: options?.name ?? String(propertyKey),
      description: options?.description ?? "",
      pattern,
      priority: options?.priority ?? 500,
      access,
      ...scope,
    });
  };
}

/**
 * Declares an LLM tool call handler.
 *
 * ```ts
 * @Tool("weather", {
 *   description: "Current weather for a city.",
 *   args: { city: s.string("City name"), days: s.integer("Days of forecast").default(1) },
 * })
 * async weather({ city, days }: { city: string; days: number }, event?: MessageEvent) { ... }
 * ```
 *
 * The handler receives the model's arguments as one object and the {@link MessageEvent} the
 * model was answering (`undefined` when the call did not come from a chat message), so a tool
 * knows who asked without trusting the model to pass it along. It returns the result: an object
 * as is, any other JSON value as `{ result: value }`, a `Buffer` as raw bytes. A thrown error
 * becomes a failed call the model can explain or retry. Operator-only operations belong in
 * {@link Action} instead.
 *
 * @param name The tool's name for the model (or the older `{ name, description, parameters }`).
 * @param options See {@link ToolOptions}.
 */
export function Tool(
  name: string | ({ name: string } & ToolOptions),
  options?: ToolOptions,
): MethodDecorator {
  const entry = typeof name === "string" ? toolEntry(name, options) : toolEntry(name.name, name);
  return (target: any, propertyKey: string | symbol) => {
    declarations(target).tools.push({ ...entry, methodName: propertyKey });
  };
}

/**
 * Declares a management action invoked by the control plane
 * (`POST /api/v1/plugins/{id}/actions/{action}`).
 *
 * Actions are never advertised to the model, so credential binding and similar flows cannot
 * be triggered by a chat message. The handler receives the parameters and returns a JSON
 * object (or nothing).
 */
export function Action(name: string): MethodDecorator {
  return (target: any, propertyKey: string | symbol) => {
    declarations(target).actions.push({ name, methodName: propertyKey });
  };
}

/**
 * Subscribes a handler to a lifecycle event.
 *
 * - `"message_sent"`: the `MessageSentEvent` for every message the bot delivered;
 * - `"notice"`: a {@link MessageEvent} for a platform notice (join, poke, recall, ...);
 * - `"llm_response"`: the `LlmResponseEvent` with the model's answer and the message it answered;
 * - `"agent_begin"` / `"agent_done"`: the `AgentBeginEvent` / `AgentDoneEvent` when the agent
 *   starts and finishes a conversation turn (`done` carries `success`, the answer or `error`, and
 *   the `tools` it called);
 * - `"tool_call"` / `"tool_result"`: the `ToolCallEvent` / `ToolResultEvent` for every tool the
 *   agent calls during a turn, with its `arguments` (a `Struct`) and its `result`.
 *
 * Each event's `context` is the chat message behind it (wrap it in {@link MessageEvent} to use
 * the helpers). Events are notifications: Core never waits on them and ignores the return value.
 */
export function OnEvent(kind: EventKind): MethodDecorator {
  if (!EVENT_KINDS[kind]) {
    throw new Error(`unknown event kind '${kind}'`);
  }
  return (target: any, propertyKey: string | symbol) => {
    declarations(target).events.push({ kind, methodName: propertyKey });
  };
}

/**
 * Marks the plugin's reply decorator.
 *
 * The handler receives a {@link Reply} and returns `undefined` to leave it alone, or new
 * content to replace it; an empty list suppresses the reply. Core gives each decorator three
 * seconds and keeps the reply unchanged if it fails or times out. Decoration never changes
 * what the model remembers saying.
 */
export function DecorateReply(): MethodDecorator {
  return (target: any, propertyKey: string | symbol) => {
    const declared = declarations(target);
    if (declared.decorator !== undefined && declared.decorator !== propertyKey) {
      throw new Error("a plugin may declare only one @DecorateReply handler");
    }
    declared.decorator = propertyKey;
  };
}

/**
 * Marks the plugin's turn preparer.
 *
 * Before the model answers a message, the handler receives the {@link MessageEvent} and the
 * conversation's session id and returns text to prepend to the current user message (retrieved
 * knowledge, long-term memory); `undefined` or `""` adds nothing. The text never reaches the
 * system prompt, so the cached request prefix stays stable, and it becomes part of the
 * conversation history. Core gives each preparer three seconds and goes ahead without it on
 * error or timeout.
 */
export function PrepareTurn(): MethodDecorator {
  return (target: any, propertyKey: string | symbol) => {
    const declared = declarations(target);
    if (declared.preparer !== undefined && declared.preparer !== propertyKey) {
      throw new Error("a plugin may declare only one @PrepareTurn handler");
    }
    declared.preparer = propertyKey;
  };
}

/**
 * Marks the plugin's system prompt rewriter.
 *
 * Before the model answers the first message of a turn, the handler receives the
 * {@link MessageEvent}, the current system prompt and the session id, and returns the new
 * system prompt, or `undefined` to keep it:
 *
 * ```ts
 * @OnLlmRequest()
 * async rules(event: MessageEvent, systemPrompt: string) {
 *   const rules = await this.kv.get(`rules:${event.channelId}`);
 *   return rules ? `${systemPrompt}\n\nRules for this chat:\n${rules}` : undefined;
 * }
 * ```
 *
 * The result must be deterministic for a session: the system prompt leads every request and
 * decides the provider's prefix cache, so per-message content (time, counters, retrieved
 * snippets) belongs in {@link PrepareTurn}. Core asks plugins one after another, each seeing the
 * previous result, and gives each three seconds; a failing plugin is skipped. Only conversation
 * turns are rewritten — never the console chat, `requestLlm` or a private `runAgent`.
 */
export function OnLlmRequest(): MethodDecorator {
  return (target: any, propertyKey: string | symbol) => {
    const declared = declarations(target);
    if (declared.rewriter !== undefined && declared.rewriter !== propertyKey) {
      throw new Error("a plugin may declare only one @OnLlmRequest handler");
    }
    declared.rewriter = propertyKey;
  };
}

/**
 * Declares a handler for HTTP requests to `path` below the plugin's `http/` root
 * (`/api/v1/plugins/<id>/http/...`); see `web.ts`.
 *
 * The handler receives an {@link HttpRequest} and returns an `HttpResponse` or a plain value (see
 * `toHttpResponse`). Paths match exactly; a known path with another method answers `405`, an
 * unknown path `404`, and a handler that throws `500` (the error is logged to the host's stderr,
 * never sent to the caller).
 *
 * @param path Path starting with `/`, e.g. `"/stats"`; `"/"` is the root itself.
 * @param options.methods Methods the handler accepts; `["GET"]` by default.
 */
export function HttpRoute(path: string, options?: { methods?: HttpMethod[] }): MethodDecorator {
  if (!path.startsWith("/")) {
    throw new Error(`HttpRoute path must start with '/', got '${path}'`);
  }
  const methods = (options?.methods ?? ["GET"]).map((m) => m.toUpperCase() as HttpMethod);
  const unknown = methods.filter((m) => !HTTP_METHODS.includes(m));
  if (methods.length === 0 || unknown.length > 0) {
    throw new Error(
      `unknown HTTP methods ${JSON.stringify(unknown)}; expected some of ${HTTP_METHODS}`,
    );
  }
  return (target: any, propertyKey: string | symbol) => {
    declarations(target).routes.push({ path, methods, methodName: propertyKey });
  };
}
