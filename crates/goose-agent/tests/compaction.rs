#![cfg(all(feature = "context-management", not(target_arch = "wasm32")))]

use async_trait::async_trait;
use goose_agent::{
    compaction::{CompactionEffect, CompactionOperation, CompactionSession, ModelCompactor},
    inference::{
        IdentityInferenceRequestPreparer, InferenceEffect, InferenceRequestPreparer,
        InferenceRunner, PreparedInferenceRequest,
    },
    machine::MachineSession,
    operation::{
        assistant_turn_count, messages_since_kickoff, ConversationEffect, Emitter, Inference,
        InferenceInput, OperationResult,
    },
};
use goose_context_management::{
    conversation::CompactionResult, CompactionModel, Templates, TokenEstimator,
};
use goose_provider_types::{
    base::{stream_from_single_message, MessageStream, Provider},
    conversation::{
        message::Message,
        token_usage::{ProviderUsage, Usage},
        Conversation,
    },
    errors::ProviderError,
    model::ModelConfig,
};
use rmcp::{
    model::{CallToolRequestParams, CallToolResult, ContentBlock, Tool},
    object,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Session(Conversation);
impl MachineSession for Session {
    fn id(&self) -> &str {
        "gdk"
    }
    fn conversation(&self) -> Option<&Conversation> {
        Some(&self.0)
    }
}
impl CompactionSession for Session {}

enum Effect {
    Compaction(Box<CompactionResult>),
    Other,
}
impl From<Message> for Effect {
    fn from(_: Message) -> Self {
        Self::Other
    }
}
impl From<Conversation> for Effect {
    fn from(_: Conversation) -> Self {
        Self::Other
    }
}
impl From<ConversationEffect> for Effect {
    fn from(_: ConversationEffect) -> Self {
        Self::Other
    }
}
impl CompactionEffect for Effect {
    fn compact_conversation(result: CompactionResult) -> Self {
        Self::Compaction(Box::new(result))
    }
}
impl InferenceEffect for Effect {
    fn record_usage(_: ProviderUsage) -> Self {
        Self::Other
    }
}

#[derive(Default)]
struct Model {
    calls: AtomicUsize,
    summaries: AtomicUsize,
}
#[async_trait]
impl Provider for Model {
    fn get_name(&self) -> &str {
        "fixture"
    }
    async fn stream(
        &self,
        _: &ModelConfig,
        _: &str,
        _: &[Message],
        _: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(stream_from_single_message(
            Message::assistant().with_text("done"),
            ProviderUsage::new("fixture".into(), Usage::default()),
        ))
    }
}
#[async_trait]
impl CompactionModel for Model {
    async fn complete(
        &self,
        _: &str,
        _: &[Message],
    ) -> Result<(Message, ProviderUsage), ProviderError> {
        self.summaries.fetch_add(1, Ordering::SeqCst);
        Ok((
            Message::assistant().with_text("summary"),
            ProviderUsage::new("fixture".into(), Usage::default()),
        ))
    }
}
struct Estimator;
#[async_trait]
impl TokenEstimator for Estimator {
    async fn count_text_tokens(&self, text: &str) -> usize {
        text.len()
    }
    async fn count_chat_tokens(&self, system: &str, messages: &[Message]) -> usize {
        system.len()
            + messages
                .iter()
                .map(|message| message.as_concat_text().len())
                .sum::<usize>()
    }
}

struct RuntimeInstructions;
#[async_trait]
impl InferenceRequestPreparer<Session> for RuntimeInstructions {
    async fn prepare(
        &self,
        session: &Session,
        conversation: &Conversation,
        input: InferenceInput,
    ) -> anyhow::Result<PreparedInferenceRequest> {
        let mut prepared = IdentityInferenceRequestPreparer
            .prepare(session, conversation, input)
            .await?;
        prepared
            .additional_messages
            .push(Message::user().with_text("runtime instructions"));
        Ok(prepared)
    }
}

fn request(large_system: bool) -> InferenceInput {
    if large_system {
        InferenceInput {
            prompt_parts: vec![("system".into(), "instructions ".repeat(100))],
            ..Default::default()
        }
    } else {
        InferenceInput {
            tools: vec![Tool::new(
                "tool",
                "description ".repeat(100),
                object!({"type": "object", "properties": {}}),
            )],
            ..Default::default()
        }
    }
}

#[tokio::test]
async fn prepared_system_and_tools_trigger_compaction_without_spending_turns_or_looping() {
    for large_system in [false, true] {
        let model = Arc::new(Model::default());
        let estimator = Arc::new(Estimator);
        let compactor = Arc::new(ModelCompactor {
            model: model.clone(),
            estimator: estimator.clone(),
            templates: Templates::default(),
        });
        let guard = Arc::new(CompactionOperation::new(compactor, estimator, 500, 0.8));
        let runner =
            InferenceRunner::<Session, Effect>::new(model.clone(), ModelConfig::new("fixture"))
                .with_request_guard(guard)
                .with_request_preparer(Arc::new(RuntimeInstructions));
        let mut session = Session(Conversation::new_unvalidated([
            Message::user().with_text("fix parser"),
            Message::assistant().with_tool_request("read", Ok(CallToolRequestParams::new("read"))),
            Message::user().with_tool_response(
                "read",
                Ok(CallToolResult::success(vec![ContentBlock::text("source")])),
            ),
        ]));
        let turns_before = assistant_turn_count(messages_since_kickoff(&session.0).unwrap());
        let original = serde_json::to_string(&session.0).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let emit = Emitter::new(tx, CancellationToken::new());
        let result = runner
            .infer(&session, &session.0, request(large_system), &emit)
            .await
            .unwrap();
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        assert_eq!(serde_json::to_string(&session.0).unwrap(), original);
        let OperationResult::Applied(step) = result else {
            panic!("expected compaction");
        };
        let compacted = step
            .effects
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Compaction(result) => Some(result.conversation),
                _ => None,
            })
            .unwrap();
        session.0 = serde_json::from_str(&serde_json::to_string(&compacted).unwrap()).unwrap();
        assert_eq!(
            session
                .0
                .agent_visible_messages()
                .last()
                .unwrap()
                .as_concat_text(),
            "fix parser"
        );
        assert_eq!(
            assistant_turn_count(messages_since_kickoff(&session.0).unwrap()),
            turns_before
        );
        runner
            .infer(&session, &session.0, request(large_system), &emit)
            .await
            .unwrap();
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        assert_eq!(model.summaries.load(Ordering::SeqCst), 1);
    }
}
