//! Tests for conversation classification and the reply policy.
//!
//! The policy decides whether a bot answers at all, so its defaults and its treatment of private
//! conversations are pinned here: an adapter that reports nothing must keep behaving exactly as it
//! did before the policy existed.

use kanon_core::{
    ConversationKind, META_BOT_MENTIONED, META_CONVERSATION_KIND, ReplyMode, ReplyPolicy,
    ReplyPolicyStore, bot_mentioned,
};
use kanon_proto::prost_types;

/// Builds an event metadata struct from typed fields.
fn metadata(fields: Vec<(&str, prost_types::Value)>) -> prost_types::Struct {
    prost_types::Struct {
        fields: fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    }
}

/// Builds a string-valued metadata entry.
fn string_value(value: &str) -> prost_types::Value {
    prost_types::Value {
        kind: Some(prost_types::value::Kind::StringValue(value.to_string())),
    }
}

/// Builds a boolean metadata entry.
fn bool_value(value: bool) -> prost_types::Value {
    prost_types::Value {
        kind: Some(prost_types::value::Kind::BoolValue(value)),
    }
}

#[test]
fn an_adapter_that_reports_nothing_is_treated_as_private() {
    assert_eq!(
        ConversationKind::from_metadata(None),
        ConversationKind::Private
    );
    assert!(
        !ConversationKind::Private.is_policy_governed(),
        "a direct message must always be answered"
    );
}

#[test]
fn conversation_kinds_are_read_from_the_platform_neutral_key() {
    let group = metadata(vec![(META_CONVERSATION_KIND, string_value("group"))]);
    let channel = metadata(vec![(META_CONVERSATION_KIND, string_value("channel"))]);
    let private = metadata(vec![(META_CONVERSATION_KIND, string_value("private"))]);
    // An unknown value is not a group: guessing would silently silence a bot.
    let unknown = metadata(vec![(META_CONVERSATION_KIND, string_value("spaceship"))]);

    assert_eq!(
        ConversationKind::from_metadata(Some(&group)),
        ConversationKind::Group
    );
    assert_eq!(
        ConversationKind::from_metadata(Some(&channel)),
        ConversationKind::Channel
    );
    assert_eq!(
        ConversationKind::from_metadata(Some(&private)),
        ConversationKind::Private
    );
    assert_eq!(
        ConversationKind::from_metadata(Some(&unknown)),
        ConversationKind::Private
    );
}

#[test]
fn mention_reports_are_read_from_the_platform_neutral_key() {
    let mentioned = metadata(vec![(META_BOT_MENTIONED, bool_value(true))]);
    let not_mentioned = metadata(vec![(META_BOT_MENTIONED, bool_value(false))]);

    assert!(bot_mentioned(Some(&mentioned)));
    assert!(!bot_mentioned(Some(&not_mentioned)));
    assert!(!bot_mentioned(None));
}

#[test]
fn private_conversations_are_always_answered() {
    for mode in [
        ReplyMode::Never,
        ReplyMode::Mention,
        ReplyMode::Probability,
        ReplyMode::Always,
    ] {
        let policy = ReplyPolicy::new(mode);
        assert!(
            policy.should_reply(ConversationKind::Private, false, 0.99),
            "mode {mode:?} must not silence a direct message"
        );
    }
}

#[test]
fn group_decisions_follow_the_mode() {
    let always = ReplyPolicy::new(ReplyMode::Always);
    let never = ReplyPolicy::new(ReplyMode::Never);
    let mention = ReplyPolicy::new(ReplyMode::Mention);

    assert!(always.should_reply(ConversationKind::Group, false, 0.0));
    assert!(!never.should_reply(ConversationKind::Group, true, 0.0));
    assert!(mention.should_reply(ConversationKind::Group, true, 0.0));
    assert!(!mention.should_reply(ConversationKind::Group, false, 0.0));
    // A channel is governed exactly like a group.
    assert!(!mention.should_reply(ConversationKind::Channel, false, 0.0));
}

#[test]
fn probability_mode_uses_the_supplied_sample() {
    let policy = ReplyPolicy {
        mode: ReplyMode::Probability,
        probability: 0.25,
        ..Default::default()
    };

    assert!(policy.should_reply(ConversationKind::Group, false, 0.24));
    assert!(!policy.should_reply(ConversationKind::Group, false, 0.25));
    assert!(!policy.should_reply(ConversationKind::Group, true, 0.90));
}

#[test]
fn an_out_of_range_probability_is_rejected_rather_than_clamped() {
    let too_big = ReplyPolicy {
        mode: ReplyMode::Probability,
        probability: 5.0,
        ..Default::default()
    };
    let negative = ReplyPolicy {
        mode: ReplyMode::Probability,
        probability: -0.1,
        ..Default::default()
    };

    assert!(too_big.validate().is_err());
    assert!(negative.validate().is_err());
    assert!(ReplyPolicy::default().validate().is_ok());
}

#[test]
fn the_policy_store_is_hot_swappable() {
    let store = ReplyPolicyStore::default();
    assert_eq!(store.get().mode, ReplyMode::Always);

    store.set(ReplyPolicy::new(ReplyMode::Mention));
    assert_eq!(store.get().mode, ReplyMode::Mention);
}

#[test]
fn the_policy_renders_a_human_readable_description() {
    assert_eq!(
        ReplyPolicy::new(ReplyMode::Mention).describe(),
        "mention only"
    );
    assert_eq!(
        ReplyPolicy {
            mode: ReplyMode::Probability,
            probability: 0.5,
            ..Default::default()
        }
        .describe(),
        "probability 50%"
    );
}
