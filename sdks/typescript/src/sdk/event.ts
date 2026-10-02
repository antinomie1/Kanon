/**
 * Event objects handed to plugin handlers, and multi-turn conversations.
 *
 * Handlers receive a {@link MessageEvent} (or its {@link CommandEvent} subclass) instead of raw
 * request objects. The event knows the conversation it came from, so a handler can answer with
 * `await event.reply(...)` and ask a follow-up question with `await event.waitNext()`.
 *
 * How `waitNext` works
 * --------------------
 * Core processes messages one at a time and never blocks waiting for a plugin to "hear back"
 * from a user, so a handler cannot sleep inside one `OnExecuteCommand` call until the next
 * message arrives. Instead:
 *
 * 1. The handler runs on its own. The RPC that started it waits for the handler's next
 *    *yield point*: either it finishes, or it calls `waitNext`.
 * 2. `waitNext(timeout)` ends the current RPC, returning the replies gathered so far together
 *    with `capture_seconds = timeout`. Core then routes the same sender's next message in that
 *    channel back to this plugin as a *continuation*.
 * 3. The continuation RPC resumes the suspended handler with the new event and again waits for
 *    its next yield point.
 *
 * Replies made while an RPC is waiting are returned in that RPC's response; replies made when
 * none is waiting (after a `waitNext` timed out, or from a background task) are delivered
 * through `ReplyMessage` instead.
 */

import type { CoreHandle } from "./index.js";
import { MessageSegmentItem, Replyable, toSegments } from "./segments.js";
import { fromProtoStruct } from "./struct.js";

/** Longest capture Core honours, in seconds. */
export const MAX_WAIT_SECONDS = 600;

/**
 * Extra time a suspended handler stays alive after its capture window, so a message Core
 * routed at the last moment still finds the handler waiting.
 */
export const WAIT_GRACE_MS = 5000;

/** Raised by {@link CommandEvent.waitNext} when the sender did not answer in time. */
export class WaitTimeoutError extends Error {
  constructor(message = "the sender did not answer in time") {
    super(message);
    this.name = "WaitTimeoutError";
  }
}

/** A promise with its settle functions exposed. */
class Deferred<T> {
  readonly promise: Promise<T>;
  resolve!: (value: T) => void;
  reject!: (error: unknown) => void;
  settled = false;

  constructor() {
    this.promise = new Promise<T>((resolve, reject) => {
      this.resolve = (value) => {
        this.settled = true;
        resolve(value);
      };
      this.reject = (error) => {
        this.settled = true;
        reject(error);
      };
    });
  }
}

/** An inbound platform message (or notice) as a plugin sees it. */
export class MessageEvent {
  /** Adapter metadata decoded into plain JSON data, computed on first use. */
  private decodedMetadata?: Record<string, any>;

  /**
   * @param raw The `PipelineEventRequest` as decoded by the host.
   * @param core The host's Core handle, or `undefined` in standalone mode.
   */
  constructor(
    readonly raw: any,
    readonly core?: CoreHandle,
  ) {}

  /** Platform-qualified id of this message; quote it with `MessageSegment.quote`. */
  get eventId(): string {
    return this.raw?.event_id ?? "";
  }

  /** Platform the message arrived on. */
  get platform(): string {
    return this.raw?.platform ?? "";
  }

  /** Conversation the message belongs to, e.g. `group:123` or `private:456`. */
  get channelId(): string {
    return this.raw?.channel_id ?? "";
  }

  /** Platform id of the author. */
  get senderId(): string {
    return this.raw?.sender_id ?? "";
  }

  /** The message's plain text. */
  get text(): string {
    return this.raw?.raw_text ?? "";
  }

  /** The message's typed segments (text, images, mentions, quotes, ...). */
  get segments(): MessageSegmentItem[] {
    return this.raw?.segments ?? [];
  }

  /** Adapter metadata as plain JSON data (keys such as `kanon.sender_name`). */
  get metadata(): Record<string, any> {
    if (this.decodedMetadata === undefined) {
      this.decodedMetadata = this.raw?.metadata ? fromProtoStruct(this.raw.metadata) : {};
    }
    return this.decodedMetadata!;
  }

