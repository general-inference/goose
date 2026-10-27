//! Makes filesystem skills available to inference and slash commands.

use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use goose_sdk_types::custom_requests::{SourceEntry, SourceType};
use rmcp::model::{CallToolResult, ContentBlock, ErrorData, JsonObject, Tool};
use serde_json::Value;
use tracing_futures::Instrument;

use crate::agents::extension_manager::ExtensionLease;
use crate::agents::state_machine::ops_toolcalling::{
    emit_post_tool_use, pending_advertised_tool_requests, run_pre_tool_hooks, tool_span,
    ToolDisposition, EXPIRED_APPROVAL_RESPONSE,
};
use crate::agents::state_machine::{
    applied, messages_since_kickoff, not_applicable, yielded_with, ConversationEffect, Emitter,
    GooseEffect, Operation, OperationResult, SlashCommand,
};
use crate::agents::tool_execution::{CHAT_MODE_TOOL_SKIPPED_RESPONSE, DECLINED_RESPONSE};
use crate::config::GooseMode;
use crate::conversation::message::Message;
use crate::conversation::Conversation;
use crate::hooks::HookManager;
use crate::session::Session;

const LOAD_SKILL_TOOL_NAME: &str = "load_skill";

pub struct SkillOperation {
    hook_manager: HookManager,
    extension_lease: Arc<StdMutex<Option<Arc<ExtensionLease>>>>,
}

fn skill_instructions(working_dir: &Path) -> Option<String> {
    let sources = crate::skills::discover_skills(Some(working_dir));
    let mut skills: Vec<&SourceEntry> = sources
        .iter()
        .filter(|source| {
            matches!(
                source.source_type,
                SourceType::Skill | SourceType::BuiltinSkill
            )
        })
        .collect();
    skills.sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
    if skills.is_empty() {
        return None;
    }

    let mut instructions = String::from(
        "# Skills\n\nYou have these skills at your disposal. Load one when it can help with the task or when the user asks for it:",
    );
    for skill in skills {
        instructions.push_str(&format!("\n- {}: {}", skill.name, skill.description));
    }
    Some(instructions)
}

fn execute_skill(working_dir: &Path, arguments: Option<JsonObject>) -> CallToolResult {
    goose_agent::skills::tool::load_skill(
        &crate::skills::discover_skills(Some(working_dir)),
        arguments,
        &crate::skills::GooseSkillLoader,
        crate::agents::max_tool_response_size(),
    )
}

impl SkillOperation {
    pub fn new(
        hook_manager: HookManager,
        extension_lease: Arc<StdMutex<Option<Arc<ExtensionLease>>>>,
    ) -> Self {
        Self {
            hook_manager,
            extension_lease,
        }
    }

    fn leased_session(&self, session: &Session) -> Option<Session> {
        let lease = self
            .extension_lease
            .lock()
            .expect("extension lease unavailable")
            .clone()?;
        if lease.scope_id() != session.id.as_str() {
            return None;
        }
        let mut session = session.clone();
        session.working_dir = lease.working_dir()?.to_path_buf();
        Some(session)
    }

    fn command_response(
        conversation: &Conversation,
        message: String,
        emit: &Emitter,
    ) -> Result<OperationResult<GooseEffect>> {
        let command = messages_since_kickoff(conversation)?
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("skill command conversation has no kickoff message"))?;
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
}

