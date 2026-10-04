/** Public Kanon plugin SDK. Implementation modules retain a single owner for each behavior. */
export * from "./core.js";
export {
  CommandAccess,
  EventKind,
  ConversationKind,
  ScopeOptions,
  CommandMeta,
  TriggerMeta,
  ToolMeta,
  PluginMeta,
  ConversationHistory,
  Reply,
  ToolOptions,
  Command,
  Trigger,
  Tool,
  Action,
  OnEvent,
  DecorateReply,
  PrepareTurn,
  OnLlmRequest,
  HttpRoute,
} from "./declarations.js";
export { Plugin } from "./plugin.js";
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
