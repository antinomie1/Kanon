//! Conversions between `serde_json` values and the protobuf `Struct`/`Value` the plugin API
//! carries for free-form payloads (tool arguments, platform API parameters, configuration).
//!
//! Protobuf numbers are doubles on the wire, so integers come back as floats; JSON `null`
//! round-trips as `NullValue`.

use prost_types::value::Kind;

/// Converts a JSON object into a protobuf `Struct`.
pub fn to_struct(fields: serde_json::Map<String, serde_json::Value>) -> prost_types::Struct {
    prost_types::Struct {
        fields: fields
            .into_iter()
            .map(|(key, value)| (key, to_value(value)))
            .collect(),
    }
}

/// Converts one JSON value into a protobuf `Value`.
pub fn to_value(value: serde_json::Value) -> prost_types::Value {
    let kind = match value {
        serde_json::Value::Null => Kind::NullValue(0),
        serde_json::Value::Bool(flag) => Kind::BoolValue(flag),
        serde_json::Value::Number(number) => Kind::NumberValue(number.as_f64().unwrap_or_default()),
        serde_json::Value::String(text) => Kind::StringValue(text),
        serde_json::Value::Array(items) => Kind::ListValue(prost_types::ListValue {
            values: items.into_iter().map(to_value).collect(),
        }),
        serde_json::Value::Object(fields) => Kind::StructValue(to_struct(fields)),
    };
    prost_types::Value { kind: Some(kind) }
}

/// Converts a protobuf `Struct` into a JSON object.
pub fn from_struct(value: prost_types::Struct) -> serde_json::Map<String, serde_json::Value> {
    value
        .fields
        .into_iter()
        .map(|(key, value)| (key, from_value(value)))
        .collect()
}

/// Rewrites every number that is a whole value within the exactly representable range of a
/// double (|n| < 2^53) as a JSON integer, recursively.
///
/// Protobuf carries all numbers as doubles, so a model's `{"days": 3}` arrives as `3.0`, which
/// serde refuses to deserialize into an integer field. Typed tool arguments go through this
/// first; floating-point fields still accept the integers.
pub(crate) fn integral_numbers(value: serde_json::Value) -> serde_json::Value {
    /// 2^53: beyond it a double no longer holds every integer, so the value was never exact.
    const EXACT: f64 = 9_007_199_254_740_992.0;
    match value {
        serde_json::Value::Number(number) => match number.as_f64() {
            Some(float) if number.is_f64() && float.fract() == 0.0 && float.abs() < EXACT => {
                serde_json::Value::from(float as i64)
            }
            _ => serde_json::Value::Number(number),
        },
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(integral_numbers).collect())
        }
        serde_json::Value::Object(fields) => serde_json::Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, integral_numbers(value)))
                .collect(),
        ),
        other => other,
    }
}

/// Converts a protobuf `Value` into JSON. An unset kind becomes `null`, and a non-finite number
/// (which JSON cannot represent) becomes `null` too.
pub fn from_value(value: prost_types::Value) -> serde_json::Value {
    match value.kind {
        None | Some(Kind::NullValue(_)) => serde_json::Value::Null,
        Some(Kind::BoolValue(flag)) => serde_json::Value::Bool(flag),
        Some(Kind::NumberValue(number)) => serde_json::Number::from_f64(number)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Some(Kind::StringValue(text)) => serde_json::Value::String(text),
        Some(Kind::ListValue(list)) => {
            serde_json::Value::Array(list.values.into_iter().map(from_value).collect())
        }
        Some(Kind::StructValue(fields)) => serde_json::Value::Object(from_struct(fields)),
    }
}
