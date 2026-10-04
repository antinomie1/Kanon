/**
 * Conversions between plain JS values and `google.protobuf.Struct` / `Value`.
 *
 * protobufjs ships its own descriptors for the well-known types, which keep the camelCase
 * field names (`numberValue`, `structValue`) even when the Kanon IDL is loaded with
 * `keepCase: true`; that is why these helpers do not use the IDL's snake_case style.
 */

/** Converts a JS primitive/object to a Protobuf Value descriptor. */
export function toProtoValue(val: any): any {
  if (val === null || val === undefined) {
    return { nullValue: 0 };
  } else if (typeof val === "number") {
    if (!Number.isFinite(val)) {
      throw new TypeError("Struct numbers must be finite");
    }
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
  // Define own properties so a JSON key named __proto__ remains payload data.
  const entries = obj && typeof obj === "object" ? Object.entries(obj) : [];
  return { fields: Object.fromEntries(entries.map(([key, value]) => [key, toProtoValue(value)])) };
}

/** Converts a Protobuf Value descriptor back to a standard JS value. */
export function fromProtoValue(val: any): any {
  if (!val) return null;
  if ("numberValue" in val) {
    if (!Number.isFinite(val.numberValue)) {
      throw new TypeError("Struct numbers must be finite");
    }
    return val.numberValue;
  }
  if ("stringValue" in val) return val.stringValue;
  if ("boolValue" in val) return val.boolValue;
  if ("nullValue" in val) return null;
  if ("listValue" in val) return (val.listValue?.values || []).map(fromProtoValue);
  if ("structValue" in val) return fromProtoStruct(val.structValue);
  return null;
}

/** Converts a google.protobuf.Struct payload back into a standard JS object dictionary. */
export function fromProtoStruct(structObj: any): Record<string, any> {
  return Object.fromEntries(
    Object.entries(structObj?.fields ?? {}).map(([key, value]) => [key, fromProtoValue(value)]),
  );
}
