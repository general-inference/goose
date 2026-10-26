use goose_agent::operation::trailing_assistant_text;
use goose_provider_types::conversation::message::Message;

#[test]
fn final_answer_excludes_earlier_narration_and_hidden_content() {
    let mut messages = vec![
        Message::user().with_text("Create the app"),
        Message::assistant().with_text("I will create it"),
        Message::user()
            .with_text("Review the goal")
            .with_visibility(false, true),
        Message::assistant().with_text("Created app.js. "),
        Message::assistant()
            .with_text("hidden")
            .with_visibility(false, true),
        Message::assistant().with_text("Tests passed."),
    ];
    assert_eq!(
        trailing_assistant_text(&messages),
        "Created app.js. Tests passed."
    );
    messages.push(Message::user().with_text("Next request"));
    assert!(trailing_assistant_text(&messages).is_empty());
    assert!(trailing_assistant_text(&[]).is_empty());
}
