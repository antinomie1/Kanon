/**
 * The part of JSON Schema a plugin's `[config_schema]` uses to describe its settings, and how each
 * declared field is edited in the console's settings form.
 *
 * The node validates saved settings against the full declaration, so the form only needs to pick
 * a sensible control per field; anything it cannot represent faithfully is edited as JSON instead
 * of being approximated.
 */
export interface JsonSchema {
  type?: string | string[];
  title?: string;
  description?: string;
  default?: unknown;
  enum?: unknown[];
  format?: string;
  /** Marks a value the plugin treats as a secret (shown masked). */
  writeOnly?: boolean;
  properties?: Record<string, JsonSchema>;
  required?: string[];
  items?: JsonSchema;
  minimum?: number;
  maximum?: number;
}

/** The control used for one field. */
export type FieldKind =
  | 'boolean'
  | 'enum'
  | 'string'
  | 'secret'
  | 'text'
  | 'number'
  | 'integer'
  | 'list'
  | 'object'
  | 'json';

/** The declared type, ignoring `null` in a `["string", "null"]` union. */
export function schemaType(schema: JsonSchema): string | undefined {
  if (Array.isArray(schema.type)) {
    return schema.type.find((type) => type !== 'null');
  }
  if (schema.type) return schema.type;
  return schema.properties ? 'object' : undefined;
}

/** Picks the control for a field declared by `schema`. */
export function fieldKind(schema: JsonSchema): FieldKind {
  const type = schemaType(schema);
  // Only enums of plain strings and numbers fit a select; mixed values are edited as JSON.
  if (
    schema.enum?.length &&
    schema.enum.every((v) => typeof v === 'string' || typeof v === 'number')
  ) {
    return 'enum';
  }
  switch (type) {
    case 'boolean':
      return 'boolean';
    case 'integer':
      return 'integer';
    case 'number':
      return 'number';
    case 'string':
      if (schema.writeOnly || schema.format === 'password') return 'secret';
      if (schema.format === 'textarea') return 'text';
      return 'string';
    case 'array': {
      const item = schema.items ? schemaType(schema.items) : undefined;
      return item === 'string' || item === 'number' || item === 'integer'
        ? 'list'
        : 'json';
    }
    case 'object':
      return schema.properties ? 'object' : 'json';
    default:
      return 'json';
  }
}

/** Whether a whole settings declaration can be shown as a form (an object with fields). */
export function formSupported(schema: unknown): schema is JsonSchema {
  if (!schema || typeof schema !== 'object') return false;
  const root = schema as JsonSchema;
  return (
    schemaType(root) === 'object' &&
    !!root.properties &&
    Object.keys(root.properties).length > 0
  );
}

/** `value` with `key` set to `next`; `undefined` removes the key so the default applies. */
export function withKey(
  value: Record<string, unknown>,
  key: string,
  next: unknown,
): Record<string, unknown> {
  const copy = { ...value };
  if (next === undefined) delete copy[key];
  else copy[key] = next;
  return copy;
}
