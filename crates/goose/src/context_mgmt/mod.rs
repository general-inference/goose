pub use goose_context_management::structured;

use crate::conversation::message::MessageMetadata;
use crate::conversation::message::{Message, MessageContent};
use crate::conversation::Conversation;
use crate::providers::base::Provider;
#[cfg(test)]
use crate::providers::base::{stream_from_single_message, MessageStream};
use crate::{config::Config, token_counter::create_token_counter};
use anyhow::Result;
use goose_providers::conversation::token_usage::ProviderUsage;
use goose_providers::errors::ProviderError;
use goose_providers::model::ModelConfig;
use indoc::indoc;
use rmcp::model::Role;
#[cfg(test)]
use rmcp::model::{Annotations, ContentBlock, TextContent};
use tracing::log::warn;

pub use goose_context_management::DEFAULT_COMPACTION_THRESHOLD;

/// Starts compaction with headroom below OpenAI's long-context price tier
/// (above 272k input tokens). This is not a maximum request size.
pub const DEFAULT_AUTO_COMPACT_TOKEN_LIMIT: usize = 225_000;

pub(crate) const TOOLCALL_SUMMARIZATION_BATCH_SIZE: usize = 10;

/// The configured threshold, lowered so compaction starts by the token limit.
pub fn auto_compact_threshold(context_limit: usize) -> f64 {
    let config = Config::global();
    let threshold = config
        .get_param::<f64>("GOOSE_AUTO_COMPACT_THRESHOLD")
        .unwrap_or(DEFAULT_COMPACTION_THRESHOLD);
    if threshold <= 0.0 || threshold >= 1.0 || context_limit == 0 {
        return threshold;
    }
    let token_limit = config
        .get_param::<usize>("GOOSE_AUTO_COMPACT_TOKEN_LIMIT")
        .unwrap_or(DEFAULT_AUTO_COMPACT_TOKEN_LIMIT);
    threshold.min(token_limit as f64 / context_limit as f64)
}

pub(crate) fn tool_pair_summarization_enabled() -> bool {
    Config::global()
        .get_param::<bool>("GOOSE_TOOL_PAIR_SUMMARIZATION")
        .unwrap_or(false)
}

pub use goose_context_management::conversation::CompactionResult;

pub async fn compact_messages(
    provider: &dyn Provider,
    model_config: &ModelConfig,
    session_id: &str,
    conversation: &Conversation,
    manual_compact: bool,
) -> Result<CompactionResult> {
    let model = GooseCompactionModel {
        provider,
        model_config,
        session_id,
    };
    goose_context_management::conversation::compact_messages(
        &model,
        Some(&GooseTokenEstimator),
        &compaction_templates()?,
        conversation,
        manual_compact,
    )
    .await
}

/// Estimate the tokens of the agent-visible messages, counted the same way
/// as the fallback estimation in `check_if_compaction_needed`.
pub(crate) async fn count_context_tokens(messages: &[Message]) -> Result<i32> {
    let counter = create_token_counter()
        .await
        .map_err(|error| anyhow::anyhow!("Failed to create token counter: {error}"))?;
    let total: usize = messages
        .iter()
        .filter(|message| message.is_agent_visible())
        .map(|message| counter.count_chat_tokens("", std::slice::from_ref(message), &[]))
        .sum();
    Ok(total.try_into()?)
}

/// Check if messages exceed the auto-compaction threshold
pub async fn check_if_compaction_needed(
    provider: &dyn Provider,
    conversation: &Conversation,
    threshold_override: Option<f64>,
    session: &crate::session::Session,
) -> Result<bool> {
    if provider.manages_own_context() {
        return Ok(false);
    }

    let messages = conversation.messages();
    let config = Config::global();
    let threshold = threshold_override.unwrap_or_else(|| {
        config
            .get_param::<f64>("GOOSE_AUTO_COMPACT_THRESHOLD")
            .unwrap_or(DEFAULT_COMPACTION_THRESHOLD)
    });

    let model_config = session
        .model_config
        .clone()
        .unwrap_or_else(|| ModelConfig::new("unknown"));
    let context_limit =
        crate::context_limit::get_context_limit(provider, &model_config.model_name).await?;

    let (current_tokens, _token_source) = match session.usage.total_tokens {
        Some(tokens) => (tokens as usize, "session metadata"),
        None => {
            let token_counter = create_token_counter()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create token counter: {}", e))?;

            let token_counts: Vec<_> = messages
                .iter()
                .filter(|m| m.is_agent_visible())
                .map(|msg| token_counter.count_chat_tokens("", std::slice::from_ref(msg), &[]))
                .collect();

            (token_counts.iter().sum(), "estimated")
        }
    };

    let usage_ratio = current_tokens as f64 / context_limit as f64;

    let needs_compaction = if threshold <= 0.0 || threshold >= 1.0 {
        false // Auto-compact is disabled.
    } else {
        usage_ratio > threshold
    };
    Ok(needs_compaction)
}

