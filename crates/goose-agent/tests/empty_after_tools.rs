use async_trait::async_trait;
use goose_agent::{
    inference::{is_empty_response_marker, InferenceEffect, InferenceRunner},
    operation::{Emitter, Inference, InferenceInput, OperationResult},
};
use goose_provider_types::{
    base::{stream_from_single_message, MessageStream, Provider},
    conversation::{
        message::{Message, MessageContent},
        token_usage::{ProviderUsage, Usage},
        Conversation,
    },
    errors::ProviderError,
    model::ModelConfig,
};
use rmcp::model::{CallToolResult, ContentBlock, Tool};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Session(Conversation);
impl goose_agent::machine::MachineSession for Session {
    fn id(&self) -> &str {
        "test"
    }
    fn conversation(&self) -> Option<&Conversation> {
        Some(&self.0)
    }
}

struct Model {
    calls: AtomicUsize,
    recover: bool,
    thinking_only: bool,
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
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(stream_from_single_message(
            if self.recover && call > 0 {
                Message::assistant().with_text("done")
            } else if self.thinking_only {
                Message::assistant().with_content(MessageContent::thinking(
                    "I should read the file.",
                    "signature",
                ))
            } else {
                Message::assistant()
            },
            ProviderUsage::new("fixture".into(), Usage::default()),
        ))
    }
}
enum Effect {
    Message(Box<Message>),
    Usage,
}
impl From<Message> for Effect {
    fn from(value: Message) -> Self {
        Self::Message(Box::new(value))
    }
}
impl InferenceEffect for Effect {
    fn record_usage(_: ProviderUsage) -> Self {
        Self::Usage
    }
}

#[tokio::test]
async fn hosts_can_retry_empty_inference_after_tools_without_replaying_the_tool() {
    for (allow_empty, recover, thinking_only, expected_calls, marker) in [
        (true, true, false, 1, false),
        (true, true, true, 1, false),
        (false, true, false, 2, false),
        (false, true, true, 2, false),
        (false, false, false, 4, true),
        (false, false, true, 4, true),
    ] {
        let model = Arc::new(Model {
            calls: AtomicUsize::new(0),
            recover,
            thinking_only,
        });
        let runner =
            InferenceRunner::<Session, Effect>::new(model.clone(), ModelConfig::new("fixture"))
                .with_empty_response_after_tools(allow_empty);
        let conversation = Conversation::new_unvalidated(vec![
            Message::user().with_text("build an app"),
            Message::user().with_tool_response(
                "call",
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    "file read",
                )])),
            ),
        ]);
        let (send, _receive) = tokio::sync::mpsc::unbounded_channel();
        let emit = Emitter::new(send, Default::default());
        let result = runner
            .infer(
                &Session(conversation.clone()),
                &conversation,
                InferenceInput::default(),
                &emit,
            )
            .await
            .unwrap();
        assert_eq!(model.calls.load(Ordering::SeqCst), expected_calls);
        let OperationResult::Applied(result) = result else {
            panic!("inference did not run")
        };
        assert_eq!(result.effects.iter().any(|effect| matches!(effect,Effect::Message(message) if is_empty_response_marker(message))),marker);
    }
}