  /** Display name of the author, when the platform reports one. */
  get senderName(): string {
    return String(this.metadata["kanon.sender_name"] ?? "");
  }

  /** `owner`, `admin` or `member` in groups whose platform reports roles. */
  get senderRole(): string {
    return String(this.metadata["kanon.sender_role"] ?? "");
  }

  /** Whether the message was posted in a group conversation. */
  get isGroup(): boolean {
    return this.metadata["kanon.conversation_kind"] === "group";
  }

  /** Whether the message @-mentions the bot. */
  get botMentioned(): boolean {
    return this.metadata["kanon.bot_mentioned"] === true;
  }

  /** Notice kind (`poke`, `member_join`, ...) or `""` for an ordinary message. */
  get notice(): string {
    return String(this.metadata["kanon.notice"] ?? "");
  }

  /** Image segments of the message, in order. */
  get images(): Array<NonNullable<MessageSegmentItem["image"]>> {
    return this.segments.filter((s) => s.image).map((s) => s.image!);
  }

  /** The key Core uses for captures: platform, channel and sender. */
  get conversationKey(): string {
    // NUL cannot appear in platform ids, so the joined key is unambiguous.
    return [this.platform, this.channelId, this.senderId].join("\u0000");
  }

  /**
   * Sends a message to this conversation right away and waits for delivery.
   *
   * Use this for progress notes during long work. Throws in standalone mode, where there is
   * no Core to deliver through.
   */
  async send(content: Replyable): Promise<any> {
    if (!this.core) {
      throw new Error("no Core connection: cannot send messages in standalone mode");
    }
    return this.core.replyTo(this.raw, content);
  }

  /** Answers this message. For a plain event this is the same as {@link send}. */
  async reply(content: Replyable): Promise<void> {
    await this.send(content);
  }
}

/** Fields of the command response a turn resolves with. */
interface TurnOutcome {
  success: boolean;
  error: string;
  captureSeconds: number;
}

/** One RPC waiting for the handler's next yield point, and the replies it will carry. */
export class Turn {
  readonly replies: MessageSegmentItem[] = [];
  readonly done = new Deferred<TurnOutcome>();
  /** Set by {@link CommandEvent.passToModel}. */
  passToModel = false;
  /** Text the model reads instead of the message's own, when passing it on. */
  modelText?: string;

  /** Ends the turn; the waiting RPC answers with the replies gathered so far. */
  finish(captureSeconds = 0, success = true, error = ""): void {
    if (!this.done.settled) {
      this.done.resolve({ success, error, captureSeconds });
    }
  }
}

/** A running command handler and the RPC currently waiting on it, if any. */
export class Session {
  turn?: Turn;

  constructor(readonly conversations: Conversations) {}
}

/** A message that invoked a command or trigger, or continued a conversation. */
export class CommandEvent extends MessageEvent {
  /** Canonical command (or trigger) name. */
  readonly command: string;
  /** Arguments split on whitespace, quotes respected. For a trigger, its regex groups. */
  readonly args: string[];
  /** Everything after the command name, unsplit. */
  readonly rawArgs: string;
  /** True when this message answers an earlier `waitNext`/capture. */
  readonly continuation: boolean;

  /**
   * @param request The `CommandExecuteRequest` as decoded by the host.
   * @param core The host's Core handle, or `undefined` in standalone mode.
   * @param session The handler session; absent for events built outside a dispatch.
   */
  constructor(
    readonly request: any,
    core?: CoreHandle,
    private readonly session?: Session,
  ) {
    super(request?.context ?? {}, core);
    this.command = request?.command ?? "";
    this.args = request?.args ?? [];
    this.rawArgs = request?.raw_args ?? "";
    this.continuation = request?.continuation === true;
  }

  /** The request's `context`, for handlers written against the raw request. */
  get context(): any {
    return this.request?.context;
  }

  /**
   * Answers this message.
   *
   * While Core is waiting on this handler, replies are collected and sent together as the
   * command's answer; otherwise they are delivered immediately.
   */
  async reply(content: Replyable): Promise<void> {
    const turn = this.session?.turn;
    if (turn) {
      turn.replies.push(...toSegments(content));
    } else {
      await this.send(content);
    }
  }

