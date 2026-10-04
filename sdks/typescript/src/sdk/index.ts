/**
 * Kanon Official TypeScript SDK.
 *
 * Provides base classes, decorators, and context abstractions for authoring
 * out-of-process Kanon plugins in TypeScript or JavaScript.
 */

import { clientAuth, ipcToken, loopbackEndpoint } from "./ipc.js";
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
import { KV } from "./kv.js";
import { ArgsSpec, JsonSchema, bindArgs, objectSchema } from "./schema.js";
import {
  LlmMessage,
  MessageSegment,
  MessageSegmentItem,
  Replyable,
  toSegments,
} from "./segments.js";
import { fromProtoStruct, fromProtoValue, toProtoStruct } from "./struct.js";
import { HTTP_METHODS, HttpMethod, HttpRequest, toHttpResponse } from "./web.js";

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
    options: grpc.CallOptions,
    callback: (
      error: grpc.ServiceError | null,
      response?: RawRegisterHostResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  Ping(
    request: { timestamp: number },
    options: grpc.CallOptions,
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

/** Who is calling: the host process and the plugin it serves. Set by the host. */
export interface CoreIdentity {
  /** `KANON_HOST_ID`; needed by {@link CoreHandle.refreshMeta}. */
  hostId?: string;
  /** The plugin's id; the namespace of {@link CoreHandle.kv}, and the caller of agent runs and renders. */
  pluginId?: string;
}

/** One conversation of a chat, as `/ls` lists it. */
export interface ConversationInfo {
  /** Pass it to {@link CoreHandle.switchConversation} or {@link CoreHandle.deleteConversation}. */
  sessionId: string;
  /** Whether the chat's next message continues this conversation. */
  current: boolean;
  /** The first user message, shortened; `""` while the conversation is empty. */
  title: string;
  /** Saved user and assistant messages (those compacted into the summary are not counted). */
  messageCount: number;
  /** Unix seconds of the last turn; 0 if there was none. */
  lastActiveAt: number;
}

/** A persona of the node's catalog. */
export interface Persona {
  id: string;
  name: string;
  /** The system prompt it gives the model. */
  prompt: string;
  /** The base assistant or an instance's own prompt, which plugins cannot change. */
  builtin: boolean;
}

/** Options of {@link CoreHandle.runAgent}. */
export interface RunAgentOptions {
  /**
   * The chat the run serves. It picks the bot instance (its model, plugins and tool policy) and is
   * what tools see as their context.
   */
  event?: MessageEvent | any;
  /**
   * Run inside the chat's current conversation, with its history and persona, and append the turn
   * to it — exactly as if the model had answered a message. Needs `event`. Otherwise the run uses
   * a private session that is discarded afterwards.
   */
  inConversation?: boolean;
  /** Images for this turn (`MessageSegment.imageUrl(...)`, `event.images`, ...). */
  images?: Array<MessageSegmentItem | NonNullable<MessageSegmentItem["image"]>>;
  /** Instructions for a private run; ignored in a conversation. */
  systemPrompt?: string;
  /** `<provider>/<model-id>`; empty uses the instance's or node's model. */
  model?: string;
  /** Offer tools to the model (default); `false` makes it a plain answer. */
  useTools?: boolean;
  /** Tool rounds allowed; 0 is the agent's default. */
  maxSteps?: number;
}

/** What an agent run produced. Nothing has been sent to the chat. */
export interface AgentResult {
  /** The final answer, without the model's reasoning. */
  content: string;
  /** Media the tools produced (`mime_type`, `file_path` or `url`); send them yourself. */
  attachments: Array<{ mime_type: string; file_path?: string; url?: string }>;
  /** Tools called, in order. */
  tools: string[];
  /** The conversation's session, or the discarded private one. */
  sessionId: string;
}

/** The raw `PipelineEventRequest` behind a {@link MessageEvent} or a raw request. */
function rawEvent(event: any): any {
  return event instanceof MessageEvent ? event.raw : event;
}

/** Image parts for a request: segments are unwrapped to their image. */
function imageParts(
  images: Array<MessageSegmentItem | NonNullable<MessageSegmentItem["image"]>>,
): Array<NonNullable<MessageSegmentItem["image"]>> {
  return images.map((image: any) => {
    const part = image?.image ?? image;
    if (!part || typeof part !== "object") {
      throw new TypeError("images must be image segments");
    }
    return part;
  });
}

function conversationList(response: any): ConversationInfo[] {
  return (response?.conversations ?? []).map((c: any) => ({
    sessionId: c.session_id ?? "",
    current: c.current === true,
    title: c.title ?? "",
    messageCount: Number(c.message_count ?? 0),
    // int64 arrives as a string (`longs: String`); seconds fit a double exactly.
    lastActiveAt: Number(c.last_active_at ?? 0),
  }));
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
  /** This host process; empty when the handle was created without an identity. */
  readonly hostId: string;
  /** The plugin this handle calls for; empty when created without an identity. */
  readonly pluginId: string;
  /** Created on first use of {@link kv}. */
  private kvStore?: KV;

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
   * @param identity The host and plugin this handle calls for; the host sets it, and the calls
   *   that need it say so.
   */
  constructor(
    endpoint: string,
    credentials: grpc.ChannelCredentials = grpc.credentials.createInsecure(),
    identity: CoreIdentity = {},
  ) {
    this.hostId = identity.hostId ?? "";
    this.pluginId = identity.pluginId ?? "";
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
    this.client = new botApiService(target, credentials, { interceptors: process.platform === "win32" || !!process.env.KANON_IPC_TOKEN ? [clientAuth(ipcToken())] : [] }) as BotApiServiceClient;
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

  /**
   * Reads the model conversation `event` belongs to (`BotApiService.GetConversationHistory`):
   * the same session the model would continue when answering it.
   *
   * Only user and assistant turns are returned; tool calls, tool results and the model's
   * reasoning are left out. History is read-only.
   *
   * @param event A {@link MessageEvent} or a raw `PipelineEventRequest`.
   * @param limit Keep only this many of the most recent messages; 0 keeps all.
   * @throws `NOT_FOUND` when no bot instance answers on the platform, `UNAVAILABLE` when no
   *   model is configured.
   */
  async conversationHistory(event: any, limit = 0): Promise<ConversationHistory> {
    const response = await this.unary("GetConversationHistory", {
      context: rawEvent(event),
      limit,
    });
    return {
      sessionId: response?.session_id ?? "",
      summary: response?.summary ?? "",
      messages: (response?.messages ?? []).map((message: any) => ({
        role: message.role === "LLM_ROLE_ASSISTANT" ? "assistant" : "user",
        text: message.text ?? "",
      })),
    };
  }

  // --- Conversations ----------------------------------------------------------------------------
  //
  // The same implementation as the built-in /ls, /new, /switch and /del: `event` names the chat,
  // and every call returns the chat's conversations after the change. A plugin acting for a user
  // should check permissions itself (the built-in /switch and /del are admin-only in groups).

  /** The chat's conversations, oldest first (`/ls`). */
  async listConversations(event: MessageEvent | any): Promise<ConversationInfo[]> {
    return conversationList(await this.unary("ListConversations", { context: rawEvent(event) }));
  }

  /** Starts an empty conversation and makes it current (`/new`). */
  async newConversation(event: MessageEvent | any): Promise<ConversationInfo[]> {
    return conversationList(await this.unary("NewConversation", { context: rawEvent(event) }));
  }

  /**
   * Makes `sessionId` the chat's current conversation (`/switch`).
   *
   * @throws `NOT_FOUND` when the session is not one of the chat's, `FAILED_PRECONDITION` while a
   *   turn is running in the chat.
   */
  async switchConversation(event: MessageEvent | any, sessionId: string): Promise<ConversationInfo[]> {
    return conversationList(
      await this.unary("SwitchConversation", { context: rawEvent(event), session_id: sessionId }),
    );
  }

  /** Deletes a conversation; deleting the current one moves the chat to a new empty one (`/del`). */
  async deleteConversation(event: MessageEvent | any, sessionId: string): Promise<ConversationInfo[]> {
    return conversationList(
      await this.unary("DeleteConversation", { context: rawEvent(event), session_id: sessionId }),
    );
  }

  /**
   * Appends whole turns to the chat's current conversation; resolves the session written to.
   *
   * `messages` must alternate user and assistant, starting with the user and ending with the
   * assistant, so the history keeps consisting of complete turns. Existing messages never change.
   *
   * @throws `FAILED_PRECONDITION` while a turn is running in the conversation (never queued, so a
   *   tool calling this during its own turn fails instead of deadlocking).
   */
  async appendConversation(
    event: MessageEvent | any,
    messages: Array<{ role: "user" | "assistant"; text: string }>,
  ): Promise<string> {
    const wire = messages.map(({ role, text }) => {
      if (role !== "user" && role !== "assistant") {
        throw new Error(`role must be "user" or "assistant", got ${JSON.stringify(role)}`);
      }
      return { role: role === "user" ? "LLM_ROLE_USER" : "LLM_ROLE_ASSISTANT", text };
    });
    const response = await this.unary("AppendConversation", {
      context: rawEvent(event),
      messages: wire,
    });
    return response?.session_id ?? "";
  }

  // --- Personas ---------------------------------------------------------------------------------

  /** Every persona: the base assistant, the operator's, and those made from instance prompts. */
  async listPersonas(): Promise<Persona[]> {
    const response = await this.unary("ListPersonas", {});
    return (response?.personas ?? []).map((p: any) => ({
      id: p.id ?? "",
      name: p.name ?? "",
      prompt: p.prompt ?? "",
      builtin: p.builtin === true,
    }));
  }

  /** Creates or replaces an operator persona; resolves whether one was replaced. */
  async upsertPersona(id: string, name: string, prompt: string): Promise<boolean> {
    const response = await this.unary("UpsertPersona", { id, name, prompt, builtin: false });
    return response?.replaced === true;
  }

  /** Deletes an operator persona; resolves whether it existed. */
  async deletePersona(id: string): Promise<boolean> {
    const response = await this.unary("DeletePersona", { id });
    return response?.deleted === true;
  }

  // --- The agent --------------------------------------------------------------------------------

  /**
   * Lets the node's agent (the model plus its tool loop) answer `prompt`.
   *
   * Unlike {@link requestLlm}, the agent can call tools — plugin, MCP and built-in ones. The answer
   * is returned, never sent: the plugin decides what reaches the chat.
   *
   * @throws `INVALID_ARGUMENT` (empty run, bad image, unknown model), `NOT_FOUND` (no instance
   *   serves the chat), `UNAVAILABLE` (no model, or the model or a tool failed),
   *   `FAILED_PRECONDITION` (the conversation is busy), `ABORTED` (stopped with `/stop`).
   */
  async runAgent(prompt: string, options: RunAgentOptions = {}): Promise<AgentResult> {
    const request: Record<string, any> = {
      plugin_id: this.requirePluginId("runAgent"),
      prompt,
      images: imageParts(options.images ?? []),
      in_conversation: options.inConversation === true,
      system_prompt: options.systemPrompt ?? "",
      model: options.model ?? "",
      use_tools: options.useTools ?? true,
      max_steps: options.maxSteps ?? 0,
    };
    if (options.event !== undefined) {
      request.context = rawEvent(options.event);
    }
    const response = await this.unary("RunAgent", request);
    return {
      content: response?.content ?? "",
      attachments: response?.attachments ?? [],
      tools: [...(response?.tools ?? [])],
      sessionId: response?.session_id ?? "",
    };
  }

  // --- Rendering --------------------------------------------------------------------------------

  /**
   * Lays `text` out as a PNG card and resolves an image segment, ready to send.
   *
   * Lines wrap to `width` pixels (default 720, 200–2000), blank lines separate paragraphs and a
   * line starting with `"# "` is a heading; CJK and emoji use the node's fonts. The file is
   * cleaned up after a day, so send it soon.
   */
  async renderText(text: string, width = 0): Promise<MessageSegmentItem> {
    const response = await this.unary("RenderImage", {
      plugin_id: this.requirePluginId("renderText"),
      text,
      width,
    });
    return MessageSegment.imageFile(response.file_path, "image/png");
  }

  /** Renders an SVG document to PNG at its own size. Embedded images must be `data:` URIs. */
  async renderSvg(svg: string): Promise<MessageSegmentItem> {
    const response = await this.unary("RenderImage", {
      plugin_id: this.requirePluginId("renderSvg"),
      svg,
    });
    return MessageSegment.imageFile(response.file_path, "image/png");
  }

  // --- Storage and metadata ---------------------------------------------------------------------

  /** The plugin's namespace in the node's KV store (see {@link KV}); `this.kv` in a plugin. */
  get kv(): KV {
    this.kvStore ??= new KV(
      (method, request) => this.unary(method, request),
      this.requirePluginId("kv"),
    );
    return this.kvStore;
  }

  /**
   * Asks the node to read this host's metadata again; resolves the plugin ids it now holds.
   *
   * `Plugin.addTool` and `removeTool` call this; the change applies from the next turn on.
   */
  async refreshMeta(): Promise<string[]> {
    if (!this.hostId) {
      throw new Error("refreshMeta needs a CoreHandle created with a hostId");
    }
    const response = await this.unary("RefreshPluginMeta", { host_id: this.hostId });
    return [...(response?.plugin_ids ?? [])];
  }

  private requirePluginId(what: string): string {
    if (!this.pluginId) {
      throw new Error(`${what} needs a CoreHandle created with a pluginId`);
    }
    return this.pluginId;
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
          { deadline: Date.now() + 3000 },
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
        // A connected Core can stop answering without dropping HTTP/2. Bound the RPC itself
        // so the watchdog observes a failure and does not retain unfinished probes forever.
        { deadline: Date.now() + 5000 },
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
  if (process.platform === "win32") return loopbackEndpoint(endpoint);
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
export type EventKind =
  | "message_sent"
  | "notice"
  | "llm_response"
  | "agent_begin"
  | "agent_done"
  | "tool_call"
  | "tool_result";

const EVENT_KINDS: Record<EventKind, string> = {
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
interface ToolEntry {
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
function toolEntry(name: string, options: ToolOptions = {}): ToolEntry {
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
interface Declarations {
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
function declarations(prototype: any): Declarations {
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

function accessValue(level: CommandAccess | undefined): string {
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
  const entry =
    typeof name === "string" ? toolEntry(name, options) : toolEntry(name.name, name);
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
    throw new Error(`unknown HTTP methods ${JSON.stringify(unknown)}; expected some of ${HTTP_METHODS}`);
  }
  return (target: any, propertyKey: string | symbol) => {
    declarations(target).routes.push({ path, methods, methodName: propertyKey });
  };
}

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
   */
  async onExecuteCommand(req: any): Promise<any> {
    if (req.continuation) {
      const key = new MessageEvent(req.context ?? {}).conversationKey;
      const waiting = this.conversations.take(key);
      if (waiting) {
        const [session, next] = waiting;
        const turn = (session.turn = new Turn());
        next.resolve(new CommandEvent(req, this.core, session));
        return runTurn(turn);
      }
    }

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
    // Not awaited: the handler may outlive this RPC by suspending in waitNext. runHandler never
    // rejects, so nothing is left unhandled.
    void this.runHandler(handler.bind(this), event, session);
    return runTurn(turn);
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
export { KV, MAX_VALUE_BYTES } from "./kv.js";
export { Param, s } from "./schema.js";
export type { ArgsOf, ArgsSpec, JsonSchema } from "./schema.js";
export { HttpRequest, HttpResponse } from "./web.js";
export type { HttpMethod } from "./web.js";

export {
  startCoreWatchdog,
  DEFAULT_WATCHDOG_FAILURES,
  DEFAULT_WATCHDOG_INTERVAL_MS,
} from "./watchdog.js";
export type { CoreWatchdogOptions, LivenessProbeTarget } from "./watchdog.js";