struct GooseCompactionModel<'a> {
    provider: &'a dyn Provider,
    model_config: &'a ModelConfig,
    session_id: &'a str,
}

#[async_trait::async_trait]
impl goose_context_management::CompactionModel for GooseCompactionModel<'_> {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
    ) -> Result<(Message, ProviderUsage), ProviderError> {
        crate::model_config::complete_one_shot(
            self.provider,
            self.model_config,
            self.session_id,
            system,
            messages,
            &[],
        )
        .await
    }
}

struct GooseTokenEstimator;

#[async_trait::async_trait]
impl goose_context_management::TokenEstimator for GooseTokenEstimator {
    async fn count_chat_tokens(&self, system: &str, messages: &[Message]) -> usize {
        match create_token_counter().await {
            Ok(counter) => counter.count_chat_tokens(system, messages, &[]),
            Err(error) => {
                warn!("Failed to create token counter: {error}");
                0
            }
        }
    }

    async fn count_text_tokens(&self, text: &str) -> usize {
        match create_token_counter().await {
            Ok(counter) => counter.count_tokens(text),
            Err(error) => {
                warn!("Failed to create token counter: {error}");
                0
            }
        }
    }
}

fn compaction_templates() -> Result<goose_context_management::Templates> {
    Ok(goose_context_management::Templates {
        compaction: crate::prompt_template::template_source("compaction.md")?,
        summary: crate::prompt_template::template_source("compaction_summary.md")?,
    })
}

pub use goose_context_management::format_message_for_compacting;

pub fn compute_tool_call_cutoff(context_limit: usize, compaction_threshold: f64) -> usize {
    let threshold = if compaction_threshold > 0.0 && compaction_threshold <= 1.0 {
        compaction_threshold
    } else {
        DEFAULT_COMPACTION_THRESHOLD
    };
    let effective_limit = (context_limit as f64 * threshold) as usize;
    (3 * effective_limit / 20_000).clamp(10, 500)
}

pub fn tool_ids_to_summarize(
    conversation: &Conversation,
    cutoff: usize,
    protect_last_n: usize,
) -> Vec<String> {
    let messages = conversation.messages();

    let mut tool_call_ids: Vec<String> = Vec::new();

    for msg in messages.iter() {
        if !msg.is_agent_visible() {
            continue;
        }

        for content in &msg.content {
            if let MessageContent::ToolRequest(req) = content {
                tool_call_ids.push(req.id.clone());
            }
        }
    }

    // Never summarize the last N tool calls (current turn)
    let eligible = tool_call_ids.len().saturating_sub(protect_last_n);
    if eligible <= cutoff + TOOLCALL_SUMMARIZATION_BATCH_SIZE {
        return Vec::new();
    }

    tool_call_ids
        .into_iter()
        .take(TOOLCALL_SUMMARIZATION_BATCH_SIZE)
        .collect()
}

