use anyhow::Result;
use async_trait::async_trait;
use goose_context_management::{
    conversation::{compact_messages as compact, CompactionResult},
    CompactionModel, Templates, TokenEstimator,
};
use goose_provider_types::{
    base::{stream_from_single_message, MessageStream, Provider},
    conversation::{
        message::{Message, MessageContent, MessageMetadata},
        token_usage::{ProviderUsage, Usage},
        Conversation,
    },
    errors::ProviderError,
    model::ModelConfig,
};
use rmcp::model::{Annotations, CallToolRequestParams, ContentBlock, Role, TextContent, Tool};

struct Estimator;
#[async_trait]
impl TokenEstimator for Estimator {
    async fn count_chat_tokens(&self, system: &str, messages: &[Message]) -> usize {
        system.len()
            + messages
                .iter()
                .map(|message| message.as_concat_text().len() + 4)
                .sum::<usize>()
    }
    async fn count_text_tokens(&self, text: &str) -> usize {
        text.len()
    }
}
#[async_trait]
impl CompactionModel for MockProvider {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
    ) -> Result<(Message, ProviderUsage), ProviderError> {
        Provider::complete(self, &self.config, system, messages, &[]).await
    }
}
async fn compact_messages(
    model: &MockProvider,
    _: &ModelConfig,
    _: &str,
    conversation: &Conversation,
    manual: bool,
) -> Result<CompactionResult> {
    compact(
        model,
        Some(&Estimator),
        &Templates::default(),
        conversation,
        manual,
    )
    .await
}
struct MockProvider {
    message: Message,
    config: ModelConfig,
    max_tool_responses: Option<usize>,
    captured_system: std::sync::Mutex<Option<String>>,
}

impl MockProvider {
    fn new(message: Message, context_limit: usize) -> Self {
        Self {
            message,
            config: ModelConfig {
                model_name: "test".to_string(),
                context_limit: Some(context_limit),
                temperature: None,
                max_tokens: None,
                toolshim: false,
                toolshim_model: None,
                request_params: None,
                reasoning: None,
                supports_vision: None,
                request_headers: None,
            },
            max_tool_responses: None,
            captured_system: std::sync::Mutex::new(None),
        }
    }