#[async_trait]
impl Operation<Session, GooseEffect> for SkillOperation {
    fn name(&self) -> &'static str {
        "skills"
    }

    async fn run_command(
        &self,
        command: &SlashCommand<'_>,
        session: &Session,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<GooseEffect>> {
        if command.command == "skills" {
            return Self::command_response(
                conversation,
                crate::slash_commands::skill_slash_command::format_installed_skills(Some(
                    &session.working_dir,
                )),
                emit,
            );
        }

        let prompt = match crate::slash_commands::skill_slash_command::resolve_command(
            command.command,
            command.params_str,
            Some(&session.working_dir),
        ) {
            Ok(Some(prompt)) => prompt,
            Ok(None) => return not_applicable(),
            Err(error) => return Self::command_response(conversation, error, emit),
        };
        let command_message = messages_since_kickoff(conversation)?
            .first()
            .ok_or_else(|| anyhow!("skill command conversation has no kickoff message"))?;
        let message_id = command_message
            .id
            .clone()
            .ok_or_else(|| anyhow!("Persisted slash command message has no id"))?;
        applied([
            ConversationEffect::SetMessageVisibility {
                message_id,
                user_visible: true,
                agent_visible: false,
            }
            .into(),
            Message::user()
                .with_text(prompt)
                .with_visibility(false, true)
                .into(),
        ])
    }

    async fn inference_tools(&self, _session: &Session) -> Result<Vec<Tool>> {
        Ok(vec![goose_agent::skills::tool::skill_tool()])
    }

    async fn prompt_parts(
        &self,
        session: &Session,
        _conversation: &Conversation,
    ) -> Result<Vec<(String, String)>> {
        Ok(skill_instructions(&session.working_dir)
            .map(|instructions| ("skills".to_string(), instructions))
            .into_iter()
            .collect())
    }

    async fn run(
        &self,
        session: &Session,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<GooseEffect>> {
        let messages = messages_since_kickoff(conversation)?;
        let pending: Vec<_> = pending_advertised_tool_requests(messages)
            .into_iter()
            .filter(|(request, _)| {
                request
                    .tool_call
                    .as_ref()
                    .is_ok_and(|tool_call| tool_call.name.as_ref() == LOAD_SKILL_TOOL_NAME)
            })
            .collect();
        if pending.is_empty() {
            return not_applicable();
        }

        let leased_session = self.leased_session(session);
        let mut response = Message::user();
        for (request, disposition) in pending {
            let result: std::result::Result<CallToolResult, ErrorData> = match disposition {
                ToolDisposition::Execute if session.goose_mode == GooseMode::Chat => {
                    // Nothing executes in chat mode, so no tool lifecycle runs.
                    Ok(CallToolResult::success(vec![ContentBlock::text(
                        CHAT_MODE_TOOL_SKIPPED_RESPONSE,
                    )]))
                }
                ToolDisposition::Execute => {
                    let Some(session) = leased_session.as_ref() else {
                        response.add_tool_response_with_metadata(
                            request.id,
                            Ok(CallToolResult::error(vec![ContentBlock::text(
                                EXPIRED_APPROVAL_RESPONSE,
                            )])),
                            request.metadata.as_ref(),
                        );
                        continue;
                    };
                    let tool_call = request.tool_call.as_ref().map_err(|error| {
                        anyhow!("load_skill tool call could not be parsed: {error}")
                    })?;
                    let span = tool_span(&tool_call.name, &request.id, &session.id);
                    // `load_skill` is executed here rather than by
                    // ToolExecutionOperation, which is registered after this one.
                    // Run the same hook lifecycle it would have run there.
                    let tool_input = tool_call
                        .arguments
                        .as_ref()
                        .map(|arguments| Value::Object(arguments.clone()));
                    match run_pre_tool_hooks(
                        &self.hook_manager,
                        session,
                        &request.id,
                        &tool_call.name,
                        tool_input.as_ref(),
                    )
                    .instrument(span.clone())
                    .await
                    {
                        // A denial returns before execution and emits no post
                        // event, the same shape ToolExecutionOperation has: its
                        // dispatch returns the denial before the post-hook wrapper
                        // is ever applied.
                        Err(denial) => Err(denial),
                        Ok(()) => {
                            let result = {
                                let _entered = span.enter();
                                execute_skill(&session.working_dir, tool_call.arguments.clone())
                            };
                            if result.is_error == Some(true) {
                                span.record("error.type", "tool_error");
                            }
                            let output = Ok(result);
                            // Post event carries the same tool_call_id as the pre
                            // events. The large-response rewrite
                            // ToolExecutionOperation applies is deliberately not
                            // reused: a skill body is content the model is meant to
                            // read, not a payload to offload to a temp file.
                            emit_post_tool_use(
                                &self.hook_manager,
                                &session.id,
                                &session.working_dir.to_string_lossy(),
                                &tool_call.name,
                                &request.id,
                                tool_input.as_ref(),
                                &output,
                            )
                            .instrument(span.clone())
                            .await;
                            output
                        }
                    }
                }
                ToolDisposition::Decline => Ok(CallToolResult::error(vec![ContentBlock::text(
                    DECLINED_RESPONSE,
                )])),
                ToolDisposition::ParseError(error) => {
                    Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                        "The tool call could not be parsed: {error}. Correct the arguments and try again."
                    ))]))
                }
            };
            response.add_tool_response_with_metadata(request.id, result, request.metadata.as_ref());
        }
        let response = emit.message(response);
        applied([response.into()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn supporting_file_loader_reads_nested_regular_file() {
        let root = tempfile::tempdir().unwrap();
        let skill_dir = std::fs::canonicalize(root.path()).unwrap();
        let nested = skill_dir.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let file = nested.join("guide.md");
        std::fs::write(&file, "Nested guidance.").unwrap();
        let skill = SourceEntry {
            source_type: SourceType::Skill,
            name: "test-skill".to_string(),
            description: String::new(),
            content: String::new(),
            path: skill_dir.to_string_lossy().into_owned(),
            global: false,
            writable: true,
            supporting_files: vec![file.to_string_lossy().into_owned()],
            properties: HashMap::new(),
        };

        let result = goose_agent::skills::tool::load_skill(
            &[skill],
            Some(
                serde_json::json!({"name":"test-skill/nested/guide.md"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            &crate::skills::GooseSkillLoader,
            crate::agents::max_tool_response_size(),
        );

        assert_eq!(result.is_error, Some(false));
        let text = result.content[0].as_text().expect("expected text");
        assert!(text.text.contains("Nested guidance."));
    }
}
