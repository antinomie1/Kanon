//! Normal legacy envelopes are separated; ordinary examples and incomplete output stay text.

use kanon_llm::strip_reasoning_tags;

#[test]
fn separates_a_complete_standard_leading_block() {
    assert_eq!(
        strip_reasoning_tags("  <think>private</think>\nanswer"),
        "answer"
    );
    assert_eq!(strip_reasoning_tags("<think>private</think>"), "");
}

#[test]
fn leaves_examples_and_nonstandard_output_untouched() {
    for text in [
        "  Ordinary answer.\n",
        "Use <think> and </think> as delimiters.",
        "`<think>example</think>`",
        "```xml\n<think>example</think>\n```",
        "<think >not a standard envelope</think >",
        "draft</think>answer",
        "<think>unfinished",
        "<thi",
    ] {
        assert_eq!(strip_reasoning_tags(text), text);
    }
}
