//! Compacts conversation history when it is too large for the configured context window.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::operation::{
    applied, last_effective_role, messages_since_kickoff, not_applicable, trailing_error,
    yielded_with, ConversationEffect, Emitter, Operation, OperationResult, SlashCommand,
};
use goose_context_management::{conversation::CompactionResult, TokenEstimator};
use goose_provider_types::conversation::message::{
    Message, MessageErrorKind, SystemNotificationType,
};
use goose_provider_types::conversation::{Conversation, EffectiveRole};
use goose_provider_types::errors::ProviderError;

/// Session usage is a hint; tool results since the last inference are counted separately.
pub trait CompactionSession: Send + Sync {
    fn context_tokens(&self) -> Option<i32> {
        None
    }
}

/// Supplies session-specific model selection, prompt overrides, and telemetry.
#[async_trait]
pub trait SessionCompactor<S>: Send + Sync {
    async fn compact(
        &self,
        session: &S,
        conversation: &Conversation,
        manual: bool,
    ) -> Result<CompactionResult>;
}

/// The standard GDK conversation transformation with a host-supplied model.
pub struct ModelCompactor {
    pub model: Arc<dyn goose_context_management::CompactionModel>,
    pub estimator: Arc<dyn TokenEstimator>,
    pub templates: goose_context_management::Templates,
}

#[async_trait]
impl<S: Sync> SessionCompactor<S> for ModelCompactor {
    async fn compact(
        &self,
        _session: &S,
        conversation: &Conversation,
        manual: bool,
    ) -> Result<CompactionResult> {
        goose_context_management::conversation::compact_messages(
            self.model.as_ref(),
            Some(self.estimator.as_ref()),
            &self.templates,
            conversation,
            manual,
        )
        .await
    }
}

/// Persist conversation replacement and summarization usage as one effect.
pub trait CompactionEffect:
    From<Message> + From<Conversation> + From<ConversationEffect> + Send + 'static
{
    fn compact_conversation(result: CompactionResult) -> Self;
}

const COMPACTION_THINKING_TEXT: &str = "goose is compacting the conversation...";

pub const MAX_CONTEXT_ERROR_COMPACTIONS: usize = 2;

/// Reported usage stops at the inference that requested the tools, so the
/// results that answered it are not counted until the next request.
async fn unreported_tool_tokens(
    conversation: &Conversation,
    estimator: &dyn TokenEstimator,
) -> Result<i32> {
    let messages = conversation.messages();
    if last_effective_role(messages)? != EffectiveRole::Tool {
        return Ok(0);
    }
    let after_request = messages
        .iter()
        .rposition(Message::is_tool_call)
        .map_or(0, |index| index + 1);
    count_context_tokens(&messages[after_request..], estimator).await
}

async fn count_context_tokens(messages: &[Message], estimator: &dyn TokenEstimator) -> Result<i32> {
    let mut total = 0usize;
    for message in messages.iter().filter(|message| message.is_agent_visible()) {
        total = total.saturating_add(
            estimator
                .count_chat_tokens("", std::slice::from_ref(message))
                .await,
        );
    }
    Ok(total.try_into()?)
}

pub struct CompactionOperation<S> {
    compactor: Arc<dyn SessionCompactor<S>>,
    estimator: Arc<dyn TokenEstimator>,
    context_limit: usize,
    threshold: f64,
}

impl<S: CompactionSession> CompactionOperation<S> {
    /// Register only for providers that do not manage their own context.
    pub fn new(
        compactor: Arc<dyn SessionCompactor<S>>,
        estimator: Arc<dyn TokenEstimator>,
        context_limit: usize,
        threshold: f64,
    ) -> Self {
        Self {
            compactor,
            estimator,
            context_limit,
            threshold,
        }
    }

    fn over_threshold(&self, tokens: usize) -> bool {
        if !(0.0..1.0).contains(&self.threshold) || self.threshold == 0.0 || self.context_limit == 0
        {
            return false;
        }
        (tokens as f64 / self.context_limit as f64) > self.threshold
    }

    async fn context_tokens(&self, session: &S, conversation: &Conversation) -> Result<i32> {
        match session.context_tokens() {
            Some(tokens) => Ok(tokens.saturating_add(
                unreported_tool_tokens(conversation, self.estimator.as_ref()).await?,
            )),
            None => count_context_tokens(conversation.messages(), self.estimator.as_ref()).await,
        }
    }

    fn command_error<E: CompactionEffect>(
        conversation: &Conversation,
        message: String,
        emit: &Emitter,
    ) -> Result<OperationResult<E>> {
        let command = messages_since_kickoff(conversation)?
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("compact command conversation has no kickoff message"))?;
        let message_id = command
            .id
            .clone()
            .ok_or_else(|| anyhow!("Persisted slash command message has no id"))?;
        let command = command.with_visibility(true, false);
        let response = Message::assistant()
            .with_text(message)
            .with_visibility(true, false);
        emit.message(command);
        let response = emit.message(response);
        yielded_with([
            ConversationEffect::SetMessageVisibility {
                message_id,
                user_visible: true,
                agent_visible: false,
            }
            .into(),
            response.into(),
        ])
    }

    fn clear<E: CompactionEffect>(
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<E>> {
        let command = messages_since_kickoff(conversation)?
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("clear command conversation has no kickoff message"))?
            .with_visibility(true, false);
        let response = Message::assistant()
            .with_text("Conversation cleared")
            .with_visibility(true, false);
        let command = emit.message(command);
        let response = emit.message(response);
        yielded_with([
            Conversation::default().into(),
            command.into(),
            response.into(),
        ])
    }
}

