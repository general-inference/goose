//! Shared autonomous-turn budget enforcement.
use crate::operation::{
    assistant_turn_count, messages_since_kickoff, not_applicable, yielded_with, Emitter, Operation,
    OperationResult,
};
use anyhow::Result;
use async_trait::async_trait;
use goose_provider_types::conversation::{message::Message, Conversation};
use goose_provider_types::maybe_send::{MaybeSend, MaybeSync};
use rmcp::model::Role;

pub const MAX_TURNS_MESSAGE: &str = include_str!("prompts/turn_limit.md");

pub fn is_turn_limit_message(message: &Message) -> bool {
    message.metadata.operation_note("turn_limit", "exhausted") == Some(&serde_json::json!(true))
}

pub struct MaxTurnsOperation {
    max_turns: u32,
    settle_tools: bool,
}

impl MaxTurnsOperation {
    pub fn new(max_turns: u32) -> Self {
        Self {
            max_turns,
            settle_tools: false,
        }
    }

    /// Allow pending assistant tool calls to settle and accept a final answer
    /// on the last allowed turn. Register before tool dispatch and inference.
    pub fn with_tool_settlement(mut self) -> Self {
        self.settle_tools = true;
        self
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<S, E> Operation<S, E> for MaxTurnsOperation
where
    S: MaybeSend + MaybeSync,
    E: From<Message> + MaybeSend + 'static,
{
    fn name(&self) -> &'static str {
        "max_turns"
    }

    async fn moim_parts(&self, _session: &S, conversation: &Conversation) -> Result<Vec<String>> {
        let taken = assistant_turn_count(messages_since_kickoff(conversation)?);
        Ok(crate::turn_context::turn_budget_part(taken, self.max_turns)
            .into_iter()
            .collect())
    }

    async fn run(
        &self,
        _session: &S,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<E>> {
        let messages = messages_since_kickoff(conversation)?;
        if assistant_turn_count(messages) < self.max_turns
            || (self.settle_tools
                && messages
                    .last()
                    .is_some_and(|message| message.role == Role::Assistant))
        {
            return not_applicable();
        }
        let mut message = Message::assistant().with_text(MAX_TURNS_MESSAGE.trim_end());
        message
            .metadata
            .set_operation_note("turn_limit", "exhausted", serde_json::json!(true));
        yielded_with([emit.message(message).into()])
    }
}