  /**
   * Hands this message on to the model once the handler returns.
   *
   * Core then continues as if no command or trigger had matched: replies made in this turn are
   * delivered first, and the reply policy and the model decide whether the bot answers. A later
   * `waitNext` in the same turn cancels the hand-off.
   *
   * @param text Replaces the message text the model reads; images are kept. Omit it to pass the
   *   message on unchanged.
   * @throws If Core is no longer waiting on this handler (after a `waitNext` timed out).
   */
  passToModel(text?: string): void {
    const turn = this.session?.turn;
    if (!turn) {
      throw new Error("Core is no longer waiting on this message; it cannot be passed on");
    }
    turn.passToModel = true;
    turn.modelText = text;
  }

  /**
   * Ends this turn and waits for the same sender's next message in this conversation.
   *
   * Replies made so far are sent first. The next message skips commands and the model and
   * comes back here as a new {@link CommandEvent} with `continuation` set.
   *
   * @param timeoutSeconds Seconds to wait, at most {@link MAX_WAIT_SECONDS}.
   * @throws WaitTimeoutError If the sender did not answer in time (or a newer `waitNext` in
   *   the same conversation replaced this one). The handler may still `reply` afterwards;
   *   those replies are delivered on their own.
   */
  async waitNext(timeoutSeconds = 60): Promise<CommandEvent> {
    const session = this.session;
    if (!session) {
      throw new Error("waitNext is only available inside a command handler");
    }
    const seconds = Math.max(1, Math.min(Math.floor(timeoutSeconds), MAX_WAIT_SECONDS));
    const key = this.conversationKey;
    const next = new Deferred<CommandEvent>();
    session.conversations.suspend(key, session, next);

    // Hand the turn back to Core: its RPC returns now, asking for the capture.
    const turn = session.turn;
    session.turn = undefined;
    turn?.finish(seconds);

    const timer = setTimeout(
      () => next.reject(new WaitTimeoutError()),
      seconds * 1000 + WAIT_GRACE_MS,
    );
    try {
      return await next.promise;
    } finally {
      clearTimeout(timer);
      session.conversations.forget(key, next);
    }
  }
}

/** Suspended command handlers, keyed by the conversation Core will route back. */
export class Conversations {
  private readonly waiting = new Map<string, [Session, Deferred<CommandEvent>]>();

  /** Registers a handler waiting for `key`'s next message. */
  suspend(key: string, session: Session, next: Deferred<CommandEvent>): void {
    // Core keeps one capture per conversation, so a newer wait replaces an older one; the
    // older handler is told instead of being left hanging until its timeout.
    const previous = this.waiting.get(key);
    if (previous && !previous[1].settled) {
      previous[1].reject(new WaitTimeoutError("superseded by a newer waitNext"));
    }
    this.waiting.set(key, [session, next]);
  }

  /** Drops `key`'s entry if it still belongs to `next`. */
  forget(key: string, next: Deferred<CommandEvent>): void {
    const entry = this.waiting.get(key);
    if (entry && entry[1] === next) {
      this.waiting.delete(key);
    }
  }

  /** Removes and returns the handler waiting for `key`, if one still is. */
  take(key: string): [Session, Deferred<CommandEvent>] | undefined {
    const entry = this.waiting.get(key);
    this.waiting.delete(key);
    return entry && !entry[1].settled ? entry : undefined;
  }
}

/**
 * Waits for `turn` to finish and turns it into a command response.
 *
 * Takes the turn rather than reading `session.turn`: a handler that calls `waitNext` before its
 * first `await` finishes and detaches the turn before this function runs.
 */
export async function runTurn(turn: Turn): Promise<any> {
  const outcome = await turn.done.promise;
  // A turn that captures the conversation never hands its message on: the handler is waiting
  // for the next message, so this one is not the model's.
  const passing = turn.passToModel && outcome.captureSeconds === 0;
  return {
    success: outcome.success,
    replies: turn.replies,
    error_message: outcome.error,
    capture_seconds: outcome.captureSeconds,
    pass_to_model: passing,
    ...(passing && turn.modelText !== undefined ? { model_text: turn.modelText } : {}),
  };
}
