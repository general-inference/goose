use goose_agent::turn_context::{system_prompt_block, turn_budget_part, turn_context_event};
use std::path::Path;

#[test]
fn host_context_is_stable_agent_only_and_escapes_host_fields() {
    let event = || {
        turn_context_event(
            Path::new("/workspace/<project>&files"),
            Some(131_072),
            vec!["  ".into(), "Current project instructions".into()],
            "2026-10-27 12:34:00 +11:00",
        )
        .unwrap()
    };
    let first = event();
    assert!(first.is_turn_context());
    assert!(first.is_agent_visible());
    assert!(!first.is_user_visible());
    assert_eq!(first.as_concat_text(), event().as_concat_text());
    assert_eq!(first.as_concat_text(), "<turn-context>\n<current-time>2026-10-27 12:34:00 +11:00</current-time>\n<working-directory>/workspace/&lt;project&gt;&amp;files</working-directory>\n\nCurrent project instructions\n</turn-context>");
    assert!(system_prompt_block().contains("only the most recent block is current"));
}

#[test]
fn preserves_goose_context_and_turn_budget_thresholds() {
    for (limit, enabled) in [(None, true), (Some(16_000), false), (Some(32_000), true)] {
        assert_eq!(
            turn_context_event(Path::new("."), limit, vec![], "time").is_some(),
            enabled
        );
    }
    for (taken, maximum, expected) in [
        (0, 0, None),
        (14, 30, None),
        (15, 30, Some("<turn-budget>15/30 used</turn-budget>")),
        (30, 30, Some("<turn-budget>30/30 used</turn-budget>")),
    ] {
        assert_eq!(turn_budget_part(taken, maximum).as_deref(), expected);
    }
}

#[test]
fn stable_context_is_not_repeated_for_goal_review() {
    use goose_agent::{goal::review_message, turn_context::update};
    use goose_provider_types::conversation::message::Message;
    let event = || turn_context_event(Path::new("/workspace"), None, vec![], "time");
    let mut messages = vec![Message::user().with_text("Implement the feature")];
    assert!(update(event(), &messages).is_some());
    messages.push(event().unwrap());
    messages.push(Message::assistant().with_text("Implemented and tested."));
    assert!(update(event(), &messages).is_none());
    messages.push(review_message("Implement the feature", &messages).unwrap());
    assert!(update(event(), &messages).is_none());
    messages.push(event().unwrap());
    assert!(update(event(), &messages).is_none());
}
