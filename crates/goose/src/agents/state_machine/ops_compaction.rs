//! Goose's model selection and usage persistence for GDK compaction.
use super::{
    ops_llm::{chat_span, record_chat_usage},
    GooseEffect,
};
use crate::{
    context_mgmt::{compact_messages, GooseTokenEstimator},
    session::Session,
};
use anyhow::Result;
use async_trait::async_trait;
use goose_agent::compaction::{CompactionEffect, CompactionSession, SessionCompactor};
use goose_context_management::conversation::CompactionResult;
use goose_providers::{base::Provider, conversation::Conversation, model::ModelConfig};
use std::sync::Arc;
use tracing_futures::Instrument;

pub type CompactionOperation = goose_agent::compaction::CompactionOperation<Session>;
#[cfg(test)]
pub(super) use goose_agent::compaction::MAX_CONTEXT_ERROR_COMPACTIONS;

impl CompactionSession for Session {
    fn context_tokens(&self) -> Option<i32> {
        self.usage.total_tokens
    }
}
impl CompactionEffect for GooseEffect {
    fn compact_conversation(result: CompactionResult) -> Self {
        Self::CompactConversation {
            conversation: result.conversation,
            usage: Some(result.usage),
        }
    }
}
struct GooseCompactor {
    provider: Arc<dyn Provider>,
    model_config: ModelConfig,
}
#[async_trait]
impl SessionCompactor<Session> for GooseCompactor {
    async fn compact(
        &self,
        session: &Session,
        conversation: &Conversation,
        manual: bool,
    ) -> Result<CompactionResult> {
        let span = chat_span(
            self.provider.as_ref(),
            &self.model_config,
            &session.id,
            "compaction",
        );
        let result = compact_messages(
            self.provider.as_ref(),
            &self.model_config,
            &session.id,
            conversation,
            manual,
        )
        .instrument(span.clone())
        .await;
        match &result {
            Ok(result) => record_chat_usage(&span, &result.usage),
            Err(_) => {
                span.record("error.type", "compaction_error");
            }
        }
        result
    }
}
pub(crate) fn compaction_operation(
    provider: Arc<dyn Provider>,
    model_config: ModelConfig,
    context_limit: usize,
    threshold: f64,
) -> CompactionOperation {
    CompactionOperation::new(
        Arc::new(GooseCompactor {
            provider,
            model_config,
        }),
        Arc::new(GooseTokenEstimator::default()),
        context_limit,
        threshold,
    )
}
