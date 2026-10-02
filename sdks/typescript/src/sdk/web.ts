/**
 * HTTP routes a plugin serves through the node's management gateway.
 *
 * The gateway forwards every request under `/api/v1/plugins/<plugin id>/http/` to the plugin
 * (`OnHttpRequest`); the SDK routes it to the method declared for its path:
 *
 * ```ts
 * @HttpRoute("/stats")
 * async stats(request: HttpRequest) {
 *   return { visits: await this.kv.get("visits", 0) };
 * }
 *
 * @HttpRoute("/webhook", { methods: ["POST"] })
 * async webhook(request: HttpRequest) {
 *   if (!validSignature(request.headers["x-signature"], request.body)) {
 *     return new HttpResponse(401);
 *   }
 *   ...
 *   return new HttpResponse(204);
 * }
 * ```
 *
 * A plugin page (`pages/index.html`) reaches these routes with relative URLs, e.g.
 * `fetch('../http/stats')`.
 *
 * The gateway authenticates nothing: whoever can reach the node's console port can call these
 * routes, so a route that changes anything must check its caller (a webhook signature, a token
 * from the plugin's config). Bodies are limited to 3 MiB and the plugin must answer within 30 s.
 */

/** Methods a route may declare. */
export const HTTP_METHODS = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"] as const;

export type HttpMethod = (typeof HTTP_METHODS)[number];

/** A request forwarded by the gateway. */
export class HttpRequest {
  /**
   * @param method Upper-case method, e.g. `"GET"`.
   * @param path Path below the plugin's `http/` root, starting with `/`.
   * @param query Query parameters; each name maps to all of its values, in order.
   * @param headers Request headers with lower-case names; repeated headers are joined with `", "`.
   * @param body The raw body.
   */
  constructor(
    readonly method: string,
    readonly path: string,
    readonly query: Record<string, string[]> = {},
    readonly headers: Record<string, string> = {},
    readonly body: Buffer = Buffer.alloc(0),
  ) {}

  /** The first value of query parameter `name`, or `fallback`. */
  arg(name: string): string | undefined;
  arg(name: string, fallback: string): string;
  arg(name: string, fallback?: string): string | undefined {
    return this.query[name]?.[0] ?? fallback;
  }

  /** The body decoded as UTF-8. */
  text(): string {
    return this.body.toString("utf8");
  }

  /** The body parsed as JSON (throws `SyntaxError` if it is not); an empty body is `null`. */
  json(): any {
    return this.body.length === 0 ? null : JSON.parse(this.text());
  }

  /** Builds a request from the wire `HttpRequest`. */
  static fromProto(request: any): HttpRequest {
    const headers: Record<string, string> = {};
    for (const header of request?.headers ?? []) {
      const name = String(header.name).toLowerCase();
      headers[name] = name in headers ? `${headers[name]}, ${header.value}` : String(header.value);
    }
    const query: Record<string, string[]> = {};
    for (const [name, value] of new URLSearchParams(request?.query ?? "")) {
      (query[name] ??= []).push(value);
    }
    return new HttpRequest(
      String(request?.method ?? "GET").toUpperCase(),
      request?.path || "/",
      query,
      headers,
      Buffer.from(request?.body ?? []),
    );
  }
}

/** A response with full control over status, headers and body. Handlers may also return plain values (see {@link toHttpResponse}). */
export class HttpResponse {
  constructor(
    readonly status: number = 200,
    readonly body: Buffer | string = Buffer.alloc(0),
    readonly headers: Record<string, string> = {},
  ) {}

  /** A JSON response. */
  static json(value: unknown, status = 200): HttpResponse {
    return new HttpResponse(status, JSON.stringify(value), {
      "content-type": "application/json; charset=utf-8",
    });
  }

  /** A plain text response. */
  static text(value: string, status = 200): HttpResponse {
    return new HttpResponse(status, value, { "content-type": "text/plain; charset=utf-8" });
  }

  /** An HTML response. The gateway serves it sandboxed, away from the console's origin. */
  static html(value: string, status = 200): HttpResponse {
    return new HttpResponse(status, value, { "content-type": "text/html; charset=utf-8" });
  }

  /** The wire `HttpResponse`. */
  toProto(): { status: number; headers: Array<{ name: string; value: string }>; body: Buffer } {
    return {
      status: this.status,
      headers: Object.entries(this.headers).map(([name, value]) => ({ name, value })),
      body: typeof this.body === "string" ? Buffer.from(this.body, "utf8") : this.body,
    };
  }
}

/**
 * Turns a route handler's return value into a response.
 *
 * `HttpResponse` is used as is; `undefined`/`null` is `204 No Content`; a string is plain text;
 * a `Buffer`/`Uint8Array` is `application/octet-stream`; anything else is encoded as JSON.
 */
export function toHttpResponse(result: unknown): HttpResponse {
  if (result instanceof HttpResponse) {
    return result;
  }
  if (result === undefined || result === null) {
    return new HttpResponse(204);
  }
  if (typeof result === "string") {
    return HttpResponse.text(result);
  }
  if (result instanceof Uint8Array) {
    return new HttpResponse(200, Buffer.from(result), {
      "content-type": "application/octet-stream",
    });
  }
  return HttpResponse.json(result);
}
