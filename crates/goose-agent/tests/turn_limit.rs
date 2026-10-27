use goose_agent::{
    operation::{ConversationEffect, Emitter, Operation, OperationResult},
    turn_limit::{is_turn_limit_message, MaxTurnsOperation},
};
use goose_provider_types::conversation::{message::Message, Conversation};
use rmcp::model::{CallToolRequestParams, CallToolResult};

#[tokio::test]
async fn settlement_preserves_last_answer_and_pending_tools_then_persists_exhaustion() {
    let limit = MaxTurnsOperation::new(1).with_tool_settlement();
    let (send, _receive) = tokio::sync::mpsc::unbounded_channel();
    let emit = Emitter::new(send, Default::default());
    for last in [
        Message::assistant().with_text("Done"),
        Message::assistant().with_tool_request("call", Ok(CallToolRequestParams::new("read"))),
    ] {
        let conversation =
            Conversation::new_unvalidated(vec![Message::user().with_text("Work"), last]);
        let result: OperationResult<ConversationEffect> =
            limit.run(&(), &conversation, &emit).await.unwrap();
        assert!(matches!(result, OperationResult::NotApplicable));
    }
    let mut conversation = Conversation::new_unvalidated(vec![
        Message::user().with_text("Work"),
        Message::assistant().with_tool_request("call", Ok(CallToolRequestParams::new("read"))),
        Message::user().with_tool_response("call", Ok(CallToolResult::success(vec![]))),
    ]);
    let result: OperationResult<ConversationEffect> =
        limit.run(&(), &conversation, &emit).await.unwrap();
    let OperationResult::Applied(result) = result else {
        panic!("budget must stop inference")
    };
    let ConversationEffect::AppendMessage(message) = &result.effects[0] else {
        panic!("missing stop")
    };
    assert!(is_turn_limit_message(message));
    let restored: Message = serde_json::from_str(&serde_json::to_string(message).unwrap()).unwrap();
    assert!(is_turn_limit_message(&restored));
    conversation.push(restored);
    conversation.push(Message::user().with_text("Continue"));
    let result: OperationResult<ConversationEffect> =
        limit.run(&(), &conversation, &emit).await.unwrap();
    assert!(matches!(result, OperationResult::NotApplicable));
}
