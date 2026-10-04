/**
 * Kanon Official TypeScript SDK.
 *
 * Provides base classes, decorators, and context abstractions for authoring
 * out-of-process Kanon plugins in TypeScript or JavaScript.
 */

import * as grpc from "@grpc/grpc-js";
import * as protoLoader from "@grpc/proto-loader";
import { randomUUID } from "node:crypto";
import * as fs from "node:fs";
import * as path from "node:path";
import { clientAuth, ipcToken, loopbackEndpoint } from "./ipc.js";

import { MessageEvent } from "./event.js";
import { KV } from "./kv.js";
import {
  LlmMessage,
  MessageSegment,
  MessageSegmentItem,
  Replyable,
  toSegments,
} from "./segments.js";
import { fromProtoValue, toProtoStruct } from "./struct.js";

import type { ConversationHistory } from "./declarations.js";

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
    callback: (error: grpc.ServiceError | null, response?: RawIngestEventResponse) => void,
  ): grpc.ClientUnaryCall;
  RegisterHost(
    request: RegisterHostRequestPayload,
    options: grpc.CallOptions,
    callback: (error: grpc.ServiceError | null, response?: RawRegisterHostResponse) => void,
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
      throw new Error("kanon.plugin.v1.BotApiService is missing from the loaded proto descriptor");
    }

    const target = normalizeCoreEndpoint(endpoint);
    this.unixSocketPath = target.startsWith("unix:") ? target.slice("unix:".length) : undefined;
    this.client = new botApiService(target, credentials, {
      interceptors:
        process.platform === "win32" || !!process.env.KANON_IPC_TOKEN
          ? [clientAuth(ipcToken())]
          : [],
    }) as BotApiServiceClient;
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

    const response = await new Promise<RawIngestEventResponse>((resolve, reject) => {
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
    });

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
  async switchConversation(
    event: MessageEvent | any,
    sessionId: string,
  ): Promise<ConversationInfo[]> {
    return conversationList(
      await this.unary("SwitchConversation", { context: rawEvent(event), session_id: sessionId }),
    );
  }

  /** Deletes a conversation; deleting the current one moves the chat to a new empty one (`/del`). */
  async deleteConversation(
    event: MessageEvent | any,
    sessionId: string,
  ): Promise<ConversationInfo[]> {
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
  async registerHost(hostId: string, endpoint: string, loadedPluginIds: string[]): Promise<void> {
    const response = await new Promise<RawRegisterHostResponse>((resolve, reject) => {
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
    });

    if (response.success !== true) {
      throw new Error(`Core rejected host registration: ${response.message ?? "no message"}`);
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
