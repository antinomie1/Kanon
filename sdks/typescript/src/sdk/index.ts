/**
 * Kanon Official TypeScript SDK.
 *
 * Provides base classes, decorators, and context abstractions for authoring
 * out-of-process Kanon plugins in TypeScript or JavaScript.
 */

import { randomUUID } from "node:crypto";
import * as fs from "node:fs";
import * as path from "node:path";
import * as grpc from "@grpc/grpc-js";
import * as protoLoader from "@grpc/proto-loader";

import {
  CommandEvent,
  Conversations,
  MessageEvent,
  Session,
  Turn,
  WaitTimeoutError,
  runTurn,
} from "./event.js";
import { LlmMessage, MessageSegmentItem, Replyable, toSegments } from "./segments.js";
import { fromProtoStruct, fromProtoValue, toProtoStruct } from "./struct.js";

export interface PluginContext {
  /** Dedicated filesystem directory for this plugin's local persistent storage. */
  dataDir: string;
  /** Active configuration dictionary passed from the Core microkernel. */
  config: Record<string, any>;
  /**
   * Shared handle onto the Core microkernel's `BotApiService`, used to push inbound
   * platform events into the Core pipeline (see {@link CoreHandle.ingestEvent}).
   *
   * The host process creates exactly one handle for the whole process, so every task
   * inside a plugin fans its inbound traffic into the same HTTP/2 channel instead of
   * opening a socket per adapter task; gRPC multiplexes concurrent unary calls over
   * one connection, which makes this fan-in cheap. Because the handle is shared,
   * callers must not mutate it and must not close it themselves: the host owns its
   * lifecycle and closes it during shutdown.
   *
   * `undefined` means the host is running in standalone mode (no `KANON_CORE_SOCK`,
   * or the Core endpoint was unreachable at startup). In that case there is no
   * channel to ingest through, and the plugin must fail explicitly rather than
   * pretend that inbound ingestion is available.
   */
  core?: CoreHandle;
}

/**
 * Result of `BotApiService.IngestEvent`, mapped from the protobuf response
 * fields `accepted` / `event_id`.
 */
export interface IngestEventResponse {
  /**
   * Whether the Core accepted the event into its bounded ingest queue.
   *
   * Callers MUST check this flag. The Core deliberately reports backpressure as a
   * value rather than as an RPC error: when its high watermark is reached it drops
   * the event and answers `accepted: false` so that an IM adapter can apply its own
   * backpressure (pause or slow down reads from the platform socket) instead of
   * assuming the message was ingested. A resolved promise therefore only proves that
   * the Core was reachable, not that the event survived.
   */
  accepted: boolean;
  /** Event id the Core acknowledges, echoed verbatim from the request. */
  eventId: string;
}

/** Wire shape of `kanon.plugin.v1.PipelineEventRequest` (snake_case field names). */
interface PipelineEventPayload {
  event_id: string;
  platform: string;
  channel_id: string;
  sender_id: string;
  raw_text: string;
  metadata?: { fields: Record<string, any> };
  segments?: Array<Record<string, any>>;
}

/** Wire shape of `kanon.plugin.v1.IngestEventRequest`. */
interface IngestEventRequestPayload {
  platform: string;
  event: PipelineEventPayload;
}

/** Raw `kanon.plugin.v1.IngestEventResponse` as decoded by the dynamic client. */
interface RawIngestEventResponse {
  accepted?: boolean;
  event_id?: string;
}

/** Wire shape of `kanon.plugin.v1.RegisterHostRequest`. */
interface RegisterHostRequestPayload {
  host_id: string;
  runtime: string;
  endpoint: string;
  loaded_plugin_ids: string[];
}

/** Raw `kanon.plugin.v1.RegisterHostResponse` as decoded by the dynamic client. */
interface RawRegisterHostResponse {
  success?: boolean;
  message?: string;
}

/**
 * Structural view of the dynamically generated `kanon.plugin.v1.BotApiService`
 * client.
 *
 * `@grpc/proto-loader` produces untyped service constructors at runtime, so the SDK
 * pins down only the handful of members it actually uses instead of falling back to
 * `any` everywhere (the same dynamic-object caveat the host lives with).
 */
