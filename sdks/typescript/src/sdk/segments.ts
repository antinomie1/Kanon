/**
 * Message segments: the wire shape of `kanon.plugin.v1.MessageSegment` and builders for it.
 *
 * The host loads the IDL with `keepCase: true`, so segments are plain objects whose field names
 * read exactly like plugin.proto (`file_path`, `target_message_id`, ...).
 */

/** Media source shared by image, audio, video and file segments; set exactly one field. */
interface MediaSource {
  url?: string;
  file_path?: string;
  raw_bytes?: Buffer | Uint8Array;
}

/** Wire shape of one `MessageSegment`; exactly one field is set. */
export interface MessageSegmentItem {
  text?: { content: string };
  image?: MediaSource & { mime_type?: string; filename?: string };
  audio?: MediaSource & { duration_seconds?: number };
  video?: MediaSource & { mime_type?: string; filename?: string };
  file?: MediaSource & { name: string };
  face?: { id: string };
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
  /** Name of the set field, added by the decoder (`oneofs: true`) on inbound segments. */
  segment?: string;
}

/** Anything a handler may answer with: text, one segment, or a list mixing both. */
export type Replyable = string | MessageSegmentItem | Array<string | MessageSegmentItem>;

/** Builders for outbound segments. */
export class MessageSegment {
  /** Plain text. */
  static text(content: string): MessageSegmentItem {
    return { text: { content } };
  }

  /** An image at a remote URL. */
  static imageUrl(url: string, mimeType?: string, filename?: string): MessageSegmentItem {
    return { image: { url, mime_type: mimeType, filename } };
  }

  /** An image read from a local file by the adapter. */
  static imageFile(filePath: string, mimeType?: string, filename?: string): MessageSegmentItem {
    return { image: { file_path: filePath, mime_type: mimeType, filename } };
  }

  /** An image from bytes in memory. */
  static imageBytes(data: Buffer | Uint8Array, mimeType?: string, filename?: string): MessageSegmentItem {
    return { image: { raw_bytes: data, mime_type: mimeType, filename } };
  }

  /** A voice message at a remote URL. */
  static audioUrl(url: string, durationSeconds?: number): MessageSegmentItem {
    return { audio: { url, duration_seconds: durationSeconds } };
  }

  /** A voice message read from a local file. */
  static audioFile(filePath: string, durationSeconds?: number): MessageSegmentItem {
    return { audio: { file_path: filePath, duration_seconds: durationSeconds } };
  }

  /** A voice message from bytes in memory. */
  static audioBytes(data: Buffer | Uint8Array, durationSeconds?: number): MessageSegmentItem {
    return { audio: { raw_bytes: data, duration_seconds: durationSeconds } };
  }

  /** A video at a remote URL. */
  static videoUrl(url: string, mimeType?: string, filename?: string): MessageSegmentItem {
    return { video: { url, mime_type: mimeType, filename } };
  }

  /** A video read from a local file. */
  static videoFile(filePath: string, mimeType?: string, filename?: string): MessageSegmentItem {
    return { video: { file_path: filePath, mime_type: mimeType, filename } };
  }

  /**
   * A file attachment. `name` is what the recipient sees and is required; pass exactly one
   * source.
   */
  static file(name: string, source: MediaSource): MessageSegmentItem {
    const set = (["url", "file_path", "raw_bytes"] as const).filter((k) => source[k] !== undefined);
    if (!name) {
      throw new Error("a file segment needs a name");
    }
    if (set.length !== 1) {
      throw new Error("a file segment needs exactly one of url, file_path and raw_bytes");
    }
    return { file: { ...source, name } };
  }

  /** A platform emoji/face by its platform id (e.g. a QQ face id). */
  static face(id: string): MessageSegmentItem {
    return { face: { id } };
  }

  /** An @-mention of one user. */
  static mention(userId: string, displayName?: string): MessageSegmentItem {
    return { mention: { target_user_id: userId, display_name: displayName } };
  }

  /** An @-mention of everyone in the group. */
  static mentionAll(): MessageSegmentItem {
    return { mention: { target_user_id: "", is_all: true } };
  }

  /** A quote of the message with `eventId` (sent as the platform's native reply). */
  static quote(eventId: string): MessageSegmentItem {
    return { reply: { target_message_id: eventId } };
  }
}

/**
 * Normalizes a {@link Replyable} into a segment list: strings become text segments.
 *
 * @throws TypeError for anything else, so a handler returning the wrong type fails loudly
 *   instead of sending an empty message.
 */
export function toSegments(value: Replyable): MessageSegmentItem[] {
  const items = Array.isArray(value) ? value : [value];
  return items.map((item) => {
    if (typeof item === "string") {
      return MessageSegment.text(item);
    }
    if (item !== null && typeof item === "object") {
      return item;
    }
    throw new TypeError(`cannot send ${typeof item} as a message segment`);
  });
}

/** Wire shape of one `LLMMessage` turn. */
export interface LlmMessage {
  role: "LLM_ROLE_USER" | "LLM_ROLE_ASSISTANT";
  text: string;
  images?: Array<NonNullable<MessageSegmentItem["image"]>>;
}

/** Builds one turn for `CoreHandle.requestLlm({ messages })`; images are user-only. */
export function llmMessage(
  text: string,
  role: "user" | "assistant" = "user",
  images: Array<NonNullable<MessageSegmentItem["image"]>> = [],
): LlmMessage {
  return {
    role: role === "user" ? "LLM_ROLE_USER" : "LLM_ROLE_ASSISTANT",
    text,
    images,
  };
}
