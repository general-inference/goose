//! Adds queued user guidance when the agent is between model and tool turns.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::agents::state_machine::effects::GooseEffect;
use crate::agents::state_machine::{Emitter, Operation, OperationResult};
use crate::conversation::message::Message;
use crate::conversation::Conversation;
use crate::hooks::{HookContext, HookEvent, HookManager};
use crate::session::Session;
pub(crate) use goose_agent::steering::SteerQueue;
use goose_agent::steering::{SteerHook, SteerOperation as SharedSteerOperation};

pub struct SteerOperation(SharedSteerOperation<Session>);

struct PromptHook(HookManager);

#[async_trait]
impl SteerHook<Session> for PromptHook {
    async fn submitted(&self, session: &Session, message: &Message) {
        let context = HookContext::new(HookEvent::UserPromptSubmit, &session.id)
            .with_message(message.agent_visible_content().as_concat_text());
        self.0.emit(HookEvent::UserPromptSubmit, context).await;
    }
}

impl SteerOperation {
    pub(crate) fn new(queue: SteerQueue, hook_manager: HookManager) -> Self {
        Self(SharedSteerOperation::new(queue).with_hook(Arc::new(PromptHook(hook_manager))))
    }
}

#[async_trait]
impl Operation<Session, GooseEffect> for SteerOperation {
    fn name(&self) -> &'static str {
        "steer"
    }

    async fn finalize_cancellation(
        &self,
        session: &Session,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Vec<GooseEffect> {
        self.0
            .finalize_cancellation(session, conversation, emit)
            .await
    }

    async fn run(
        &self,
        session: &Session,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<GooseEffect>> {
        self.0.run(session, conversation, emit).await
    }
}
