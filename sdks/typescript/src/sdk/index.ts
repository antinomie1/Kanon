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
  if (process.env.KANON_PROTO_PATH && fs.existsSync(process.env.KANON_PROTO_PATH)) {
    return process.env.KANON_PROTO_PATH;
  }

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

export interface CommandMeta {
  name: string;
  description?: string;
  usage?: string;
  priority?: number;
}

export interface ToolMeta {
  name: string;
  description?: string;
  parameters?: Record<string, any>;
}

export interface PluginMeta {
  id: string;
  name: string;
  version: string;
  author?: string;
  description?: string;
  commands?: CommandMeta[];
  tools?: ToolMeta[];
}

export interface MessageSegmentItem {
  text?: { content: string };
  image?: {
    url?: string;
    file_path?: string;
    raw_bytes?: Buffer | Uint8Array;
    mime_type?: string;
    filename?: string;
  };
  audio?: {
    url?: string;
    file_path?: string;
    raw_bytes?: Buffer | Uint8Array;
    duration_seconds?: number;
  };
  mention?: {
    target_user_id: string;
    display_name?: string;
    is_all?: boolean;
  };
  reply?: {
    target_message_id: string;
    snippet?: string;
  };
  custom?: {
    type_name: string;
    payload?: Record<string, any>;
  };
}

export class MessageSegment {
  /** Creates a plain text message segment. */
  static text(content: string): MessageSegmentItem {
    return { text: { content } };
  }

  /** Creates an image message segment referencing a remote URL. */
  static imageUrl(
    url: string,
    mimeType?: string,
    filename?: string,
  ): MessageSegmentItem {
    return {
      image: {
        url,
        mime_type: mimeType,
        filename,
      },
    };
  }

  /** Creates an image message segment referencing a local physical file. */
  static imageFile(
    filePath: string,
    mimeType?: string,
    filename?: string,
  ): MessageSegmentItem {
    return {
      image: {
        file_path: filePath,
        mime_type: mimeType,
        filename,
      },
    };
  }
}

/** Decorator for declaring command handler methods. */
export function Command(
  name: string,
  options?: { description?: string; usage?: string; priority?: number },
): MethodDecorator {
  return (
    target: any,
    propertyKey: string | symbol,
    descriptor: PropertyDescriptor,
  ) => {
    target._kanon_commands = target._kanon_commands || [];
    target._kanon_commands.push({
      methodName: propertyKey,
      name,
      description: options?.description || "",
      usage: options?.usage || `/${name}`,
      priority: options?.priority || 500,
    });
  };
}

/** Decorator for declaring LLM tool calling methods. */
export function Tool(
  nameOrOptions:
    | string
    | {
        name: string;
        description?: string;
        parameters?: Record<string, any>;
      },
): MethodDecorator {
  return (
    target: any,
    propertyKey: string | symbol,
    descriptor: PropertyDescriptor,
  ) => {
    target._kanon_tools = target._kanon_tools || [];
    const info =
      typeof nameOrOptions === "string"
        ? { name: nameOrOptions, description: "", parameters: undefined }
        : nameOrOptions;
    target._kanon_tools.push({
      methodName: propertyKey,
      name: info.name,
      description: info.description || "",
      parameters: info.parameters,
    });
  };
}

/** Base class for Kanon out-of-process TypeScript plugins. */
export abstract class Plugin {
  id: string = "org.kanon.plugin.base";
  name: string = "Base TS Plugin";
  version: string = "0.1.0";
  author: string = "Kanon Dev";
  description: string = "Default TypeScript plugin";
  priority: number = 500;

  context?: PluginContext;