#[async_trait]
impl<S: CompactionSession, E: CompactionEffect> Operation<S, E> for CompactionOperation<S> {
    fn name(&self) -> &'static str {
        "compaction"
    }

    async fn run_command(
        &self,
        command: &SlashCommand<'_>,
        session: &S,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<E>> {
        match command.command {
            "clear" => return Self::clear(conversation, emit),
            "compact" => {}
            _ => return not_applicable(),
        }

        let result = match self.compactor.compact(session, conversation, true).await {
            Ok(result) => result,
            Err(error) => return Self::command_error(conversation, error.to_string(), emit),
        };

        let command = messages_since_kickoff(conversation)?
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("compact command conversation has no kickoff message"))?
            .with_visibility(true, false);
        let response = Message::assistant()
            .with_text("Compaction complete")
            .with_visibility(true, false);
        emit.message(command);
        let response = emit.message(response);
        yielded_with([E::compact_conversation(result), response.into()])
    }

    async fn run(
        &self,
        session: &S,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<E>> {
        let messages = messages_since_kickoff(conversation)?;
        let reactive_context_error = matches!(
            trailing_error(conversation),
            Some(MessageErrorKind::ContextLengthExceeded)
        );

        if reactive_context_error {
            let context_errors = messages
                .iter()
                .filter(|message| {
                    message.error_kind() == Some(MessageErrorKind::ContextLengthExceeded)
                        && !message.is_agent_visible()
                })
                .count();
            if context_errors > MAX_CONTEXT_ERROR_COMPACTIONS {
                return not_applicable();
            }
        } else {
            // Compact only ahead of an inference; an assistant tail ends the turn.
            if last_effective_role(messages)? == EffectiveRole::Assistant {
                return not_applicable();
            }
            let tokens = self.context_tokens(session, conversation).await?;
            if tokens <= 0 || !self.over_threshold(tokens as usize) {
                return not_applicable();
            }
        }

        let conversation_with_hidden_error;
        let conversation = if reactive_context_error {
            let mut messages = conversation.messages().to_vec();
            let Some(last) = messages.last_mut() else {
                return not_applicable();
            };
            last.metadata.agent_visible = false;
            conversation_with_hidden_error = Conversation::new_unvalidated(messages);
            &conversation_with_hidden_error
        } else {
            conversation
        };

        let threshold_k_tokens = (self.context_limit as f64 * self.threshold / 1000.0) as usize;
        emit.message(Message::assistant().with_system_notification(
            SystemNotificationType::InlineMessage,
            format!(
                "Exceeded auto-compact threshold of {threshold_k_tokens}k tokens. \
                     Performing auto-compaction..."
            ),
        ));
        emit.message(Message::assistant().with_system_notification(
            SystemNotificationType::ThinkingMessage,
            COMPACTION_THINKING_TEXT,
        ));

        match self.compactor.compact(session, conversation, false).await {
            Ok(result) => {
                emit.message(Message::assistant().with_system_notification(
                    SystemNotificationType::InlineMessage,
                    "Compaction complete",
                ));
                applied([E::compact_conversation(result)])
            }
            Err(e) => {
                let message = emit.message(compaction_failure_message(&e));
                yielded_with([message.into()])
            }
        }
    }
}

fn compaction_failure_message(e: &anyhow::Error) -> Message {
    let provider_error = e.downcast_ref::<ProviderError>();
    if let Some(err @ ProviderError::CreditsExhausted { .. }) = provider_error {
        return Message::from_provider_error(err);
    }
    Message::assistant().with_error(
        provider_error.map_or(MessageErrorKind::Other, MessageErrorKind::from),
        format!(
            "Ran into this error trying to compact: {e}.\n\n\
             Please try again or create a new session"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_failure_keeps_the_top_up_url() {
        let error = anyhow::Error::from(ProviderError::CreditsExhausted {
            details: "no credits".to_string(),
            top_up_url: Some("https://example.com/billing".to_string()),
        });
        let message = compaction_failure_message(&error);
        let Some(goose_provider_types::conversation::message::MessageContent::SystemNotification(
            notification,
        )) = message.content.first()
        else {
            panic!("expected a credits notification, got {message:?}");
        };
        assert_eq!(
            notification.notification_type,
            SystemNotificationType::CreditsExhausted
        );
        assert_eq!(
            notification.data.as_ref().unwrap()["top_up_url"],
            "https://example.com/billing"
        );
    }
}
