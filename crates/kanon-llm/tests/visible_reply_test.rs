//! Platform delivery receives an already normalized answer, not an untyped model completion.

use kanon_llm::visible_reply;

#[test]
fn answer_channel_preserves_reasoning_delimiters_and_examples() {
    for text in [
        "hello\n  world",
        "Use `<think>literal</think>` in examples",
        "```xml\n<think>outer<think>inner</think>end</think>\n```",
        "Here is <think>outer<think>inner</think>end</think> in prose",
        "<think>an intentional example in the answer channel</think>",
        "Explain </think> as a delimiter",
        "draft</think >answer",
        "<think >text</think >",
        "<thinker> and <tool_callback> stay",
    ] {
        assert_eq!(visible_reply(text), text);
    }
}

#[test]
fn unrecovered_tool_call_markup_is_answer_text() {
    // The agent already removed every block it could parse, so anything left is an example or a
    // malformed block; dropping it would mutilate the answer or leave nothing to send.
    for text in [
        "Models emit `<tool_call>{\"name\": ...}</tool_call>` blocks",
        "```xml\n<tool_calls><tool_call>{bad</tool_call></tool_calls>\n```",
        "<function=x><parameter=a>1</parameter></function>",
        "<tool_call>not a tool call at all</tool_call>",
        "truncated <tool_call>{\"name\":",
    ] {
        assert_eq!(visible_reply(text), text);
    }
}