  /** Returns static metadata describing this plugin's identity, commands, and tools. */
  meta(): PluginMeta {
    const proto = Object.getPrototypeOf(this);
    const declaredCommands: any[] = proto._kanon_commands || [];
    const declaredTools: any[] = proto._kanon_tools || [];

    const commands: CommandMeta[] = declaredCommands.map((c) => ({
      name: c.name,
      description: c.description,
      usage: c.usage,
      priority: c.priority,
    }));

    const tools: ToolMeta[] = declaredTools.map((t) => ({
      name: t.name,
      description: t.description,
      parameters: t.parameters ? (toProtoStruct(t.parameters) as any) : undefined,
    }));

    return {
      id: this.id,
      name: this.name,
      version: this.version,
      author: this.author,
      description: this.description,
      commands,
      tools,
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

  /** Executes a matched slash command. */
  async onExecuteCommand(req: any): Promise<any> {
    const proto = Object.getPrototypeOf(this);
    const declaredCommands: any[] = proto._kanon_commands || [];
    const entry = declaredCommands.find((c) => c.name === req.command);

    if (entry && typeof (this as any)[entry.methodName] === "function") {
      const res = await (this as any)[entry.methodName](req, req.args);
      if (typeof res === "string") {
        return {
          success: true,
          replies: [MessageSegment.text(res)],
          error_message: "",
        };
      } else if (Array.isArray(res)) {
        return {
          success: true,
          replies: res,
          error_message: "",
        };
      } else if (res && typeof res === "object") {
        return {
          success: res.success ?? true,
          replies: res.replies || [],
          error_message: res.error_message || "",
        };
      }
      return { success: true, replies: [], error_message: "" };
    }

    return {
      success: false,
      replies: [],
      error_message: `Unknown command: ${req.command}`,
    };
  }

  /** Executes an LLM tool call dispatched by the Core microkernel. */
  async onCallTool(req: any): Promise<any> {
    const proto = Object.getPrototypeOf(this);
    const declaredTools: any[] = proto._kanon_tools || [];
    const entry = declaredTools.find((t) => t.name === req.tool_name);

    if (entry && typeof (this as any)[entry.methodName] === "function") {
      const args = req.structured_args ? fromProtoStruct(req.structured_args) : {};
      const res = await (this as any)[entry.methodName](args);

      if (Buffer.isBuffer(res) || res instanceof Uint8Array) {
        return {
          call_id: req.call_id,
          success: true,
          error_message: "",
          raw_bytes: res,
        };
      } else if (res && typeof res === "object") {
        return {
          call_id: req.call_id,
          success: true,
          error_message: "",
          structured_result: toProtoStruct(res),
        };
      }
      return {
        call_id: req.call_id,
        success: true,
        error_message: "",
        structured_result: toProtoStruct({ result: String(res) }),
      };
    }

    return {
      call_id: req.call_id,
      success: false,
      error_message: `Unknown tool: ${req.tool_name}`,
    };
  }

  /** Processes an event notification broadcast from Core. */
  async onEvent(req: any): Promise<void> {}

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

/** Converts a JS primitive/object to a Protobuf Value descriptor. */
export function toProtoValue(val: any): any {
  if (val === null || val === undefined) {
    return { nullValue: 0 };
  } else if (typeof val === "number") {
    return { numberValue: val };
  } else if (typeof val === "string") {
    return { stringValue: val };
  } else if (typeof val === "boolean") {
    return { boolValue: val };
  } else if (Array.isArray(val)) {
    return { listValue: { values: val.map(toProtoValue) } };
  } else if (typeof val === "object") {
    return { structValue: toProtoStruct(val) };
  }
  return { stringValue: String(val) };
}

/** Converts a standard JS object dictionary into a google.protobuf.Struct payload. */
export function toProtoStruct(obj: Record<string, any>): {
  fields: Record<string, any>;
} {
  const fields: Record<string, any> = {};
  if (obj && typeof obj === "object") {
    for (const [k, v] of Object.entries(obj)) {
      fields[k] = toProtoValue(v);
    }
  }
  return { fields };
}

/** Converts a Protobuf Value descriptor back to a standard JS value. */
export function fromProtoValue(val: any): any {
  if (!val) return null;
  if ("numberValue" in val) return val.numberValue;
  if ("stringValue" in val) return val.stringValue;
  if ("boolValue" in val) return val.boolValue;
  if ("nullValue" in val) return null;
  if ("listValue" in val) return (val.listValue?.values || []).map(fromProtoValue);
  if ("structValue" in val) return fromProtoStruct(val.structValue);
  return null;
}

/** Converts a google.protobuf.Struct payload back into a standard JS object dictionary. */
export function fromProtoStruct(structObj: any): Record<string, any> {
  const res: Record<string, any> = {};
  if (structObj && structObj.fields) {
    for (const [k, v] of Object.entries(structObj.fields)) {
      res[k] = fromProtoValue(v);
    }
  }
  return res;
}


export {
  startCoreWatchdog,
  DEFAULT_WATCHDOG_FAILURES,
  DEFAULT_WATCHDOG_INTERVAL_MS,
} from "./watchdog.js";
export type { CoreWatchdogOptions, LivenessProbeTarget } from "./watchdog.js";
