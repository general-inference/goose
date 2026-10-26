use async_trait::async_trait;
use goose_agent::{operation::Emitter, tool::ToolProvider};
use goose_developer::{
    edit::EditTools,
    filesystem::{FileSystem, LocalFileSystem},
    process::{RunningShell, ShellCommand, ShellExecutor},
    shell::ShellTool,
    Developer, DeveloperContext, DeveloperTool,
};
use rmcp::{model::CallToolRequestParams, object};
use std::{io, path::Path, sync::Arc};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct ReadOnly;
impl FileSystem for ReadOnly {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        LocalFileSystem.read_to_string(path)
    }
    fn write(&self, _: &Path, _: &str) -> io::Result<()> {
        Err(io::ErrorKind::PermissionDenied.into())
    }
    fn create_dir_all(&self, _: &Path) -> io::Result<()> {
        Err(io::ErrorKind::PermissionDenied.into())
    }
    fn exists(&self, path: &Path) -> bool {
        LocalFileSystem.exists(path)
    }
}
struct DenyProcess;
#[async_trait]
impl ShellExecutor for DenyProcess {
    async fn spawn(&self, _: ShellCommand<'_>) -> io::Result<Box<dyn RunningShell>> {
        Err(io::ErrorKind::PermissionDenied.into())
    }
}

#[tokio::test]
async fn text_tools_cannot_bypass_the_host_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "original").unwrap();
    let provider = Developer::new()
        .unwrap()
        .with_tools([DeveloperTool::Write, DeveloperTool::Edit])
        .with_edit_tools(EditTools::with_filesystem(Arc::new(ReadOnly)));
    let context = DeveloperContext {
        working_dir: Some(dir.path().to_owned()),
        ..Default::default()
    };
    let (tx, _rx) = mpsc::unbounded_channel();
    let emit = Emitter::new(tx, CancellationToken::new());
    for call in [
        CallToolRequestParams::new("edit").with_arguments(
            object!({"path": "notes.txt", "before": "original", "after": "changed"}),
        ),
        CallToolRequestParams::new("write")
            .with_arguments(object!({"path": "new.txt", "content": "changed"})),
    ] {
        assert_eq!(
            provider
                .call(&context, "call", call, &emit)
                .await
                .unwrap()
                .is_error,
            Some(true)
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "original"
    );
    assert!(!dir.path().join("new.txt").exists());
    let names: Vec<_> = provider
        .tools(&context)
        .await
        .unwrap()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(names, ["write", "edit"]);
    let shell =
        CallToolRequestParams::new("shell").with_arguments(object!({"command": "touch forbidden"}));
    assert!(provider
        .call(&context, "shell", shell, &emit)
        .await
        .is_err());
    assert!(!dir.path().join("forbidden").exists());
}

#[tokio::test]
async fn shell_tool_cannot_bypass_the_host_executor() {
    let dir = tempfile::tempdir().unwrap();
    let shell = ShellTool::with_executor(Arc::new(DenyProcess)).unwrap();
    let provider = Developer::new()
        .unwrap()
        .with_tools([DeveloperTool::Shell])
        .with_shell_tool(shell);
    let context = DeveloperContext {
        working_dir: Some(dir.path().to_owned()),
        ..Default::default()
    };
    let (tx, _rx) = mpsc::unbounded_channel();
    let emit = Emitter::new(tx, CancellationToken::new());
    let call =
        CallToolRequestParams::new("shell").with_arguments(object!({"command": "touch forbidden"}));
    let result = provider.call(&context, "shell", call, &emit).await.unwrap();
    assert_eq!(result.is_error, Some(true));
    assert!(!dir.path().join("forbidden").exists());
}
