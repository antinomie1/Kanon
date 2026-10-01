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
fn tool_call_markup_is_dropped_even_when_unparsable_or_truncated() {
    let text = "ok <tool_calls><tool_call>{bad</tool_call></tool_calls> then \
                <function=x><parameter=a>1</parameter></function> end <tool_call>{\"name\":";
    assert_eq!(visible_reply(text), "ok  then  end");
}
