/**
 * The plugin's namespace in the node's central key-value store.
 *
 * Small state — counters, switches, tokens, per-user settings — belongs here: the node keeps it
 * in `data/kv.db`, so it survives restarts of the plugin and of the node, and nothing has to be
 * set up. Larger data, or data that needs queries, belongs in files or a database under
 * `context.dataDir`.
 *
 * Values are stored as JSON (UTF-8), the convention every Kanon SDK follows, so a value written
 * by a plugin in another language reads back the same. A stored value that is not JSON is
 * reported, never guessed at.
 */

/** Largest value the node accepts, in bytes of encoded JSON. */
export const MAX_VALUE_BYTES = 1024 * 1024;

/** Issues one unary `BotApiService` call. */
export type UnaryCall = (method: string, request: Record<string, any>) => Promise<any>;

/** JSON's traversal checks nested numbers without silently converting them to null. */
function finiteJsonValue(_key: string, value: unknown): unknown {
  if (typeof value === "number" && !Number.isFinite(value)) {
    throw new TypeError("JSON numbers must be finite");
  }
  return value;
}

/**
 * Reads and writes one plugin's keys in the node's KV store.
 *
 * Get one from `this.kv` in a plugin:
 *
 * ```ts
 * const visits = await this.kv.get(`visits:${event.senderId}`, 0);
 * await this.kv.set(`visits:${event.senderId}`, visits + 1);
 * await this.kv.set("login-token", token, { ttl: 3600 });
 * ```
 *
 * Every call is one RPC to the node; errors (an invalid key, a value over 1 MiB, the store being
 * unavailable) reject with the gRPC status the node chose.
 */
export class KV {
  constructor(
    private readonly call: UnaryCall,
    readonly pluginId: string,
  ) {
    if (!pluginId) {
      throw new Error("the KV store needs the plugin id as its namespace");
    }
  }

  /**
   * Returns the value of `key`, or `fallback` when it is missing or expired.
   *
   * @throws Error if the stored bytes are not JSON (written by something that did not follow the
   *   convention); the key is named so the culprit can be found.
   */
  async get<T = any>(key: string): Promise<T | undefined>;
  async get<T>(key: string, fallback: T): Promise<T>;
  async get<T>(key: string, fallback?: T): Promise<T | undefined> {
    const response = await this.call("GetStorage", { plugin_id: this.pluginId, key });
    if (!response?.found) {
      return fallback;
    }
    try {
      return JSON.parse(Buffer.from(response.value ?? []).toString("utf8"), finiteJsonValue);
    } catch (err: any) {
      throw new Error(`KV value of '${key}' is not JSON: ${err?.message ?? err}`);
    }
  }

  /**
   * Stores `value` (anything JSON can express) under `key`.
   *
   * @param key 1–256 bytes.
   * @param options.ttl Positive safe integer seconds until the key expires; omit it to keep
   *   the key until it is deleted. Setting a key again replaces both its value and its expiry.
   * @throws TypeError if `value` cannot be encoded as JSON (`undefined`, a function, a cycle,
   *   a non-finite number); RangeError for a `ttl` that is not a positive safe integer, or a value over
   *   {@link MAX_VALUE_BYTES} (checked here so the mistake is reported before a round trip).
   */
  async set(key: string, value: unknown, options: { ttl?: number } = {}): Promise<void> {
    const { ttl } = options;
    if (ttl !== undefined && !(Number.isSafeInteger(ttl) && ttl > 0)) {
      throw new RangeError(`ttl must be a positive safe integer number of seconds, got ${ttl}`);
    }
    const json = JSON.stringify(value, finiteJsonValue);
    if (json === undefined) {
      throw new TypeError(`KV value of '${key}' cannot be encoded as JSON`);
    }
    const encoded = Buffer.from(json, "utf8");
    if (encoded.length > MAX_VALUE_BYTES) {
      throw new RangeError(
        `KV value of '${key}' is ${encoded.length} bytes; the limit is ${MAX_VALUE_BYTES}`,
      );
    }
    await this.call("SetStorage", {
      plugin_id: this.pluginId,
      key,
      value: encoded,
      ttl_seconds: ttl ?? 0,
    });
  }

  /** Removes `key`; resolves whether it existed (an expired key did not). */
  async delete(key: string): Promise<boolean> {
    const response = await this.call("DeleteStorage", { plugin_id: this.pluginId, key });
    return response?.deleted === true;
  }

  /** Lists the live keys starting with `prefix` (taken literally), sorted. */
  async keys(prefix = ""): Promise<string[]> {
    const response = await this.call("ListStorage", { plugin_id: this.pluginId, prefix });
    return [...(response?.keys ?? [])];
  }
}
