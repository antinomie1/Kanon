//! Tests for recovering tool calls that a model emitted as text markup.
//!
//! The failure this guards against is concrete: a model that ignores the `tools` request field
//! answers with `<tool_call>...` markup, the pipeline forwards that markup to the chat platform as
//! the assistant's answer, and the tool never runs. These tests pin the accepted markup shapes and
//! the rule that unparsable markup is preserved rather than swallowed.

use kanon_llm::extract_textual_tool_calls;
use serde_json::json;

#[test]
fn recovers_the_function_parameter_markup_used_by_mimo_style_models() {
    let content = "好的，正在查询。\n<tool_call><function=mcp__maimai__mai_play_score>\
<parameter=output_format>image</parameter><parameter=qq>1705702687</parameter>\
<parameter=song_name>忙シー日</parameter></function></tool_call>";

    let (calls, cleaned) = extract_textual_tool_calls(content);

    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "mcp__maimai__mai_play_score");
    assert_eq!(calls[0].arguments["output_format"], json!("image"));
    // Numeric parameters are coerced so a tool schema expecting a number receives one.
    assert_eq!(calls[0].arguments["qq"], json!(1705702687u64));
    assert_eq!(calls[0].arguments["song_name"], json!("忙シー日"));
    assert!(
        !calls[0].id.is_empty(),
        "a recovered call must carry an id so its result can be matched"
    );
    // Surrounding prose stays as the visible answer.
    assert_eq!(cleaned, "好的，正在查询。");
}

#[test]
fn recovers_attribute_style_function_and_parameter_tags() {
    let content = r#"<tool_call><function name="weather"><parameter name="city">北京</parameter></function></tool_call>"#;

    let (calls, cleaned) = extract_textual_tool_calls(content);

    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "weather");
    assert_eq!(calls[0].arguments["city"], json!("北京"));
    assert!(cleaned.is_empty());
}

#[test]
fn recovers_json_bodies_including_a_stringified_arguments_payload() {
    let content = r#"<tool_call>{"name": "search", "arguments": "{\"q\": \"kanon\"}"}</tool_call>"#;

    let (calls, cleaned) = extract_textual_tool_calls(content);

    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "search");
    assert_eq!(calls[0].arguments["q"], json!("kanon"));
    assert!(cleaned.is_empty());
}

#[test]
fn recovers_the_openai_shaped_json_body() {
    let content =
        r#"<tool_call>{"function": {"name": "lookup", "arguments": {"id": 7}}}</tool_call>"#;

    let (calls, _cleaned) = extract_textual_tool_calls(content);

    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "lookup");
    assert_eq!(calls[0].arguments["id"], json!(7));
}

#[test]
fn recovers_several_blocks_from_one_message() {
    let content = "<tool_call><function=a><parameter=x>1</parameter></function></tool_call> and \
<tool_call><function=b><parameter=y>two</parameter></function></tool_call>";

    let (calls, cleaned) = extract_textual_tool_calls(content);

    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "a");
    assert_eq!(calls[1].name, "b");
    assert_eq!(cleaned, "and");
}

#[test]
fn a_tool_calls_wrapper_goes_with_the_calls_it_held() {
    let content = "<tool_calls>\n<tool_call>{\"name\": \"a\"}</tool_call>\n</tool_calls> done";
    let (calls, cleaned) = extract_textual_tool_calls(content);
    assert_eq!(calls.len(), 1);
    assert_eq!(cleaned, "done");

    // A wrapper that still holds an unparsable block stays around it.
    let content = "<tool_calls><tool_call>{\"name\": \"a\"}</tool_call>\
<tool_call>bad</tool_call></tool_calls>";
    let (calls, cleaned) = extract_textual_tool_calls(content);
    assert_eq!(calls.len(), 1);
    assert_eq!(
        cleaned,
        "<tool_calls><tool_call>bad</tool_call></tool_calls>"
    );
}

#[test]
fn malformed_markup_is_preserved_instead_of_silently_dropped() {
    let content = "<tool_call>not a tool call at all</tool_call>";

    let (calls, cleaned) = extract_textual_tool_calls(content);

    assert!(calls.is_empty());
    assert_eq!(
        cleaned, content,
        "an unparsable block must stay visible rather than vanish"
    );
}

#[test]
fn plain_text_is_returned_unchanged() {
    let content = "just a normal reply";

    let (calls, cleaned) = extract_textual_tool_calls(content);

    assert!(calls.is_empty());
    assert_eq!(cleaned, content);
}
