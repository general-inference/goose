use goose_agent::{events::AgentEvent, operation::Emitter, tool::ToolProvider};
use goose_developer::{Developer, DeveloperContext};
use rmcp::{model::CallToolRequestParams, object};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn gdk_calls_edit_files_in_the_session_directory() {
    let directory = tempfile::tempdir().unwrap();
    let session = DeveloperContext {
        working_dir: Some(directory.path().to_path_buf()),
        ..Default::default()
    };
    let tools = Developer::new().unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let emit = Emitter::new(tx, CancellationToken::new());
    let write = CallToolRequestParams::new("write").with_arguments(object!({
        "path": "notes.txt", "content": "before"
    }));
    assert_eq!(
        tools
            .call(&session, "write-1", write, &emit)
            .await
            .unwrap()
            .is_error,
        Some(false)
    );
    let edit = CallToolRequestParams::new("edit").with_arguments(object!({
        "path": "notes.txt", "before": "before", "after": "after"
    }));
    assert_eq!(
        tools
            .call(&session, "edit-1", edit, &emit)
            .await
            .unwrap()
            .is_error,
        Some(false)
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("notes.txt")).unwrap(),
        "after"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn gdk_shell_notifications_keep_the_request_id() {
    let tools = Developer::new().unwrap();
    let session = DeveloperContext {
        session_id: Some("gdk-session".into()),
        ..Default::default()
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let emit = Emitter::new(tx, CancellationToken::new());
    let call = CallToolRequestParams::new("shell")
        .with_arguments(object!({"command": "printf '%s' \"$AGENT_SESSION_ID\""}));
    let result = tools.call(&session, "shell-1", call, &emit).await.unwrap();
    assert_eq!(result.structured_content.unwrap()["stdout"], "gdk-session");
    let mut notifications = 0;
    while let Ok(event) = rx.try_recv() {
        if let AgentEvent::McpNotification((id, _)) = event {
            assert_eq!(id, "shell-1");
            notifications += 1;
        }
    }
    assert!(notifications > 0);
}
