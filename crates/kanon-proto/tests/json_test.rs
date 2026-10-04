//! The wire codec preserves JSON values and rejects invalid protobuf numbers recursively.

use kanon_proto::json::{
    NonFiniteNumber, json_to_prost_struct, json_to_prost_value, prost_struct_to_json,
    prost_value_to_json,
};
use prost_types::value::Kind;
use serde_json::json;

fn number(value: f64) -> prost_types::Value {
    prost_types::Value {
        kind: Some(Kind::NumberValue(value)),
    }
}

#[test]
fn nested_json_roundtrips_and_safe_whole_numbers_become_integers() {
    let original = json!({
        "null": null,
        "nested": [true, "text", {"count": -42, "ratio": 0.125}],
        "min": -(1_i64 << 53),
        "max": 1_i64 << 53,
    });
    let encoded = json_to_prost_struct(&original).unwrap();
    assert_eq!(prost_struct_to_json(encoded).unwrap(), original);
    assert!(json_to_prost_struct(&json!([])).is_none());
    assert_eq!(
        prost_value_to_json(prost_types::Value { kind: None }).unwrap(),
        json!(null)
    );

    for value in [-(1_i64 << 53), -1, 0, 1, 1_i64 << 53] {
        let decoded = prost_value_to_json(number(value as f64)).unwrap();
        assert_eq!(decoded.as_i64(), Some(value));
        assert!(!decoded.is_f64());
    }
    // Values above the exact-integer boundary must not saturate through an i64 cast.
    for value in [((1_i64 << 53) + 2) as f64, i64::MAX as f64, 1e300, -1e300] {
        let decoded = prost_value_to_json(number(value)).unwrap();
        assert!(decoded.is_f64());
        assert_eq!(decoded.as_f64(), Some(value));
    }
    assert_eq!(
        prost_value_to_json(json_to_prost_value(&json!(3.0))).unwrap(),
        json!(3)
    );
}

#[test]
fn nonfinite_numbers_fail_even_inside_nested_lists_and_objects() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(prost_value_to_json(number(value)), Err(NonFiniteNumber));
        let nested = prost_types::Struct {
            fields: [(
                "items".into(),
                prost_types::Value {
                    kind: Some(Kind::ListValue(prost_types::ListValue {
                        values: vec![prost_types::Value {
                            kind: Some(Kind::StructValue(prost_types::Struct {
                                fields: [("value".into(), number(value))].into(),
                            })),
                        }],
                    })),
                },
            )]
            .into(),
        };
        assert_eq!(prost_struct_to_json(nested), Err(NonFiniteNumber));
    }
}
