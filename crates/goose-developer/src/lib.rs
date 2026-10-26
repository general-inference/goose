//! Goose's native developer tools shared with GDK embedders.
//! Filesystem and process access use the host account unless a backend is supplied.

pub mod edit;
pub mod image;
pub mod notifications;
pub mod shell;
mod shell_output_streaming;
mod shell_process;
pub mod tree;

use anyhow::Result;
use async_trait::async_trait;
use edit::{EditTools, FileEditParams, FileWriteParams};
use image::{ImageReadParams, ImageTool};
use rmcp::model::{
    Annotations, CallToolResult, ContentBlock, Implementation, InitializeResult, JsonObject,
    ListToolsResult, ServerCapabilities, TextContent, Tool, ToolAnnotations,
};
use schemars::{schema_for, JsonSchema};
use serde_json::Value;
use shell::{shell_display_name, ShellOutput, ShellParams, ShellTool};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tree::{TreeParams, TreeTool};

pub static EXTENSION_NAME: &str = "developer";

fn visible_text(text: impl Into<String>) -> ContentBlock {
    ContentBlock::Text(
        TextContent::new(text).with_annotations(Annotations::default().with_priority(0.0)),
    )
}

pub struct Developer {
    info: InitializeResult,
    shell_tool: Arc<ShellTool>,
    edit_tools: Arc<EditTools>,
    tree_tool: Arc<TreeTool>,
    image_tool: Arc<ImageTool>,
}

pub fn developer_instructions() -> &'static str {
    if cfg!(windows) {
        include_str!("../prompts/windows.md")
    } else {
        include_str!("../prompts/unix.md")
    }
}

impl Developer {
    pub fn new() -> Result<Self> {
        let info = InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(EXTENSION_NAME, "1.0.0").with_title("Developer"))
            .with_instructions(developer_instructions());

        Ok(Self {
            info,
            shell_tool: Arc::new(ShellTool::new()?),
            edit_tools: Arc::new(EditTools::new()),
            tree_tool: Arc::new(TreeTool::new()),
            image_tool: Arc::new(ImageTool::new()),
        })
    }

    fn schema<T: JsonSchema>() -> JsonObject {
        serde_json::to_value(schema_for!(T))
            .expect("schema serialization should succeed")
            .as_object()
            .expect("schema should serialize to an object")
            .clone()
    }

    pub fn parse_args<T: serde::de::DeserializeOwned>(
        arguments: Option<JsonObject>,
    ) -> Result<T, String> {
        let value = arguments
            .map(Value::Object)
            .ok_or_else(|| "Missing arguments".to_string())?;
        serde_json::from_value(value).map_err(|e| format!("Failed to parse arguments: {e}"))
    }

    pub fn get_tools() -> Vec<Tool> {
        vec![
            Tool::new(
                "write".to_string(),
                "Create a new file or overwrite an existing file. Creates parent directories if needed.".to_string(),
                Self::schema::<FileWriteParams>(),
            )
            .annotate(ToolAnnotations::from_raw(
                Some("Write".to_string()),
                Some(false),
                Some(true),
                Some(false),
                Some(false),
            )),
            Tool::new(
                "edit".to_string(),
                "Edit a file by finding and replacing text. The before text must match exactly and uniquely. Use empty after text to delete.".to_string(),
                Self::schema::<FileEditParams>(),
            )
            .annotate(ToolAnnotations::from_raw(
                Some("Edit".to_string()),
                Some(false),
                Some(true),
                Some(false),
                Some(false),
            )),
            {
                let shell = shell_display_name();
                let newline_note = if shell == "cmd" {
                    " Commands must be on a single line — cmd.exe silently truncates at the \
                     first newline. Use `&` to chain (e.g. `echo a & echo b`) or set \
                     GOOSE_SHELL=powershell for multi-line support."
                } else {
                    ""
                };
                let description = format!(
                    "Execute a shell command in the current dir. Commands run under `{shell}` \
                     (set GOOSE_SHELL to override) - write command strings in that shell's \
                     syntax.{newline_note} Returns an object with stdout and stderr as separate \
                     fields. The output of each stream is limited to up to 2000 lines, and \
                     longer outputs will be saved to a temporary file.",
                );
                Tool::new("shell".to_string(), description, Self::schema::<ShellParams>())
            }
            .with_output_schema::<ShellOutput>()
            .annotate(ToolAnnotations::from_raw(
                Some("Shell".to_string()),
                Some(false),
                Some(true),
                Some(false),
                Some(true),
            )),
            Tool::new(
                "tree".to_string(),
                "List a directory tree with line counts. Traversal respects .gitignore rules.".to_string(),
                Self::schema::<TreeParams>(),
            )
            .annotate(ToolAnnotations::from_raw(
                Some("Tree".to_string()),
                Some(true),
                Some(false),
                Some(true),
                Some(false),
            )),
            Tool::new(
                "read_image".to_string(),
                "Read an image from a local file path or http(s) URL and return it as image content for the model to inspect. Supports png, jpeg, gif, and webp.".to_string(),
                Self::schema::<ImageReadParams>(),
            )
            .annotate(ToolAnnotations::from_raw(
                Some("Read Image".to_string()),
                Some(false),
                Some(false),
                Some(true),
                Some(true),
            )),
        ]
    }
}

