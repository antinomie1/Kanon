//! Shared JSON/protobuf conversions for free-form plugin protocol payloads.
//!
//! Protobuf stores numbers as doubles. Finite whole numbers within ±2^53 become JSON integers;
//! values outside that range remain floats. Non-finite numbers fail instead of becoming null.

use prost_types::value::Kind;
use serde_json::Value;

/// A protobuf payload contains a number JSON cannot represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonFiniteNumber;

impl std::fmt::Display for NonFiniteNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("protobuf number must be finite")
    }
}

impl std::error::Error for NonFiniteNumber {}

/// Converts a protobuf object into JSON, rejecting non-finite numbers at any depth.
pub fn prost_struct_to_json(value: prost_types::Struct) -> Result<Value, NonFiniteNumber> {
    let fields = value
        .fields
        .into_iter()
        .map(|(key, value)| prost_value_to_json(value).map(|value| (key, value)))
        .collect::<Result<_, _>>()?;
    Ok(Value::Object(fields))
}

/// Converts a protobuf value into JSON; an unset kind is treated as null.
pub fn prost_value_to_json(value: prost_types::Value) -> Result<Value, NonFiniteNumber> {
    const MAX_SAFE_INTEGER: f64 = (1_u64 << 53) as f64;
    Ok(match value.kind {
        Some(Kind::NullValue(_)) | None => Value::Null,
        Some(Kind::NumberValue(number))
            if number.fract() == 0.0 && number.abs() <= MAX_SAFE_INTEGER =>
        {
            Value::from(number as i64)
        }
        Some(Kind::NumberValue(number)) => {
            Value::Number(serde_json::Number::from_f64(number).ok_or(NonFiniteNumber)?)
        }
        Some(Kind::StringValue(text)) => Value::String(text),
        Some(Kind::BoolValue(flag)) => Value::Bool(flag),
        Some(Kind::StructValue(fields)) => prost_struct_to_json(fields)?,
        Some(Kind::ListValue(list)) => Value::Array(
            list.values
                .into_iter()
                .map(prost_value_to_json)
                .collect::<Result<_, _>>()?,
        ),
    })
}

/// Converts a JSON object into protobuf; non-object inputs return `None`.
pub fn json_to_prost_struct(value: &Value) -> Option<prost_types::Struct> {
    let fields = value
        .as_object()?
        .iter()
        .map(|(key, value)| (key.clone(), json_to_prost_value(value)))
        .collect();
    Some(prost_types::Struct { fields })
}

/// Converts a JSON value into protobuf, using the wire format's double-precision numbers.
pub fn json_to_prost_value(value: &Value) -> prost_types::Value {
    let kind = match value {
        Value::Null => Kind::NullValue(0),
        Value::Bool(flag) => Kind::BoolValue(*flag),
        // Without serde_json's arbitrary_precision feature, every Number is a finite f64,
        // i64 or u64; each has an f64 representation. Never replace a violated invariant with 0.
        Value::Number(number) => Kind::NumberValue(
            number
                .as_f64()
                .expect("JSON number must have an f64 representation"),
        ),
        Value::String(text) => Kind::StringValue(text.clone()),
        Value::Array(items) => Kind::ListValue(prost_types::ListValue {
            values: items.iter().map(json_to_prost_value).collect(),
        }),
        Value::Object(fields) => Kind::StructValue(prost_types::Struct {
            fields: fields
                .iter()
                .map(|(key, value)| (key.clone(), json_to_prost_value(value)))
                .collect(),
        }),
    };
    prost_types::Value { kind: Some(kind) }
}
