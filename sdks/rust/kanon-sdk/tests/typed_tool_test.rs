//! Typed tools: the parameters schema comes from the argument struct in the plain form every
//! model provider accepts, and the model's JSON arguments are decoded into that struct.

use std::collections::BTreeMap;

use kanon_sdk::prelude::*;
use kanon_sdk::schema::tool_parameters;

/// Looks up a forecast.
#[derive(Debug, Deserialize, JsonSchema)]
struct Forecast {
    /// City to look up.
    city: String,
    /// Days ahead.
    days: Option<u32>,
    /// Unit system.
    units: Units,
    /// Where exactly.
    location: Option<Location>,
    #[serde(default)]
    verbose: bool,
    labels: BTreeMap<String, i64>,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Units {
    Metric,
    Imperial,
}

/// A coordinate.
#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
struct Location {
    lat: f64,
    lon: f64,
}

#[derive(Deserialize, JsonSchema)]
struct NoArgs {}

#[derive(Deserialize, JsonSchema)]
struct Tree {
    #[allow(dead_code)]
    children: Vec<Tree>,
}

#[test]
fn schema_is_plain_inlined_json_schema_with_doc_comments() {
    let schema = tool_parameters::<Forecast>();
    let properties = &schema["properties"];

    assert_eq!(schema["type"], "object");
    // Nothing a provider might choke on, nor the Rust type name.
    for keyword in ["$schema", "title", "definitions", "$defs"] {
        assert!(schema.get(keyword).is_none(), "{keyword} in {schema}");
    }
    assert_eq!(
        properties["city"],
        json!({ "type": "string", "description": "City to look up." })
    );
    // `Option` makes a field optional instead of nullable, and schemars' `uint32` format (an
    // OpenAPI extension) is gone while the bound it implies stays.
    assert_eq!(
        properties["days"],
        json!({ "type": "integer", "minimum": 0, "description": "Days ahead." })
    );
    // Referenced types are inlined, and the field's doc comment wins over the type's.
    assert_eq!(
        properties["units"],
        json!({ "type": "string", "enum": ["metric", "imperial"], "description": "Unit system." })
    );
    assert_eq!(properties["location"]["description"], "Where exactly.");
    assert_eq!(properties["location"]["type"], "object");
    assert_eq!(properties["location"]["additionalProperties"], false);
    assert_eq!(properties["location"]["required"], json!(["lat", "lon"]));
    assert_eq!(
        properties["verbose"],
        json!({ "type": "boolean", "default": false })
    );
    assert_eq!(
        properties["labels"]["additionalProperties"],
        json!({ "type": "integer" })
    );
    assert_eq!(schema["required"], json!(["city", "units", "labels"]));
    // Extra fields are ignored by serde unless denied, so the root does not forbid them.
    assert!(schema.get("additionalProperties").is_none());
}

#[test]
fn argument_less_and_recursive_structs_still_give_object_schemas() {
    // Some OpenAI-compatible endpoints reject an object schema without `properties`.
    assert_eq!(
        tool_parameters::<NoArgs>(),
        json!({ "type": "object", "properties": {} })
    );
    // A recursive type cannot be inlined; it points back at the root, which still resolves.
    let tree = tool_parameters::<Tree>();
    assert_eq!(
        tree["properties"]["children"]["items"],
        json!({ "$ref": "#" })
    );
}

#[test]
#[should_panic(expected = "must be a struct with named fields")]
fn non_object_arguments_are_refused_at_registration() {
    let _ = ToolSpec::typed::<Vec<String>>("bad");
}

fn tool_request(name: &str, args: serde_json::Value) -> ToolCallRequest {
    let serde_json::Value::Object(args) = args else {
        panic!("arguments are an object");
    };
    ToolCallRequest {
        call_id: "call-1".into(),
        tool_name: name.into(),
        session_id: "s1".into(),
        payload: Some(tool_call_request::Payload::StructuredArgs(
            kanon_sdk::json::to_struct(args),
        )),
        context: None,
    }
}

fn result_json(response: &ToolCallResponse) -> serde_json::Value {
    match &response.payload {
        Some(tool_call_response::Payload::StructuredResult(result)) => {
            serde_json::Value::Object(kanon_sdk::json::from_struct(result.clone()))
        }
        other => panic!("no structured result: {other:?}"),
    }
}

fn forecast_router() -> Router {
    Router::new("test.plugin", "Test", "0.1.0").tool(
        ToolSpec::typed::<Forecast>("forecast").description("Get the weather"),
        |args, _event| async move {
            Ok(json!({
                "city": args.city,
                "days": args.days,
                "metric": args.units == Units::Metric,
                "location": args.location.map(|at| [at.lat, at.lon]),
                "verbose": args.verbose,
                "labels": args.labels,
            }))
        },
    )
}

#[tokio::test]
async fn typed_tools_declare_their_schema_and_receive_decoded_arguments() {
    let router = forecast_router();
    let meta = router.meta();
    assert_eq!(meta.tools.len(), 1);
    assert_eq!(meta.tools[0].description, "Get the weather");
    let declared = kanon_sdk::json::from_struct(meta.tools[0].parameters.clone().unwrap());
    let serde_json::Value::Object(expected) = tool_parameters::<Forecast>() else {
        panic!("schemas are objects");
    };
    // Compared after the same protobuf round trip, which turns integers into doubles.
    assert_eq!(
        declared,
        kanon_sdk::json::from_struct(kanon_sdk::json::to_struct(expected))
    );

    // Protobuf carries every number as a double; `3.0` must still fill the `u32`.
    let response = router
        .on_call_tool(tool_request(
            "forecast",
            json!({
                "city": "Paris",
                "days": 3,
                "units": "metric",
                "location": { "lat": 48.5, "lon": 2.25 },
                "labels": { "a": 1 },
            }),
        ))
        .await
        .unwrap();
    assert!(response.success, "{}", response.error_message);
    assert_eq!(
        result_json(&response),
        json!({
            "city": "Paris",
            "days": 3.0,
            "metric": true,
            "location": [48.5, 2.25],
            "verbose": false,
            "labels": { "a": 1.0 },
        })
    );
}

#[tokio::test]
async fn bad_arguments_fail_the_call_for_the_model_without_running_the_handler() {
    let router = forecast_router();
    for (args, problem) in [
        (
            json!({ "units": "metric", "labels": {} }),
            "missing field `city`",
        ),
        (
            json!({ "city": "Paris", "units": "kelvin", "labels": {} }),
            "unknown variant `kelvin`",
        ),
        (
            json!({ "city": "Paris", "days": 1.5, "units": "metric", "labels": {} }),
            "invalid type",
        ),
        (
            json!({
                "city": "Paris",
                "units": "metric",
                "labels": {},
                "location": { "lat": 1, "lon": 2, "alt": 3 },
            }),
            "unknown field `alt`",
        ),
    ] {
        let response = router
            .on_call_tool(tool_request("forecast", args))
            .await
            .unwrap();
        assert!(!response.success);
        assert!(
            response
                .error_message
                .starts_with("invalid arguments for tool 'forecast': "),
            "{}",
            response.error_message
        );
        assert!(
            response.error_message.contains(problem),
            "{}",
            response.error_message
        );
    }
}

#[tokio::test]
async fn typed_results_are_serialized_and_scalars_wrapped() {
    #[derive(Serialize)]
    struct Total {
        sum: i64,
    }
    #[derive(Deserialize, JsonSchema)]
    struct Pair {
        a: i64,
        b: i64,
    }
    let router = Router::new("test.plugin", "Test", "0.1.0")
        .tool(ToolSpec::typed::<Pair>("add"), |pair, _event| async move {
            Ok(Total {
                sum: pair.a + pair.b,
            })
        })
        .tool(ToolSpec::typed::<Pair>("sub"), |pair, _event| async move {
            Ok(pair.a - pair.b)
        });

    let added = router
        .on_call_tool(tool_request("add", json!({ "a": 2, "b": 3 })))
        .await
        .unwrap();
    assert_eq!(result_json(&added), json!({ "sum": 5.0 }));
    let subtracted = router
        .on_call_tool(tool_request("sub", json!({ "a": 2, "b": 3 })))
        .await
        .unwrap();
    assert_eq!(result_json(&subtracted), json!({ "result": -1.0 }));
}

#[test]
#[should_panic(expected = "tool 'add' is already declared")]
fn tool_names_must_be_unique() {
    let _ = Router::new("test.plugin", "Test", "0.1.0")
        .tool(ToolSpec::new("add"), |args, _event| async move { Ok(args) })
        .tool(ToolSpec::new("add"), |args, _event| async move { Ok(args) });
}
