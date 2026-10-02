//! Tool parameter schemas inferred from Rust types.
//!
//! [`tool_parameters`] turns a `#[derive(Deserialize, JsonSchema)]` struct into the JSON Schema a
//! tool declares. Doc comments on the fields become their descriptions.
//!
//! Model providers accept only a plain subset of JSON Schema, and each rejects different extras,
//! so the generated schema is reduced to what all of them take:
//!
//! - draft-07 keywords only, with every definition inlined (no `$ref`, except for recursive
//!   types, which cannot be inlined), and no `$schema` or root `title`;
//! - `Option<T>` fields are simply not `required` — their `null` alternative is dropped, since a
//!   missing field and `null` deserialize the same way and several providers reject type lists;
//! - `format` is kept only where JSON Schema defines it (`date-time`, `email`, ...): schemars'
//!   numeric formats such as `uint32` are OpenAPI extensions some providers refuse;
//! - `additionalProperties` stays as serde decides: absent (extra fields are ignored) unless the
//!   struct uses `#[serde(deny_unknown_fields)]`, which yields `false`.
//!
//! The schema is part of the model's request prefix, so it is generated once, at registration,
//! and stays byte-identical for the lifetime of the plugin.

use schemars::generate::SchemaSettings;
use schemars::transform::{ReplaceBoolSchemas, RestrictFormats, Transform, transform_subschemas};
use schemars::{JsonSchema, Schema};
use serde_json::{Map, Value};

/// The parameters schema of a tool whose arguments deserialize into `T`.
///
/// # Panics
/// If `T` does not describe a JSON object (a tool's arguments always are one): use a struct with
/// named fields. A schema mistake should stop the plugin at startup, not surface mid-conversation.
pub fn tool_parameters<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::draft07()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.transforms.push(Box::new(DropNull));
            let mut replace_bools = ReplaceBoolSchemas::default();
            // `additionalProperties: false` is meaningful and every provider accepts it.
            replace_bools.skip_additional_properties = true;
            settings.transforms.push(Box::new(replace_bools));
            // Reads `$schema` (still present at this point) to keep the formats draft-07 defines.
            settings
                .transforms
                .push(Box::new(RestrictFormats::default()));
        })
        .into_generator();
    let Value::Object(mut root) = generator.into_root_schema_for::<T>().to_value() else {
        panic!(
            "tool arguments `{}` must be a struct with named fields",
            T::schema_name()
        );
    };
    root.remove("$schema");
    // The Rust type name means nothing to the model and only lengthens the request.
    root.remove("title");
    assert!(
        root.get("type").and_then(Value::as_str) == Some("object"),
        "tool arguments `{}` must be a struct with named fields, got schema {}",
        T::schema_name(),
        Value::Object(root.clone()),
    );
    // Some OpenAI-compatible endpoints reject an object schema without `properties`, which is
    // what an argument-less struct produces.
    root.entry("properties")
        .or_insert_with(|| Value::Object(Map::new()));
    Value::Object(root)
}

/// Removes the `null` alternative schemars adds for `Option<T>`: `"type": ["string", "null"]`
/// becomes `"type": "string"`, and `anyOf: [S, {"type": "null"}]` becomes `S` (keeping the
/// field's own description and default).
#[derive(Clone)]
struct DropNull;

impl Transform for DropNull {
    fn transform(&mut self, schema: &mut Schema) {
        if let Some(object) = schema.as_object_mut() {
            if let Some(Value::Array(types)) = object.get("type") {
                let kept: Vec<Value> = types
                    .iter()
                    .filter(|kind| kind.as_str() != Some("null"))
                    .cloned()
                    .collect();
                if kept.len() == 1 && kept.len() < types.len() {
                    object.insert("type".to_string(), kept[0].clone());
                }
            }
            for keyword in ["anyOf", "oneOf"] {
                let Some(Value::Array(branches)) = object.get(keyword) else {
                    continue;
                };
                let is_null =
                    |branch: &Value| branch.get("type").and_then(Value::as_str) == Some("null");
                // Exactly `[S, null]` (in either order); real unions are left alone.
                let [first, second] = branches.as_slice() else {
                    continue;
                };
                let only = match (is_null(first), is_null(second)) {
                    (false, true) => first,
                    (true, false) => second,
                    _ => continue,
                };
                let Value::Object(only) = only else {
                    continue;
                };
                let mut merged = only.clone();
                object.remove(keyword);
                // The field's own keywords (its doc comment, its default) describe this use of
                // the type better than the type's own, so they win.
                for (key, value) in std::mem::take(object) {
                    merged.insert(key, value);
                }
                *object = merged;
                break;
            }
        }
        transform_subschemas(self, schema);
    }
}
