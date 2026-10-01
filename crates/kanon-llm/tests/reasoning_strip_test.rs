//! Regression tests for the legacy user-visible reasoning boundary.

use kanon_llm::strip_reasoning_tags;

#[test]
fn strips_a_complete_reasoning_block() {
    let completion =
        "<think>\nThe user greeted me; I should greet back.\n</think>\n\n你好！我是黑猪AI。";
    assert_eq!(strip_reasoning_tags(completion), "你好！我是黑猪AI。");
}

#[test]
fn leaves_a_plain_answer_untouched() {
    let completion = "你好！我是黑猪AI。";
    assert_eq!(strip_reasoning_tags(completion), completion);
}

#[test]
fn handles_reasoning_only_and_truncated_blocks() {
    // A reasoning-only completion carries no answer for the user.
    assert_eq!(
        strip_reasoning_tags("<think>\nstill thinking\n</think>"),
        ""
    );

    // A stream cut off mid-reasoning must not leak the partial chain of thought.
    assert_eq!(strip_reasoning_tags("<think>\nstill thinking"), "");
}

#[test]
fn does_not_strip_a_block_that_is_not_leading() {
    // Only the leading block is the reasoning channel; a literal mention inside an answer stays.
    let completion = "我来解释 <think> 标签：它是推理标记。";
    assert_eq!(strip_reasoning_tags(completion), completion);
}

#[test]
fn consumes_consecutive_nested_and_case_variant_envelopes() {
    for text in [
        "<think>private-a</think><think>private-b</think>answer",
        " \n<THINK>private-a<think>private-b</think>private-c</THINK>\nanswer",
        "<think></think>\n<think>private-b</think>answer",
    ] {
        assert_eq!(strip_reasoning_tags(text), "answer");
    }
    for text in [
        "<think>a</think><think>b",
        "<think>a<think>b</think>c",
        "<think>a</think><thi",
        "<thi",
        "<THINK",
    ] {
        assert_eq!(strip_reasoning_tags(text), "");
    }
}

#[test]
fn preserves_literal_explanations_and_code_byte_for_byte() {
    for text in [
        "  Ordinary answer.\n",
        "Use <think> and </think> as delimiters.",
        "`<think>example</think>`",
        "```xml\n<think>example</think>\n```",
        "<thinking>not an envelope</thinking>",
        "<think >not a standard envelope</think >",
        "private draft</think >public answer",
        "<THINK ",
        "1 < 2",
    ] {
        assert_eq!(strip_reasoning_tags(text), text);
    }
    let code = "```xml\n<think>example</think>\n```";
    assert_eq!(
        strip_reasoning_tags(&format!("<think>private</think>\n{code}")),
        code
    );
}
