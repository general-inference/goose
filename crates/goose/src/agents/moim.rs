use crate::agents::extension_manager::{ExtensionLease, ExtensionManager};
use crate::conversation::message::Message;
#[cfg(test)]
use crate::conversation::{CURRENT_TIME_TAG, TURN_CONTEXT_TAG};
use goose_agent::turn_context::{should_skip, turn_budget_part};
use std::path::{Path, PathBuf};

thread_local! {
    pub static SKIP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn system_prompt_block() -> Option<String> {
    if SKIP.with(|f| f.get()) {
        None
    } else {
        Some(goose_agent::turn_context::system_prompt_block())
    }
}

pub async fn turn_context_message(
    session_id: &str,
    extension_manager: &ExtensionManager,
    lease: &ExtensionLease,
    turns_taken: u32,
    max_turns: u32,
    turn_start: chrono::DateTime<chrono::Local>,
) -> Option<Message> {
    if SKIP.with(|f| f.get()) {
        return None;
    }

    let session = extension_manager
        .get_context()
        .session_manager
        .get_session(session_id, false)
        .await
        .ok();
    let session_model_config = session
        .as_ref()
        .and_then(|session| session.model_config.clone());
    let context_limit = match (session.as_ref(), session_model_config.as_ref()) {
        (Some(session), Some(model_config)) => {
            match extension_manager
                .get_context()
                .providers
                .provider_for(session)
                .await
            {
                Ok(provider) => crate::context_limit::get_context_limit(
                    provider.as_ref(),
                    &model_config.model_name,
                )
                .await
                .ok(),
                Err(_) => None,
            }
        }
        _ => None,
    };
    if should_skip(context_limit) {
        return None;
    }

    let working_dir = lease
        .working_dir()
        .map(Path::to_path_buf)
        .or_else(|| session.as_ref().map(|session| session.working_dir.clone()))
        .unwrap_or_else(|| PathBuf::from("."));
    let mut parts = lease.moim().await;
    parts.extend(turn_budget_part(turns_taken, max_turns));
    turn_context_event(&working_dir, context_limit, parts, turn_start)
}

/// The state-machine variant: operations contribute the parts and the caller
/// persists the event, so later requests in the turn reuse the same bytes.
pub(crate) fn turn_context_event(
    working_dir: &Path,
    context_limit: Option<usize>,
    parts: Vec<String>,
    turn_start: chrono::DateTime<chrono::Local>,
) -> Option<Message> {
    if SKIP.with(|f| f.get()) || should_skip(context_limit) {
        return None;
    }

    goose_agent::turn_context::turn_context_event(
        working_dir,
        context_limit,
        parts,
        &turn_start.format("%Y-%m-%d %H:%M:00 %:z").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::extension_manager::ExtensionSet;
    use crate::agents::mcp_client::McpClientTrait;
    use crate::agents::tool_execution::ToolCallContext;
    use crate::config::ExtensionConfig;
    use rmcp::model::{CallToolResult, InitializeResult, JsonObject, ListToolsResult};
    use rmcp::ServiceError;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct MoimClient(&'static str);

    #[async_trait::async_trait]
    impl McpClientTrait for MoimClient {
        async fn list_tools(
            &self,
            _session_id: &str,
            _next_cursor: Option<String>,
            _cancel_token: CancellationToken,
        ) -> Result<ListToolsResult, ServiceError> {
            Ok(ListToolsResult::default())
        }

        async fn call_tool(
            &self,
            _ctx: &ToolCallContext,
            _name: &str,
            _arguments: Option<JsonObject>,
            _cancel_token: CancellationToken,
        ) -> Result<CallToolResult, ServiceError> {
            unreachable!()
        }

        fn get_info(&self) -> Option<&InitializeResult> {
            None
        }

        async fn get_moim(
            &self,
            _session_id: &str,
            _tools: &[rmcp::model::Tool],
        ) -> Option<String> {
            Some(self.0.to_string())
        }
    }

    fn moim_extension() -> ExtensionConfig {
        ExtensionConfig::Platform {
            name: "todo".to_string(),
            description: String::new(),
            display_name: None,
            bundled: None,
            available_tools: Vec::new(),
        }
    }

    async fn session_and_manager() -> (String, Arc<ExtensionManager>, tempfile::TempDir) {
        let temp_dir = tempfile::tempdir().unwrap();
        let em = Arc::new(ExtensionManager::with_data_dir(
            temp_dir.path().to_path_buf(),
        ));
        let session = em
            .get_context()
            .session_manager
            .create_session(
                PathBuf::from("/test/dir"),
                "test".to_string(),
                crate::session::SessionType::User,
                crate::config::GooseMode::Auto,
            )
            .await
            .unwrap();
        (session.id, em, temp_dir)
    }

    #[tokio::test]
    async fn turn_context_message_is_an_agent_only_user_message() {
        let (session_id, em, _tmp) = session_and_manager().await;
        let lease = em.current_lease(&session_id).await.unwrap();

        let message = turn_context_message(&session_id, &em, &lease, 0, 100, chrono::Local::now())
            .await
            .expect("turn context should be produced");

        assert_eq!(message.role, rmcp::model::Role::User);
        assert!(message.is_agent_visible());
        assert!(!message.is_user_visible());
        assert!(message.is_turn_context());
        let text = message.content[0].as_text().unwrap();
        assert!(text.starts_with(&format!("<{TURN_CONTEXT_TAG}>\n")));
        assert!(text.trim_end().ends_with(&format!("</{TURN_CONTEXT_TAG}>")));
    }

    #[tokio::test]
    async fn turn_context_bytes_are_stable_for_a_turn() {
        let (session_id, em, _tmp) = session_and_manager().await;
        let lease = em.current_lease(&session_id).await.unwrap();
        let turn_start = chrono::Local::now();

        let first = turn_context_message(&session_id, &em, &lease, 0, 100, turn_start)
            .await
            .unwrap();
        let second = turn_context_message(&session_id, &em, &lease, 0, 100, turn_start)
            .await
            .unwrap();

        assert_eq!(
            first.content[0].as_text(),
            second.content[0].as_text(),
            "the same turn inputs must render byte-identical blocks"
        );
    }

    #[tokio::test]
    async fn turn_context_uses_the_inference_lease() {
        let (session_id, em, _tmp) = session_and_manager().await;
        em.add_client(
            &session_id,
            moim_extension(),
            Arc::new(MoimClient("old context")),
            None,
        )
        .await;
        let lease = em.current_lease(&session_id).await.unwrap();
        em.add_client(
            &session_id,
            moim_extension(),
            Arc::new(MoimClient("new context")),
            None,
        )
        .await;

        let message = turn_context_message(&session_id, &em, &lease, 0, 100, chrono::Local::now())
            .await
            .unwrap();
        let text = message.content[0].as_text().unwrap();

        assert!(text.contains("old context"));
        assert!(!text.contains("new context"));
    }

    #[tokio::test]
    async fn turn_context_uses_the_leased_working_dir() {
        let (session_id, em, _tmp) = session_and_manager().await;
        let leased_working_dir = PathBuf::from("/leased/dir");
        let lease = em
            .resolve(&ExtensionSet::new(&session_id, Some(leased_working_dir), vec![]).unwrap())
            .await;

        let message = turn_context_message(&session_id, &em, &lease, 0, 100, chrono::Local::now())
            .await
            .unwrap();
        let text = message.content[0].as_text().unwrap();

        assert!(text.contains("<working-directory>/leased/dir</working-directory>"));
        assert!(!text.contains("<working-directory>/test/dir</working-directory>"));
    }

    #[test]
    fn turn_context_event_is_marked_and_skips_small_contexts() {
        let event = turn_context_event(
            Path::new("/test/dir"),
            Some(200_000),
            vec![],
            chrono::Local::now(),
        )
        .expect("large-context models should get a turn-context event");
        assert!(event.is_turn_context());
        assert!(!event.is_user_visible());

        assert!(turn_context_event(
            Path::new("/test/dir"),
            Some(16_000),
            vec![],
            chrono::Local::now(),
        )
        .is_none());
    }

    #[test]
    fn current_time_is_timezone_aware() {
        let block = turn_context_event(
            Path::new("/Users/me/code/goose"),
            None,
            vec![],
            chrono::Local::now(),
        )
        .unwrap()
        .as_concat_text();
        let time_line = block
            .lines()
            .find(|line| line.starts_with(&format!("<{CURRENT_TIME_TAG}>")))
            .expect("turn-context block should contain a current-time line");

        let value = time_line
            .trim_start_matches(&format!("<{CURRENT_TIME_TAG}>"))
            .trim_end_matches(&format!("</{CURRENT_TIME_TAG}>"));

        chrono::DateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S %:z")
            .expect("current-time should include a numeric UTC offset");
    }
}
