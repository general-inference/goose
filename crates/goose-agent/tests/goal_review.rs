use goose_agent::goal::{review_message, GoalReviewOperation};
use goose_agent::operation::{ConversationEffect, Emitter, Operation, OperationResult};
use goose_provider_types::conversation::{
    message::{Message, MessageErrorKind},
    Conversation,
};
use rmcp::model::CallToolRequestParams;

#[tokio::test]
async fn reviews_an_unfinished_answer_once_across_restore_then_reviews_the_next_turn() {
    let goal = "Fix the test and run it";
    let review = GoalReviewOperation::new(goal);
    let mut conversation = Conversation::new_unvalidated(vec![
        Message::user().with_text(goal),
        Message::assistant().with_text("I need to fix the missing variable."),
    ]);
    let (send, _receive) = tokio::sync::mpsc::unbounded_channel();
    let emit = Emitter::new(send, Default::default());
    let result: OperationResult<ConversationEffect> =
        review.run(&(), &conversation, &emit).await.unwrap();
    let OperationResult::Applied(result) = result else {
        panic!("review did not apply")
    };
    let ConversationEffect::AppendMessage(message) = &result.effects[0] else {
        panic!("no review message")
    };
    assert!(!message.is_user_visible());
    assert!(message.is_agent_visible());
    assert!(message.as_concat_text().contains(goal));
    conversation.push(message.clone());
    conversation.push(Message::assistant().with_text("Fixed and tested."));
    let persisted = serde_json::to_string(&conversation).unwrap();
    let mut restored: Conversation = serde_json::from_str(&persisted).unwrap();
    let second: OperationResult<ConversationEffect> = GoalReviewOperation::new(goal)
        .run(&(), &restored, &emit)
        .await
        .unwrap();
    assert!(matches!(second, OperationResult::NotApplicable));
    restored.push(Message::user().with_text("Explain the fix without editing files"));
    restored.push(Message::assistant().with_text("The variable is now initialized."));
    let next: OperationResult<ConversationEffect> =
        GoalReviewOperation::new("Explain the fix without editing files")
            .run(&(), &restored, &emit)
            .await
            .unwrap();
    assert!(matches!(next, OperationResult::Applied(_)));
}

#[test]
fn does_not_turn_errors_limits_or_pending_actions_into_a_review() {
    let mut limited = Message::assistant().with_text("Partial response");
    limited.metadata.output_token_limit_reached = true;
    for last in [
        Message::user().with_text("Fix the test"),
        Message::assistant().with_error(MessageErrorKind::Other, "model failed"),
        Message::assistant().with_tool_request("read", Ok(CallToolRequestParams::new("read"))),
        limited,
    ] {
        assert!(review_message(
            "Fix the test",
            &[Message::user().with_text("Fix the test"), last]
        )
        .is_none());
    }
}
