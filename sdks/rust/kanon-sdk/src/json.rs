//! Conversions between JSON and the protobuf payloads used by the plugin API.
//!
//! Finite whole protobuf numbers within ±2^53 become JSON integers. Non-finite numbers fail;
//! JSON null and an unset protobuf kind both decode as null.

pub use kanon_proto::json::NonFiniteNumber;

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
    kanon_proto::json::json_to_prost_value(&value)
}

/// Converts a protobuf `Struct` into a JSON object, rejecting non-finite numbers.
pub fn from_struct(
    value: prost_types::Struct,
) -> Result<serde_json::Map<String, serde_json::Value>, NonFiniteNumber> {
    value
        .fields
        .into_iter()
        .map(|(key, value)| from_value(value).map(|value| (key, value)))
        .collect()
}

/// Converts a protobuf `Value` into JSON, rejecting non-finite numbers.
pub fn from_value(value: prost_types::Value) -> Result<serde_json::Value, NonFiniteNumber> {
    kanon_proto::json::prost_value_to_json(value)
}

/// Rewrites every number that is a whole value within the exactly representable range of a
/// double (|n| < 2^53) as a JSON integer, recursively.
///
/// Direct JSON callers can provide `3.0`, which serde refuses to deserialize into an integer
/// field. Typed tool arguments go through this before deserialization; floating-point fields
/// still accept the integers. Protobuf decoding already normalizes its whole numbers.
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