impl Developer {
    pub async fn list_tools(
        &self,
        _session_id: &str,
        _next_cursor: Option<String>,
        _cancellation_token: CancellationToken,
    ) -> Result<ListToolsResult, rmcp::model::ErrorData> {
        Ok(ListToolsResult {
            tools: Self::get_tools(),
            next_cursor: None,
            meta: None,
            ..Default::default()
        })
    }

    pub async fn call_tool(
        &self,
        ctx: &DeveloperContext,
        name: &str,
        arguments: Option<JsonObject>,
        cancel_token: CancellationToken,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let working_dir = ctx.working_dir.as_deref();
        match name {
            "shell" => match Self::parse_args::<ShellParams>(arguments) {
                Ok(mut params) => {
                    params.timeout_secs.get_or_insert(ctx.default_timeout);
                    Ok(self
                        .shell_tool
                        .shell_with_cwd_and_emitter(
                            params,
                            working_dir,
                            ctx.session_id.as_deref(),
                            ctx.notifications.clone(),
                            cancel_token,
                        )
                        .await)
                }
                Err(error) => Ok(ShellTool::error_result(&format!("Error: {error}"), None)),
            },
            "write" => match Self::parse_args::<FileWriteParams>(arguments) {
                Ok(params) => Ok(self.edit_tools.file_write_with_cwd(params, working_dir)),
                Err(error) => Ok(CallToolResult::error(vec![visible_text(format!(
                    "Error: {error}"
                ))])),
            },
            "edit" => match Self::parse_args::<FileEditParams>(arguments) {
                Ok(params) => Ok(self.edit_tools.file_edit_with_cwd(params, working_dir)),
                Err(error) => Ok(CallToolResult::error(vec![visible_text(format!(
                    "Error: {error}"
                ))])),
            },
            "tree" => match Self::parse_args::<TreeParams>(arguments) {
                Ok(params) => Ok(self.tree_tool.tree_with_cwd(params, working_dir)),
                Err(error) => Ok(CallToolResult::error(vec![visible_text(format!(
                    "Error: {error}"
                ))])),
            },
            "read_image" => match Self::parse_args::<ImageReadParams>(arguments) {
                Ok(params) => Ok(self
                    .image_tool
                    .image_read_with_cwd(params, working_dir)
                    .await),
                Err(error) => Ok(CallToolResult::error(vec![visible_text(format!(
                    "Error: {error}"
                ))])),
            },
            _ => Ok(CallToolResult::error(vec![visible_text(format!(
                "Error: Unknown tool: {name}"
            ))])),
        }
    }

    pub fn get_info(&self) -> Option<&InitializeResult> {
        Some(&self.info)
    }
}

/// Execution context supplied by the embedding application for each call.
#[derive(Clone)]
pub struct DeveloperContext {
    pub working_dir: Option<std::path::PathBuf>,
    pub session_id: Option<String>,
    pub notifications: Option<notifications::ToolCallNotificationEmitter>,
    pub default_timeout: u64,
}
impl Default for DeveloperContext {
    fn default() -> Self {
        Self {
            working_dir: None,
            session_id: None,
            notifications: None,
            default_timeout: 300,
        }
    }
}

pub trait DeveloperSession: Send + Sync {
    fn developer_context(&self) -> DeveloperContext;
}
impl DeveloperSession for DeveloperContext {
    fn developer_context(&self) -> DeveloperContext {
        self.clone()
    }
}
#[async_trait]
impl<S: DeveloperSession> goose_agent::tool::ToolProvider<S> for Developer {
    async fn tools(&self, _session: &S) -> Result<Vec<Tool>> {
        Ok(Self::get_tools())
    }
    async fn call(
        &self,
        session: &S,
        request_id: &str,
        call: rmcp::model::CallToolRequestParams,
        emit: &goose_agent::operation::Emitter,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let mut context = session.developer_context();
        let event_emitter = emit.clone();
        let request_id = request_id.to_owned();
        context.notifications = Some(notifications::ToolCallNotificationEmitter::from_callback(
            move |notification| {
                event_emitter.emit(goose_agent::events::AgentEvent::McpNotification((
                    request_id.clone(),
                    notification,
                )));
            },
        ));
        self.call_tool(
            &context,
            &call.name,
            call.arguments,
            emit.cancel_token().clone(),
        )
        .await
    }
}