    fn with_max_tool_responses(mut self, max: usize) -> Self {
        self.max_tool_responses = Some(max);
        self
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn get_name(&self) -> &str {
        "mock"
    }

    async fn stream(
        &self,
        _model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        _tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        *self.captured_system.lock().unwrap() = Some(system.to_string());
        // If max_tool_responses is set, fail if we have too many
        if let Some(max) = self.max_tool_responses {
            let tool_response_count = messages
                .iter()
                .filter(|m| {
                    m.content
                        .iter()
                        .any(|c| matches!(c, MessageContent::ToolResponse(_)))
                })
                .count();

            if tool_response_count > max {
                return Err(ProviderError::ContextLengthExceeded(format!(
                    "Too many tool responses: {} > {}",
                    tool_response_count, max
                )));
            }
        }

        let message = self.message.clone();
        let usage = ProviderUsage::new("mock-model".to_string(), Usage::default());
        Ok(stream_from_single_message(message, usage))
    }

    async fn get_context_limit(&self, _model: &str, override_limit: Option<usize>) -> usize {
        override_limit.unwrap_or_else(|| self.config.context_limit())
    }
}

#[tokio::test]
async fn test_keeps_tool_request() {
    let response_message = Message::assistant().with_text("<mock summary>");
    let provider = MockProvider::new(response_message, 1);
    let basic_conversation = vec![
        Message::user().with_text("read hello.txt"),
        Message::assistant()
            .with_tool_request("tool_0", Ok(CallToolRequestParams::new("read_file"))),
        Message::user().with_tool_response(
            "tool_0",
            Ok(rmcp::model::CallToolResult::success(vec![
                ContentBlock::text("hello, world"),
            ])),
        ),
    ];

    let conversation = Conversation::new_unvalidated(basic_conversation);
    let model_config = provider.config.clone();
    let compaction = compact_messages(
        &provider,
        &model_config,
        "test-session-id",
        &conversation,
        false,
    )
    .await
    .unwrap();

    let agent_conversation = compaction.conversation.agent_visible_messages();

    let _ = Conversation::new(agent_conversation)
        .expect("compaction should produce a valid conversation");
}

#[tokio::test]
async fn test_structured_summary_is_rendered() {
    let structured_response = r#"<analysis>User asked to fix a bug; I patched parser.rs.</analysis>
```json
{
  "user_intent": ["Fix the parser bug"],
  "files": [{"path": "src/parser.rs", "summary": "Fixed off-by-one"}],
  "pending_tasks": ["Add a regression test"],
  "current_work": "Writing the regression test"
}
```"#;
    let provider = MockProvider::new(Message::assistant().with_text(structured_response), 100_000);
    let conversation = Conversation::new_unvalidated(vec![
        Message::user().with_text("fix the parser bug"),
        Message::assistant().with_text("Looking into it"),
    ]);

    let model_config = provider.config.clone();
    let compaction = compact_messages(
        &provider,
        &model_config,
        "test-session-id",
        &conversation,
        true,
    )
    .await
    .unwrap();

    let summary_text = compaction.conversation.agent_visible_messages()[0].as_concat_text();
    assert!(summary_text.contains("# Conversation Summary"));
    assert!(summary_text.contains("## User Intent"));
    assert!(summary_text.contains("- Fix the parser bug"));
    assert!(summary_text.contains("### src/parser.rs"));
    assert!(
        !summary_text.contains("```json"),
        "raw JSON should be replaced"
    );
    assert!(
        !summary_text.contains("<analysis>"),
        "analysis scratchpad should be dropped"
    );
    assert!(compaction.retained_context_tokens > 0);
    assert!(
        compaction.usage.usage.output_tokens.is_some(),
        "billable output tokens must survive the rewrite"
    );
}

#[tokio::test]
async fn retained_context_counts_preserved_user_message() {
    async fn retained(final_user_text: &str) -> i32 {
        let provider = MockProvider::new(Message::assistant().with_text("<mock summary>"), 100_000);
        let conversation = Conversation::new_unvalidated(vec![
            Message::user().with_text("start"),
            Message::assistant().with_text("ok"),
            Message::user().with_text(final_user_text),
        ]);
        let model_config = provider.config.clone();
        compact_messages(
            &provider,
            &model_config,
            "test-session-id",
            &conversation,
            false,
        )
        .await
        .unwrap()
        .retained_context_tokens
    }

    let short = retained("continue").await;
    let long = retained(&"long preserved user message ".repeat(200)).await;
    assert!(
        long > short,
        "the preserved user message must be part of the retained context ({short} vs {long})"
    );
}

#[tokio::test]
async fn preserved_user_message_keeps_audience_projection_after_compaction() {
    let annotated_text = |text: &str, audience| {
        MessageContent::Text(
            TextContent::new(text).with_annotations(Annotations::default().with_audience(audience)),
        )
    };
    let current_request = Message::user()
        .with_text("visible current request")
        .with_content(annotated_text("user-only secret", vec![Role::User]))
        .with_content(annotated_text(
            "assistant-only preprompt",
            vec![Role::Assistant],
        ));
    let conversation = Conversation::new_unvalidated([
        Message::user().with_text("earlier request"),
        Message::assistant().with_text("earlier response"),
        current_request,
    ]);
    let provider = MockProvider::new(Message::assistant().with_text("summary"), 1000);

    let compacted = compact_messages(
        &provider,
        &provider.config,
        "test-session-id",
        &conversation,
        false,
    )
    .await
    .unwrap()
    .conversation;

    let preserved_copies = compacted
        .messages()
        .iter()
        .filter(|message| message.as_concat_text().contains("visible current request"))
        .collect::<Vec<_>>();
    assert_eq!(preserved_copies.len(), 2);
    let archived = preserved_copies
        .iter()
        .find(|message| message.is_user_visible())
        .unwrap();
    assert!(!archived.is_agent_visible());
    assert!(archived.as_concat_text().contains("user-only secret"));
    let replay = preserved_copies
        .iter()
        .find(|message| message.is_agent_visible())
        .unwrap();
    assert!(!replay.is_user_visible());
    assert!(replay.as_concat_text().contains("assistant-only preprompt"));
    assert!(!replay.as_concat_text().contains("user-only secret"));

    let agent_text = compacted
        .agent_visible_messages()
        .iter()
        .map(Message::as_concat_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(agent_text.contains("visible current request"));
    assert!(agent_text.contains("assistant-only preprompt"));
    assert!(!agent_text.contains("user-only secret"));

    let user_text = compacted
        .user_visible_messages()
        .iter()
        .map(Message::as_concat_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(user_text.contains("user-only secret"));
    assert!(!user_text.contains("assistant-only preprompt"));
}

#[tokio::test]
async fn preserved_user_message_skips_turn_context_events() {
    let conversation = Conversation::new_unvalidated([
        Message::user().with_text("earlier request"),
        Message::assistant().with_text("earlier response"),
        Message::user().with_text("the real current request"),
        Message::user()
            .with_text("<turn-context>frozen block</turn-context>")
            .with_metadata(MessageMetadata::agent_only().with_turn_context()),
    ]);
    let provider = MockProvider::new(Message::assistant().with_text("summary"), 1000);

    let compacted = compact_messages(
        &provider,
        &provider.config,
        "test-session-id",
        &conversation,
        false,
    )
    .await
    .unwrap()
    .conversation;

    let preserved: Vec<_> = compacted
        .messages()
        .iter()
        .filter(|message| message.is_agent_visible() && message.role == Role::User)
        .collect();
    let turn_context_events: Vec<_> = preserved
        .iter()
        .filter(|message| message.is_turn_context())
        .collect();
    let [carried_block] = turn_context_events.as_slice() else {
        panic!("expected exactly one carried turn-context event");
    };
    assert!(
        carried_block.as_concat_text().contains("frozen block"),
        "the turn's context event must be carried forward for the mid-turn retry"
    );
    let last = compacted.messages().last().unwrap();
    assert!(
        last.is_turn_context() && last.is_agent_visible(),
        "the carried event must trail the preserved user message"
    );
    let user_prompt = preserved[preserved.len() - 2];
    assert!(
        user_prompt
            .as_concat_text()
            .contains("the real current request"),
        "the user's prompt must survive compaction verbatim"
    );
    assert!(!user_prompt.is_turn_context());

    let continuation = compacted
        .messages()
        .iter()
        .find(|message| message.role == Role::Assistant && message.is_agent_visible())
        .unwrap()
        .as_concat_text();
    assert!(
        continuation
            .contains(include_str!("../src/prompts/conversation_continuation_text.md").trim_end()),
        "a trailing turn-context event must not demote the compaction to a tool-loop continuation"
    );
}

#[tokio::test]
async fn stale_turn_context_from_an_earlier_turn_is_not_carried() {
    let conversation = Conversation::new_unvalidated([
        Message::user().with_text("earlier request"),
        Message::user()
            .with_text("<turn-context>stale block</turn-context>")
            .with_metadata(MessageMetadata::agent_only().with_turn_context()),
        Message::assistant().with_text("earlier response"),
        Message::user().with_text("the new prompt"),
    ]);
    let provider = MockProvider::new(Message::assistant().with_text("summary"), 1000);

    let compacted = compact_messages(
        &provider,
        &provider.config,
        "test-session-id",
        &conversation,
        false,
    )
    .await
    .unwrap()
    .conversation;

    assert!(
        !compacted
            .messages()
            .iter()
            .any(|message| message.is_agent_visible() && message.is_turn_context()),
        "pre-turn compaction must not resurrect a previous turn's context event"
    );
    let last = compacted.messages().last().unwrap();
    assert_eq!(last.as_concat_text(), "the new prompt");
}

#[tokio::test]
async fn summarizer_input_excludes_turn_context_events() {
    let provider = MockProvider::new(Message::assistant().with_text("summary"), 1000);
    let turn_context = |text: &str| {
        Message::user()
            .with_text(text)
            .with_metadata(MessageMetadata::agent_only().with_turn_context())
    };
    let conversation = Conversation::new_unvalidated([
        Message::user().with_text("please refactor the parser"),
        turn_context("<turn-context>cwd /old/dir</turn-context>"),
        Message::assistant().with_text("working on it"),
        turn_context("<turn-context>cwd /new/dir</turn-context>"),
    ]);

    let compacted = compact_messages(
        &provider,
        &provider.config,
        "test-session-id",
        &conversation,
        false,
    )
    .await
    .unwrap()
    .conversation;

    let system = provider.captured_system.lock().unwrap().clone().unwrap();
    assert!(system.contains("please refactor the parser"));
    assert!(
        !system.contains("/old/dir") && !system.contains("/new/dir"),
        "turn-context events must not reach the summarizer as dialogue"
    );

    let carried = compacted.messages().last().unwrap();
    assert!(
        carried.is_turn_context() && carried.as_concat_text().contains("/new/dir"),
        "the newest turn-context event must still be carried forward"
    );
}

#[tokio::test]
async fn test_progressive_removal_on_context_exceeded() {
    let response_message = Message::assistant().with_text("<mock summary>");
    // Set max to 2 tool responses - will trigger progressive removal
    let provider = MockProvider::new(response_message, 1000).with_max_tool_responses(2);

    // Create a conversation with many tool responses
    let mut messages = vec![Message::user().with_text("start")];
    for i in 0..10 {
        messages.push(Message::assistant().with_tool_request(
            format!("tool_{}", i),
            Ok(CallToolRequestParams::new("read_file")),
        ));
        messages.push(Message::user().with_tool_response(
            format!("tool_{}", i),
            Ok(rmcp::model::CallToolResult::success(vec![
                ContentBlock::text(format!("response{}", i)),
            ])),
        ));
    }

    let conversation = Conversation::new_unvalidated(messages);
    let model_config = provider.config.clone();
    let result = compact_messages(
        &provider,
        &model_config,
        "test-session-id",
        &conversation,
        false,
    )
    .await;

    assert!(
        result.is_ok(),
        "Should succeed with progressive removal: {:?}",
        result.err()
    );
}
