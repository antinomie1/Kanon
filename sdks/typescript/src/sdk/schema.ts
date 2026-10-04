/**
 * Tool parameters without hand-written JSON Schema.
 *
 * TypeScript types are erased at runtime, so a tool describes its arguments with the `s`
 * builders instead of a signature:
 *
 * ```ts
 * @Tool("weather", {
 *   description: "Current weather and forecast for a city.",
 *   args: {
 *     city: s.string("City name, e.g. Paris"),
 *     days: s.integer("Days of forecast, 1-7").default(1),
 *     unit: s.enum(["c", "f"]).optional(),
 *   },
 * })
 * async weather({ city, days, unit }: { city: string; days: number; unit?: "c" | "f" }) { ... }
 * ```
 *
 * Every argument is required unless marked `.optional()` or given a `.default(...)`. The SDK
 * fills in defaults the model left out and tells the model about arguments it does not know or
 * forgot, before the handler runs; it does not re-check types (the model sees the schema).
 */

/** A JSON Schema object. */
export type JsonSchema = Record<string, any>;

/** One argument: its JSON Schema and whether the model must provide it. */
export class Param<T = unknown> {
  /**
   * @param schema JSON Schema of the value.
   * @param required Whether the model must pass the argument.
   * @param fallback The value the handler sees when the model leaves the argument out.
   */
  constructor(
    readonly schema: JsonSchema,
    readonly required: boolean = true,
    readonly fallback?: { value: T },
  ) {}

  /** The model may leave the argument out; the handler then sees `undefined`. */
  optional(): Param<T | undefined> {
    return new Param<T | undefined>(this.schema, false, this.fallback);
  }

  /** The model may leave the argument out; the handler then sees `value`, which the model is shown. */
  default(value: T): Param<T> {
    return new Param<T>({ ...this.schema, default: value }, false, { value });
  }

  /** Replaces the description the model reads. */
  describe(text: string): Param<T> {
    return new Param<T>({ ...this.schema, description: text }, this.required, this.fallback);
  }
}

/** A tool's arguments by name. */
export type ArgsSpec = Record<string, Param<any>>;

/** The argument object a handler receives for `S`, e.g. `ArgsOf<typeof weatherArgs>`. */
export type ArgsOf<S extends ArgsSpec> = {
  [K in keyof S]: S[K] extends Param<infer T> ? T : never;
};

function param<T>(schema: JsonSchema, description?: string): Param<T> {
  return new Param<T>(description ? { ...schema, description } : schema);
}

/** The object schema for `spec`: its properties and the names the model must pass. */
export function objectSchema(spec: ArgsSpec): JsonSchema {
  const required: string[] = [];
  const properties = Object.fromEntries(Object.entries(spec).map(([name, arg]) => {
    if (!(arg instanceof Param)) {
      throw new TypeError(`argument '${name}' must be built with s.string(), s.integer(), ...`);
    }
    if (arg.required) {
      required.push(name);
    }
    return [name, arg.schema];
  }));
  return required.length > 0
    ? { type: "object", properties, required }
    : { type: "object", properties };
}

/** Builders for tool arguments; see the module documentation. */
export const s = {
  string: (description?: string) => param<string>({ type: "string" }, description),
  /** Any number. */
  number: (description?: string) => param<number>({ type: "number" }, description),
  /** A whole number. */
  integer: (description?: string) => param<number>({ type: "integer" }, description),
  boolean: (description?: string) => param<boolean>({ type: "boolean" }, description),
  /** One of fixed values, e.g. `s.enum(["c", "f"])`. */
  enum: <const V extends readonly (string | number)[]>(values: V, description?: string) => {
    const kinds = new Set(values.map((value) => typeof value));
    const schema: JsonSchema = { enum: [...values] };
    if (kinds.size === 1) {
      schema.type = kinds.has("string") ? "string" : "number";
    }
    return param<V[number]>(schema, description);
  },
  /** A list of `items`. */
  array: <T>(items: Param<T>, description?: string) =>
    param<T[]>({ type: "array", items: items.schema }, description),
  /** A nested object; its own `.optional()` and `.default()` only change what the model sees. */
  object: <S extends ArgsSpec>(properties: S, description?: string) =>
    param<ArgsOf<S>>(objectSchema(properties), description),
  /** Any JSON value. */
  any: (description?: string) => param<any>({}, description),
};

/**
 * Checks the model's arguments against `spec` and fills in defaults.
 *
 * @throws Error naming unknown or missing arguments; the message reaches the model, which can
 *   then correct its call instead of seeing a JavaScript failure.
 */
export function bindArgs(spec: ArgsSpec, args: Record<string, any>): Record<string, any> {
  const names = Object.keys(spec);
  const unknown = Object.keys(args)
    .filter((name) => !Object.prototype.hasOwnProperty.call(spec, name))
    .sort();
  if (unknown.length > 0) {
    throw new Error(
      `unexpected arguments ${JSON.stringify(unknown)}; expected ${JSON.stringify(names)}`,
    );
  }
  const missing = names.filter(
    (name) => spec[name].required && (!Object.hasOwn(args, name) || args[name] === undefined),
  );
  if (missing.length > 0) {
    throw new Error(`missing required arguments ${JSON.stringify(missing)}`);
  }
  const bound = { ...args };
  for (const name of names) {
    const fallback = spec[name].fallback;
    if ((!Object.hasOwn(bound, name) || bound[name] === undefined) && fallback !== undefined) {
      // Defaults named __proto__ are values, not requests to change the argument object's prototype.
      Object.defineProperty(bound, name, {
        value: fallback.value, enumerable: true, writable: true, configurable: true,
      });
    }
  }
  return bound;
}
