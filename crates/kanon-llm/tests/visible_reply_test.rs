//! Tests for the final answer boundary in front of chat platforms.

use kanon_llm::visible_reply;

#[test]
fn plain_answers_pass_through_unchanged() {
    assert_eq!(
        visible_reply("  hello\n  world "),
        ("hello\n  world".into(), None)
    );
}

#[test]
fn reasoning_blocks_anywhere_become_plain_reasoning() {
    let (answer, reasoning) = visible_reply("<think>a</think>one <THINK >b</think> two");
    assert_eq!(answer, "one  two");
    assert_eq!(reasoning.as_deref(), Some("a\n\nb"));
}

#[test]
fn a_closing_tag_without_an_opening_one_ends_template_opened_reasoning() {
    let (answer, reasoning) = visible_reply("draft</think>\n\nfinal");
    assert_eq!(answer, "final");
    assert_eq!(reasoning.as_deref(), Some("draft"));
}

#[test]
fn an_unclosed_reasoning_block_hides_the_rest() {
    let (answer, reasoning) = visible_reply("answer <think>still going");
    assert_eq!(answer, "answer");
    assert_eq!(reasoning.as_deref(), Some("still going"));
}

#[test]
fn tool_call_markup_is_dropped_even_when_unparsable_or_truncated() {
    let text = "ok <tool_calls><tool_call>{bad</tool_call></tool_calls> then \
                <function=x><parameter=a>1</parameter></function> end <tool_call>{\"name\":";
    let (answer, reasoning) = visible_reply(text);
    assert_eq!(answer, "ok  then  end");
    assert_eq!(reasoning, None);
}

#[test]
fn similar_tag_names_are_left_alone() {
    let text = "<thinker> and <tool_callback> stay";
    assert_eq!(visible_reply(text), (text.to_string(), None));
}
