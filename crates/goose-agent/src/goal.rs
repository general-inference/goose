//! The same one-time goal review used by Goose's retry operation.
use anyhow::Result;
use async_trait::async_trait;
use goose_provider_types::conversation::{message::Message, Conversation};
use goose_provider_types::maybe_send::{MaybeSend, MaybeSync};

use crate::inference::is_empty_response_marker;
use crate::operation::{
    applied, ends_turn, messages_since_kickoff, not_applicable, Emitter, Operation, OperationResult,
};

pub const NUDGED: &str = "nudged";
const SCOPE: &str = "retry";

/// Returns one persisted review request per user turn. The legacy metadata scope
/// keeps reviews made by Goose before extraction recognizable after restoration.
/// This prompts self-review; it does not prove the goal was met.
pub fn review_message(goal: &str, messages: &[Message]) -> Option<Message> {
    if !ends_turn(messages)
        || messages
            .iter()
            .any(|message| message.metadata.operation_note(SCOPE, NUDGED).is_some())
        || messages.last().is_some_and(|message| {
            message.metadata.output_token_limit_reached || is_empty_response_marker(message)
        })
    {
        return None;
    }
    let mut message = Message::user()
        .with_text(
            include_str!("prompts/goal_review.md")
                .trim_end()
                .replace("{{goal}}", goal),
        )
        .with_visibility(false, true);
    message
        .metadata
        .set_operation_note(SCOPE, NUDGED, serde_json::json!(true));
    Some(message)
}

/// Register before inference to give a completed answer one opportunity for
/// goal review. Tool execution, approval, cancellation and turn limits remain
/// the responsibility of the surrounding pipeline.
pub struct GoalReviewOperation {
    goal: String,
}

impl GoalReviewOperation {
    pub fn new(goal: impl Into<String>) -> Self {
        Self { goal: goal.into() }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<S, E> Operation<S, E> for GoalReviewOperation
where
    S: MaybeSend + MaybeSync,
    E: From<Message> + MaybeSend + 'static,
{
    fn name(&self) -> &'static str {
        "goal_review"
    }

    async fn run(
        &self,
        _session: &S,
        conversation: &Conversation,
        _emit: &Emitter,
    ) -> Result<OperationResult<E>> {
        match review_message(&self.goal, messages_since_kickoff(conversation)?) {
            Some(message) => applied([message.into()]),
            None => not_applicable(),
        }
    }
}