interface BotApiServiceClient {
  IngestEvent(
    request: IngestEventRequestPayload,
    callback: (
      error: grpc.ServiceError | null,
      response?: RawIngestEventResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  RegisterHost(
    request: RegisterHostRequestPayload,
    callback: (
      error: grpc.ServiceError | null,
      response?: RawRegisterHostResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  Ping(
    request: { timestamp: number },
    callback: (error: grpc.ServiceError | null) => void,
  ): grpc.ClientUnaryCall;
  waitForReady(deadline: grpc.Deadline, callback: (error?: Error) => void): void;
  close(): void;
  /** Other unary and streaming methods, called by name; see the `CoreHandle` wrappers. */
  [method: string]: any;
}

/** Options of {@link CoreHandle.requestLlm}; set exactly one of `prompt` and `messages`. */
export interface LlmRequestOptions {
  /** A single user turn. */
  prompt?: string;
  /** A whole exchange, oldest first (see `llmMessage`). */
  messages?: LlmMessage[];
  systemPrompt?: string;
  /** `<provider>/<model-id>`; empty uses the node's default model. */
  model?: string;
  temperature?: number;
  maxTokens?: number;
}

/**
 * Shared client handle for the Core microkernel's `BotApiService`.
 *
 * A `CoreHandle` owns exactly one gRPC channel to the Core IPC endpoint. Host
 * processes create one handle and share it with every plugin they load: the channel
 * multiplexes concurrent RPCs, so a platform adapter plugin can run many concurrent
 * inbound tasks (one per chat update) that all ingest through this single handle
 * without exhausting file descriptors or paying a reconnect per message.
 *
 * Inbound ingestion is the primary use case: a platform adapter receives updates from
 * its platform SDK and forwards them with {@link CoreHandle.ingestEvent}. Outbound
 * delivery stays on the plugin side through `Plugin.onDeliverMessage`.
 */
export class CoreHandle {
  /** Dynamically generated BotApiService client bound to this handle's channel. */
  private readonly client: BotApiServiceClient;
  /** Filesystem path of the target when it is a Unix domain socket, else undefined. */
  private readonly unixSocketPath?: string;

  /**
   * Creates a handle for one Core IPC endpoint.
   *
   * @param endpoint Core `BotApiService` endpoint. A bare filesystem path (the value
   *   the supervisor passes in `KANON_CORE_SOCK`, e.g. `./run/core.sock`) is treated
   *   as a Unix domain socket and resolved to an absolute `unix:` target; explicit
   *   grpc-js targets (`unix:/path`, and loopback `host:port` used on Windows) are
   *   passed through unchanged.
   * @param credentials Channel credentials for that endpoint. Defaults to
   *   `grpc.credentials.createInsecure()`, which mirrors how the host binds its own
   *   IPC server: local IPC sockets are protected by filesystem permissions (0700 on
   *   the run directory) rather than by TLS. Callers that connect over a transport
   *   requiring authentication pass the matching `grpc.ChannelCredentials` here.
   */
  constructor(
    endpoint: string,
    credentials: grpc.ChannelCredentials = grpc.credentials.createInsecure(),
  ) {
    const descriptor = loadKanonProto();
    const botApiService = (descriptor as any).kanon?.plugin?.v1?.BotApiService;
    if (!botApiService) {
      throw new Error(
        "kanon.plugin.v1.BotApiService is missing from the loaded proto descriptor",
      );
    }

    const target = normalizeCoreEndpoint(endpoint);
    this.unixSocketPath = target.startsWith("unix:")
      ? target.slice("unix:".length)
      : undefined;
    this.client = new botApiService(target, credentials) as BotApiServiceClient;
  }

  /**
   * Pushes one inbound platform message into the Core pipeline.
   *
   * The event is wrapped in an `IngestEventRequest` carrying a nested
   * `PipelineEventRequest`; the Core answers with a Fast-ACK as soon as the event sits
   * in its bounded queue, so this call is cheap to await from a hot inbound path.
   *
   * @param platform Platform identifier of the adapter (e.g. `"demo"`, `"telegram"`).
   * @param channelId Channel / room / conversation the message arrived in.
   * @param senderId Platform user id of the message author.
   * @param text Raw message text as received from the platform.
   * @param eventId Optional caller-supplied event id. When omitted, a `randomUUID()`
   *   is generated locally so the Core can echo it for tracing and de-duplication.
   * @param metadata Optional adapter-specific extras; converted to a
   *   `google.protobuf.Struct` for transport. The platform-neutral reply-policy keys
   *   (`kanon.conversation_kind`, `kanon.bot_mentioned`) belong here.
   * @param segments Optional rich-media segments in proto-JSON shape (for example
   *   `{ text: { content: "hi" } }` or `{ image: { url: "https://…" } }`). The Core
   *   builds the model-visible message from them, so an adapter that omits them can
   *   only deliver plain text — and a picture-only message would look empty.
   * @returns The mapped response. `accepted: false` means the Core's ingest queue was
   *   at its high watermark and the event was dropped, so callers MUST check it and
   *   apply their own backpressure.
   * @throws When the RPC itself fails (Core unreachable, deadline exceeded, core
   *   error status). Failures are never converted into a fabricated success.
   */
  async ingestEvent(
    platform: string,
    channelId: string,
    senderId: string,
    text: string,
    eventId?: string,
    metadata?: Record<string, any>,
    segments?: Array<Record<string, any>>,
  ): Promise<IngestEventResponse> {
    const resolvedEventId = eventId ?? randomUUID();

    const event: PipelineEventPayload = {
      event_id: resolvedEventId,
      platform,
      channel_id: channelId,
      sender_id: senderId,
      raw_text: text,
    };
    if (metadata !== undefined) {
      event.metadata = toProtoStruct(metadata);
    }
    if (segments !== undefined && segments.length > 0) {
      // `keepCase` makes the dynamic client accept the proto-JSON field names verbatim, so the
      // repeated `MessageSegment` is transmitted as-is.
      event.segments = segments;
    }

    const response = await new Promise<RawIngestEventResponse>(
      (resolve, reject) => {
        this.client.IngestEvent({ platform, event }, (error, value) => {
          if (error) {
            // Mechanical failure: reject so the caller can retry or drop the message
            // instead of believing the event reached the pipeline.
            reject(error);
            return;
          }
          if (!value) {
            reject(new Error("Core returned an empty IngestEvent response"));
            return;
          }
          resolve(value);
        });
      },
    );

    return {
      // `accepted` is the Core's backpressure verdict, never inferred locally.
      accepted: response.accepted === true,
      // Echoed by the Core; the SDK does not invent an id the Core never acknowledged.
      eventId: response.event_id ?? "",
    };
  }

  /**
   * Replies to an inbound event and waits for the platform's delivery result
   * (`BotApiService.ReplyMessage`).
   *
   * Success here is the adapter's delivery outcome, not mere queue admission. An RPC failure or
   * timeout is ambiguous — the message may have gone out — so never retry a reply automatically.
   *
   * @param event The `PipelineEventRequest` being answered.
   * @param content Text, a segment, or a list of either.
   */
  async replyTo(event: any, content: Replyable): Promise<any> {
    return this.unary(
      "ReplyMessage",
      {
        platform: event.platform,
        channel_id: event.channel_id,
        recipient_id: event.sender_id,
        event_id: event.event_id,
        segments: toSegments(content),
      },
      35_000,
    );
  }

  /**
   * Sends a message on the bot's own initiative: reminders, broadcasts, subscriptions
   * (`BotApiService.SendMessage`).
   *
   * Success means Core accepted the message into its outbound queue, not that the platform
   * delivered it; use {@link replyTo} when the delivery outcome matters.
   *
   * @param channelId Conversation to send to, as events report it (`"group:123"`).
   */
  async sendMessage(
    platform: string,
    channelId: string,
    content: Replyable,
    recipientId = "",
  ): Promise<{ success: boolean; accepted: boolean; message_id: string; error_message: string }> {
    return this.unary("SendMessage", {
      platform,
      channel_id: channelId,
      recipient_id: recipientId,
      segments: toSegments(content),
    });
  }

  /**
   * Asks the node's model one question and returns the complete answer.
   *
   * The call is independent of every chat conversation: nothing is read from or written to any
   * session's memory. Pass a prompt string (one user turn) or `messages` (a whole exchange,
   * oldest first; see `llmMessage`).
   *
   * @throws A gRPC error: `UNAVAILABLE` when the node has no model configured,
   *   `INVALID_ARGUMENT` for messages the model cannot take.
   */
  async requestLlm(prompt: string | LlmRequestOptions): Promise<string> {
    let answer = "";
    for await (const delta of this.streamLlm(prompt)) {
      answer += delta;
    }
    return answer;
  }

  /** Like {@link requestLlm}, but yields the answer as it is generated. */
  async *streamLlm(prompt: string | LlmRequestOptions): AsyncGenerator<string> {
    const options: LlmRequestOptions = typeof prompt === "string" ? { prompt } : prompt;
    if ((options.prompt === undefined) === (options.messages === undefined)) {
      throw new Error("pass exactly one of prompt and messages");
    }
    const request: Record<string, any> = {
      model: options.model ?? "",
      system_prompt: options.systemPrompt ?? "",
      messages:
        options.prompt !== undefined
          ? [{ role: "LLM_ROLE_USER", text: options.prompt, images: [] }]
          : options.messages,
    };
    // Optional scalars: leaving them unset keeps the provider's own defaults.
    if (options.temperature !== undefined) {
      request.temperature = options.temperature;
    }
    if (options.maxTokens !== undefined) {
      request.max_tokens = options.maxTokens;
    }
    // A grpc-js server stream is an async iterable; a stream error surfaces as a throw here.
    for await (const chunk of this.client.RequestLLM(request)) {
      if (chunk?.delta_text) {
        yield chunk.delta_text as string;
      }
    }
  }

  /**
   * Calls one action of a built-in adapter's platform API and returns its result
   * (`BotApiService.CallPlatformApi`).
   *
   * This reaches what the generic contract does not model, e.g. OneBot's
   * `get_group_member_list` or Milky's `set_group_member_mute`. The result is plain JSON data;
   * `null` when the action returns nothing.
   *
   * @throws A gRPC error: `NOT_FOUND` for an unknown platform, `UNIMPLEMENTED` when the adapter
   *   offers no API, `UNAVAILABLE` when the platform refused the call.
   */
  async callPlatformApi(
    platform: string,
    action: string,
    params: Record<string, any> = {},
  ): Promise<any> {
    const response = await this.unary("CallPlatformApi", {
      platform,
      action,
      params: toProtoStruct(params),
    });
    return response?.result ? fromProtoValue(response.result) : null;
  }

  /** Issues one unary RPC on the shared channel. */
  private unary(method: string, request: any, timeoutMs?: number): Promise<any> {
    return new Promise((resolve, reject) => {
      const callback = (error: grpc.ServiceError | null, value?: any) =>
        error ? reject(error) : resolve(value);
      if (timeoutMs === undefined) {
        this.client[method](request, callback);
      } else {
        this.client[method](request, { deadline: Date.now() + timeoutMs }, callback);
      }
    });
  }

  /**
   * Announces this host process to the Core (`BotApiService.RegisterHost`).
   *
   * Called by the host after its own IPC endpoint is bound, so the endpoint in the
   * registration is already reachable. Plugins normally do not call this.
   *
   * @throws When the RPC fails or the Core rejects the registration.
   */
  async registerHost(
    hostId: string,
    endpoint: string,
    loadedPluginIds: string[],
  ): Promise<void> {
    const response = await new Promise<RawRegisterHostResponse>(
      (resolve, reject) => {
        this.client.RegisterHost(
          {
            host_id: hostId,
            // This host runtime; the Core records it in its host registry.
            runtime: "typescript",
            endpoint,
            loaded_plugin_ids: loadedPluginIds,
          },
          (error, value) => {
            if (error) {
              reject(error);
              return;
            }
            if (!value) {
              reject(new Error("Core returned an empty RegisterHost response"));
              return;
            }
            resolve(value);
          },
        );
      },
    );

    if (response.success !== true) {
      throw new Error(
        `Core rejected host registration: ${response.message ?? "no message"}`,
      );
    }
  }

  /**
   * Probes the Core's liveness (`BotApiService.Ping`).
   *
   * Deliberately trivial so a host can tell "Core is gone" apart from "Core is busy";
   * used by the core-liveness watchdog.
   *
   * @throws When the RPC fails or times out.
   */
  async ping(): Promise<void> {
    await new Promise<void>((resolve, reject) => {
      this.client.Ping(
        { timestamp: Date.now() },
        (error) => (error ? reject(error) : resolve()),
      );
    });
  }

  /**
   * Waits until the Core endpoint is actually reachable.
   *
   * Used by the host before handing this handle to a plugin, so a dead
   * `KANON_CORE_SOCK` results in standalone mode instead of a handle that only fails
   * on the first inbound message.
   *
   * @param timeoutMs Maximum time to wait for the channel to become READY.
   * @returns `true` when the channel is READY, `false` when the target Unix socket
   *   file does not exist or the timeout elapses first.
   */
  async waitForReady(timeoutMs: number): Promise<boolean> {
    // A missing Unix socket can never become ready; returning immediately avoids
    // burning the whole deadline inside the gRPC reconnect backoff.
    if (this.unixSocketPath !== undefined && !fs.existsSync(this.unixSocketPath)) {
      return false;
    }

    return new Promise<boolean>((resolve) => {
      this.client.waitForReady(Date.now() + timeoutMs, (error?: Error) => {
        resolve(!error);
      });
    });
  }

  /** Closes the underlying channel; no further RPC may be issued afterwards. */
  close(): void {
    this.client.close();
  }
}

/** Matches grpc-js targets that already carry an explicit scheme. */
const SCHEMED_TARGET = /^(unix|dns|ipv4|ipv6):/i;
/** Matches a bare loopback `host:port` target such as `127.0.0.1:50051`. */
const HOST_PORT_TARGET = /^[A-Za-z0-9._-]+:\d+$/;

/**
 * Normalizes a Core endpoint into a grpc-js target string.
 *
 * `KANON_CORE_SOCK` holds a filesystem path (the Core only ever writes `core.sock`),
 * so bare paths are promoted to the `unix:` scheme and made absolute; explicit
 * grpc-js targets are passed through so Windows loopback TCP endpoints keep working.
 */
function normalizeCoreEndpoint(endpoint: string): string {
  if (SCHEMED_TARGET.test(endpoint) || HOST_PORT_TARGET.test(endpoint)) {
    return endpoint;
  }
  return `unix:${path.resolve(endpoint)}`;
}

/** Shared proto-loader options, so every consumer decodes the IDL identically. */
const PROTO_LOADER_OPTIONS: protoLoader.Options = {
  // keepCase preserves the snake_case names written in the IDL: the wire contract
  // then reads exactly like plugin.proto in host, SDK, and plugins alike.
  keepCase: true,
  longs: String,
  enums: String,
  defaults: true,
  oneofs: true,
};

/** Locates the canonical proto IDL file across workspaces. */
export function findProtoPath(): string {
  let current = __dirname;
  for (let i = 0; i < 6; i++) {
    const candidate = path.join(current, "proto/kanon/v1/plugin.proto");
    if (fs.existsSync(candidate)) {
      return candidate;
    }
    current = path.dirname(current);
  }

  current = process.cwd();
  for (let i = 0; i < 6; i++) {
    const candidate = path.join(current, "proto/kanon/v1/plugin.proto");
    if (fs.existsSync(candidate)) {
      return candidate;
    }
    current = path.dirname(current);
  }

  throw new Error("Cannot locate proto/kanon/v1/plugin.proto");
}

/** Cached package descriptor; loading and parsing the IDL once per process is enough. */
let cachedProtoDescriptor: grpc.GrpcObject | undefined;

/**
 * Loads the Kanon proto package descriptor, memoized per process.
 *
 * Both the host (server-side services) and {@link CoreHandle} (client-side
 * BotApiService) build their stubs from this single descriptor, so the two sides can
 * never disagree about field naming or loader options.
 */
export function loadKanonProto(): grpc.GrpcObject {
  if (!cachedProtoDescriptor) {
    const protoFile = findProtoPath();
    cachedProtoDescriptor = grpc.loadPackageDefinition(
      protoLoader.loadSync(protoFile, {
        ...PROTO_LOADER_OPTIONS,
        includeDirs: [path.dirname(path.dirname(path.dirname(protoFile)))],
      }),
    );
  }
  return cachedProtoDescriptor;
}

/** Access levels a command or trigger may declare; the operator's command policy overrides them. */
export type CommandAccess = "everyone" | "admins_in_groups" | "admins";

const ACCESS_LEVELS: Record<CommandAccess, string> = {
  everyone: "COMMAND_ACCESS_EVERYONE",
  admins_in_groups: "COMMAND_ACCESS_ADMINS_IN_GROUPS",
  admins: "COMMAND_ACCESS_ADMINS",
};

/** Lifecycle events a plugin may subscribe to with {@link OnEvent}. */
export type EventKind = "message_sent" | "notice" | "llm_response";

const EVENT_KINDS: Record<EventKind, string> = {
  message_sent: "EVENT_KIND_MESSAGE_SENT",
  notice: "EVENT_KIND_NOTICE",
  llm_response: "EVENT_KIND_LLM_RESPONSE",
};

/** Wire shape of `CommandMeta`. */
export interface CommandMeta {
  name: string;
  description?: string;
  usage?: string;
  priority?: number;
  aliases?: string[];
  access?: string;
}

/** Wire shape of `TriggerMeta`. */
export interface TriggerMeta {
  name: string;
  description?: string;
  pattern: string;
  priority?: number;
  access?: string;
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

/** Declarations collected by the decorators, kept per class. */
interface Declarations {
  commands: Array<CommandMeta & { methodName: string | symbol }>;
  triggers: Array<TriggerMeta & { methodName: string | symbol }>;
  tools: Array<ToolMeta & { methodName: string | symbol }>;
  actions: Array<{ name: string; methodName: string | symbol }>;
  events: Array<{ kind: EventKind; methodName: string | symbol }>;
  decorator?: string | symbol;
}

const DECLARATIONS = Symbol("kanon.declarations");

/**
 * Returns the declarations of `prototype`'s own class, starting from a copy of its parent's.
 *
 * Copying instead of sharing keeps a subclass's decorators from leaking into its base class
 * and into sibling subclasses.
 */
function declarations(prototype: any): Declarations {
  if (!Object.prototype.hasOwnProperty.call(prototype, DECLARATIONS)) {
    const inherited: Declarations | undefined = prototype[DECLARATIONS];
    prototype[DECLARATIONS] = {
      commands: [...(inherited?.commands ?? [])],
      triggers: [...(inherited?.triggers ?? [])],
      tools: [...(inherited?.tools ?? [])],
      actions: [...(inherited?.actions ?? [])],
      events: [...(inherited?.events ?? [])],
      decorator: inherited?.decorator,
    };
  }
  return prototype[DECLARATIONS];
}

function accessValue(level: CommandAccess | undefined): string {
  const value = ACCESS_LEVELS[level ?? "everyone"];
  if (!value) {
    throw new Error(`unknown access level '${level}'`);
  }
  return value;
}

/**
 * Declares a slash command handler.
 *
 * The handler receives a {@link CommandEvent} and its arguments, and answers by returning text
 * or segments, or with `await event.reply(...)`.
 *
 * @param name Command name without the slash.
 * @param options.aliases Other names that invoke the command; the handler always sees `name`.
 * @param options.access Default access level, which the operator can override.
 * @param options.priority Lower wins when several plugins declare the same name.
 */
export function Command(
  name: string,
  options?: {
    description?: string;
    usage?: string;
    priority?: number;
    aliases?: string[];
    access?: CommandAccess;
  },
): MethodDecorator {
  const access = accessValue(options?.access);
  return (target: any, propertyKey: string | symbol) => {
    const canonical = name.replace(/^\//, "");
    declarations(target).commands.push({
      methodName: propertyKey,
      name: canonical,
      description: options?.description ?? "",
      usage: options?.usage ?? `/${canonical}`,
      priority: options?.priority ?? 500,
      aliases: (options?.aliases ?? []).map((alias) => alias.replace(/^\//, "")),
      access,
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
 */
export function Trigger(
  pattern: string,
  options?: {
    name?: string;
    description?: string;
    priority?: number;
    access?: CommandAccess;
  },
): MethodDecorator {
  const access = accessValue(options?.access);
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
    });
  };
}

/**
 * Declares an LLM tool call handler.
 *
 * The handler receives the model's arguments and the {@link MessageEvent} the model was
 * answering (`undefined` when the call did not come from a chat message), so a tool knows who
 * asked without trusting the model to pass it along. Operator-only operations belong in
 * {@link Action} instead.
 */
export function Tool(
  nameOrOptions:
    | string
    | {
        name: string;
        description?: string;
        parameters?: Record<string, any>;
      },
): MethodDecorator {
  return (target: any, propertyKey: string | symbol) => {
    const info =
      typeof nameOrOptions === "string"
        ? { name: nameOrOptions, description: "", parameters: undefined }
        : nameOrOptions;
    declarations(target).tools.push({
      methodName: propertyKey,
      name: info.name,
      description: info.description ?? "",
      parameters: info.parameters,
    });
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
 * - `"llm_response"`: the `LlmResponseEvent` with the model's answer and the message it answered.
 *
 * Events are notifications: Core never waits on them and ignores the return value.
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

  /** The host's Core handle, or `undefined` in standalone mode. */
  get core(): CoreHandle | undefined {
    return this.context?.core;
  }

  /** The declarations of this plugin's class, validated. */
  private declared(): Declarations {
    const declared = declarations(Object.getPrototypeOf(this));
    // Core sends commands and triggers through the same RPC, naming either in `command`, so
    // the two share one namespace.
    const commandNames = new Set(declared.commands.map((c) => c.name));
    const clash = declared.triggers.filter((t) => commandNames.has(t.name)).map((t) => t.name);
    if (clash.length > 0) {
      throw new Error(`names used by both a command and a trigger: ${clash.join(", ")}`);
    }
    return declared;
  }

  /** Returns static metadata describing this plugin's identity, commands, and tools. */
  meta(): PluginMeta {
    const declared = this.declared();
    const strip = <T extends { methodName: unknown }>({ methodName, ...rest }: T) => rest;
    const kinds = new Set<EventKind>([...declared.events.map((e) => e.kind), ...this.events]);
    return {
      id: this.id,
      name: this.name,
      version: this.version,
      author: this.author,
      description: this.description,
      commands: declared.commands.map(strip),
      triggers: declared.triggers.map(strip),
      tools: declared.tools.map((t) => ({
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
   */
  async onExecuteCommand(req: any): Promise<any> {
    if (req.continuation) {
      const key = new MessageEvent(req.context ?? {}).conversationKey;
      const waiting = this.conversations.take(key);
      if (waiting) {
        const [session, next] = waiting;
        session.turn = new Turn();
        next.resolve(new CommandEvent(req, this.core, session));
        return runTurn(session);
      }
    }

    const declared = this.declared();
    const entry =
      declared.commands.find((c) => c.name === req.command) ??
      declared.triggers.find((t) => t.name === req.command);
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
    session.turn = new Turn();
    const event = new CommandEvent(req, this.core, session);
    // Not awaited: the handler may outlive this RPC by suspending in waitNext. runHandler never
    // rejects, so nothing is left unhandled.
    void this.runHandler(handler.bind(this), event, session);
    return runTurn(session);
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
      if (err instanceof WaitTimeoutError) {
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
    const entry = this.declared().tools.find((t) => t.name === req.tool_name);
    const handler = entry && (this as any)[entry.methodName];
    if (typeof handler !== "function") {
      return {
        call_id: req.call_id,
        success: false,
        error_message: `Unknown tool: ${req.tool_name}`,
      };
    }

    const args = req.structured_args ? fromProtoStruct(req.structured_args) : {};
    const event = req.context ? new MessageEvent(req.context, this.core) : undefined;
    let res: any;
    try {
      res = await handler.call(this, args, event);
    } catch (err: any) {
      // The model is told the tool failed and can explain or retry.
      return { call_id: req.call_id, success: false, error_message: err?.message ?? String(err) };
    }

    if (Buffer.isBuffer(res) || res instanceof Uint8Array) {
      return { call_id: req.call_id, success: true, error_message: "", raw_bytes: res };
    }
    const result = res && typeof res === "object" && !Array.isArray(res) ? res : { result: String(res) };
    return {
      call_id: req.call_id,
      success: true,
      error_message: "",
      structured_result: toProtoStruct(result),
    };
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

export {
  CommandEvent,
  MAX_WAIT_SECONDS,
  MessageEvent,
  WaitTimeoutError,
} from "./event.js";
export {
  MessageSegment,
  llmMessage,
  toSegments,
} from "./segments.js";
export type { LlmMessage, MessageSegmentItem, Replyable } from "./segments.js";
export { fromProtoStruct, fromProtoValue, toProtoStruct, toProtoValue } from "./struct.js";

export {
  startCoreWatchdog,
  DEFAULT_WATCHDOG_FAILURES,
  DEFAULT_WATCHDOG_INTERVAL_MS,
} from "./watchdog.js";
export type { CoreWatchdogOptions, LivenessProbeTarget } from "./watchdog.js";