fn agent_visible_tool_pair(conversation: &Conversation, tool_id: &str) -> Result<Vec<Message>> {
    let matching_messages = conversation
        .messages()
        .iter()
        .filter(|m| {
            m.content.iter().any(|c| match c {
                MessageContent::ToolRequest(req) => req.id == tool_id,
                MessageContent::ToolResponse(resp) => resp.id == tool_id,
                _ => false,
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    let matching_messages =
        Conversation::new_unvalidated(matching_messages).agent_visible_messages();

    let has_request = matching_messages.iter().any(|message| {
        message.content.iter().any(
            |content| matches!(content, MessageContent::ToolRequest(request) if request.id == tool_id),
        )
    });
    let has_response = matching_messages.iter().any(|message| {
        message.content.iter().any(
            |content| matches!(content, MessageContent::ToolResponse(response) if response.id == tool_id),
        )
    });
    if !has_request || !has_response {
        return Err(anyhow::anyhow!(
            "No agent-visible tool pair found for tool id: {}",
            tool_id
        ));
    }
    Ok(matching_messages)
}

pub async fn summarize_tool_call(
    provider: &dyn Provider,
    model_config: &ModelConfig,
    session_id: &str,
    conversation: &Conversation,
    tool_id: &str,
) -> Result<Message> {
    let matching_messages = agent_visible_tool_pair(conversation, tool_id)?;

    let formatted = matching_messages
        .iter()
        .map(format_message_for_compacting)
        .collect::<Vec<_>>()
        .join("\n");

    let user_message = Message::user().with_text(formatted);
    let summarization_request = vec![user_message];

    let system_prompt = indoc! {r#"
                Your task is to summarize a tool call & response pair to save tokens.

                Reply with a single message that describes what happened. Typically a tool call
                asks for something using a bunch of parameters and then the result is also some
                structured output. So the tool might ask to look up something on github and the
                reply might be a json document. So you could reply with something like:

                "A call to github was made to get the project status"

                if that is what it was.
            "#};

    let (mut response, _) = crate::model_config::complete_one_shot(
        provider,
        model_config,
        session_id,
        system_prompt,
        &summarization_request,
        &[],
    )
    .await?;

    response.role = Role::User;
    response.created = matching_messages.last().unwrap().created;
    response.metadata = MessageMetadata::agent_only();

    Ok(response.with_generated_id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use goose_providers::conversation::token_usage::Usage;
    use rmcp::model::{CallToolRequestParams, Tool};

    fn create_tool_pair(
        call_id: &str,
        response_id: &str,
        tool_name: &str,
        response_text: &str,
    ) -> Vec<Message> {
        vec![
            Message::assistant()
                .with_tool_request(
                    call_id,
                    Ok(CallToolRequestParams::new(tool_name.to_string())),
                )
                .with_id(call_id),
            Message::user()
                .with_tool_response(
                    call_id,
                    Ok(rmcp::model::CallToolResult::success(vec![
                        ContentBlock::text(response_text),
                    ])),
                )
                .with_id(response_id),
        ]
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
    async fn carried_turn_context_stays_last_after_persist_and_reload() {
        let provider = MockProvider::new(Message::assistant().with_text("summary"), 1000);
        let mut prompt = Message::user().with_text("the real current request");
        prompt.created -= 3600;
        let mut block = Message::user()
            .with_text("<turn-context>frozen block</turn-context>")
            .with_metadata(MessageMetadata::agent_only().with_turn_context());
        block.created -= 3600;
        let conversation = Conversation::new_unvalidated([prompt, block]);

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
        assert!(compacted.messages().last().unwrap().is_turn_context());

        let temp_dir = tempfile::tempdir().unwrap();
        let manager = crate::session::SessionManager::new(temp_dir.path().to_path_buf());
        let session = manager
            .create_session(
                std::path::PathBuf::from("/tmp/test"),
                "carry order".to_string(),
                crate::session::session_manager::SessionType::User,
                crate::config::GooseMode::default(),
            )
            .await
            .unwrap();
        manager
            .replace_conversation(&session.id, &compacted)
            .await
            .unwrap();

        let reloaded = manager
            .get_session(&session.id, true)
            .await
            .unwrap()
            .conversation
            .unwrap();
        assert_eq!(reloaded.messages().len(), compacted.messages().len());
        let last = reloaded.messages().last().unwrap();
        assert!(
            last.is_turn_context(),
            "the carried event must not resurface at its original timestamp on reload"
        );
    }

    #[tokio::test]
    async fn tool_pair_summary_projects_nested_audiences_before_provider_input() {
        let provider = MockProvider::new(Message::assistant().with_text("summary"), 1000);
        let conversation =
            Conversation::new_unvalidated([
                Message::assistant()
                    .with_tool_request("tool_0", Ok(CallToolRequestParams::new("read_file"))),
                Message::user().with_tool_response(
                    "tool_0",
                    Ok(rmcp::model::CallToolResult::success(vec![
                        ContentBlock::text("visible result"),
                        ContentBlock::Text(TextContent::new("user-only secret").with_annotations(
                            Annotations::default().with_audience(vec![Role::User]),
                        )),
                    ])),
                ),
            ]);

        let projected = agent_visible_tool_pair(&conversation, "tool_0").unwrap();
        let formatted = projected
            .iter()
            .map(format_message_for_compacting)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(formatted.contains("visible result"));
        assert!(!formatted.contains("user-only secret"));

        let user_only_conversation =
            Conversation::new_unvalidated([
                Message::assistant()
                    .with_tool_request("tool_1", Ok(CallToolRequestParams::new("read_file"))),
                Message::user().with_tool_response(
                    "tool_1",
                    Ok(rmcp::model::CallToolResult::success(vec![
                        ContentBlock::Text(TextContent::new("user-only secret").with_annotations(
                            Annotations::default().with_audience(vec![Role::User]),
                        )),
                    ])),
                ),
            ]);
        let user_only_formatted = agent_visible_tool_pair(&user_only_conversation, "tool_1")
            .unwrap()
            .iter()
            .map(format_message_for_compacting)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!user_only_formatted.contains("user-only secret"));

        summarize_tool_call(
            &provider,
            &provider.config,
            "test-session-id",
            &conversation,
            "tool_0",
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn tool_pair_summary_rejects_agent_hidden_response() {
        let provider = MockProvider::new(Message::assistant().with_text("summary"), 1000);
        let conversation = Conversation::new_unvalidated([
            Message::assistant()
                .with_tool_request("tool_0", Ok(CallToolRequestParams::new("read_file"))),
            Message::user()
                .with_tool_response(
                    "tool_0",
                    Ok(rmcp::model::CallToolResult::success(vec![
                        ContentBlock::text("user-only secret"),
                    ])),
                )
                .with_metadata(MessageMetadata::user_only()),
        ]);

        let error = summarize_tool_call(
            &provider,
            &provider.config,
            "test-session-id",
            &conversation,
            "tool_0",
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("No agent-visible tool pair"));
    }

    #[test]
    fn test_compute_tool_call_cutoff_scales_with_context() {
        // Default threshold (0.8)
        assert_eq!(compute_tool_call_cutoff(128_000, 0.8), 15); // 102K effective
        assert_eq!(compute_tool_call_cutoff(200_000, 0.8), 24); // 160K effective
        assert_eq!(compute_tool_call_cutoff(1_000_000, 0.8), 120); // 800K effective
                                                                   // Clamp at minimum
        assert_eq!(compute_tool_call_cutoff(50_000, 0.8), 10);
        assert_eq!(compute_tool_call_cutoff(10_000, 0.8), 10);
        // Clamp at maximum (500)
        assert_eq!(compute_tool_call_cutoff(10_000_000, 0.8), 500);
        // Lower compaction threshold means earlier summarization
        assert_eq!(compute_tool_call_cutoff(200_000, 0.3), 10); // 60K effective
        assert_eq!(compute_tool_call_cutoff(1_000_000, 0.5), 75); // 500K effective
                                                                  // Invalid threshold falls back to default 0.8
        assert_eq!(compute_tool_call_cutoff(200_000, 0.0), 24); // falls back to 0.8
        assert_eq!(compute_tool_call_cutoff(200_000, -1.0), 24); // falls back to 0.8
    }

    #[test]
    fn test_tool_ids_to_summarize_triggers_at_cutoff_plus_batch() {
        // cutoff=5, so we need >5+10=15 to trigger. 15 exactly should NOT trigger.
        let mut messages = vec![Message::user().with_text("hello")];
        for i in 0..15 {
            messages.extend(create_tool_pair(
                &format!("call{}", i),
                &format!("resp{}", i),
                "read_file",
                "content",
            ));
        }
        let conversation = Conversation::new_unvalidated(messages);
        let result = tool_ids_to_summarize(&conversation, 5, 0);
        assert!(result.is_empty(), "Exactly cutoff+batch should not trigger");

        // 16 tool calls: now exceeds cutoff+10, should return a batch of 10
        let mut messages = vec![Message::user().with_text("hello")];
        for i in 0..16 {
            messages.extend(create_tool_pair(
                &format!("call{}", i),
                &format!("resp{}", i),
                "read_file",
                "content",
            ));
        }
        let conversation = Conversation::new_unvalidated(messages);
        let result = tool_ids_to_summarize(&conversation, 5, 0);
        assert_eq!(result.len(), TOOLCALL_SUMMARIZATION_BATCH_SIZE);
        assert_eq!(result[0], "call0");
        assert_eq!(result[9], "call9");
    }

    #[test]
    fn test_tool_ids_to_summarize_protects_current_turn() {
        // 20 tool pairs, cutoff=2 → 20 > 12, would normally trigger
        let mut messages = vec![Message::user().with_text("hello")];
        for i in 0..20 {
            messages.extend(create_tool_pair(
                &format!("call{}", i),
                &format!("resp{}", i),
                "read_file",
                "content",
            ));
        }
        let conversation = Conversation::new_unvalidated(messages);

        // No protection: 20 eligible, 20 > 12 → batch of 10
        let result = tool_ids_to_summarize(&conversation, 2, 0);
        assert_eq!(result.len(), TOOLCALL_SUMMARIZATION_BATCH_SIZE);

        // Protect last 8: 12 eligible, 12 <= 12 → nothing
        let result = tool_ids_to_summarize(&conversation, 2, 8);
        assert!(
            result.is_empty(),
            "Should not summarize when protected count leaves eligible <= cutoff + batch"
        );

        // Protect last 7: 13 eligible, 13 > 12 → batch of 10
        let result = tool_ids_to_summarize(&conversation, 2, 7);
        assert_eq!(result.len(), TOOLCALL_SUMMARIZATION_BATCH_SIZE);
        assert_eq!(result[0], "call0");
    }
}
