#![cfg(all(feature = "mcp", not(target_arch = "wasm32")))]

use goose_agent::mcp::{stored_authorization, Connection, ConnectionError};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceError, ServiceExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Fixture {
    started: Arc<Notify>,
    cancelled: Arc<Notify>,
}

impl ServerHandler for Fixture {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let second = request.and_then(|r| r.cursor).is_some();
        let mut result = ListToolsResult::with_all_items(vec![Tool::new(
            if second { "second" } else { "first" },
            "fixture",
            serde_json::Map::new(),
        )]);
        result.next_cursor = (!second).then(|| "page2".into());
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if request.name == "wait" {
            self.started.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(request.name)]).into())
    }

    async fn on_cancelled(
        &self,
        _: rmcp::model::CancelledNotificationParam,
        _: NotificationContext<RoleServer>,
    ) {
        self.cancelled.notify_one();
    }
}

async fn connection(timeout: Duration) -> (Connection, Fixture, tokio::task::JoinHandle<()>) {
    let fixture = Fixture {
        started: Arc::default(),
        cancelled: Arc::default(),
    };
    let (client_io, server_io) = tokio::io::duplex(65536);
    let handler = fixture.clone();
    let server = tokio::spawn(async move {
        let service = handler.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    let connection = Connection::connect(client_io, timeout, &CancellationToken::new())
        .await
        .unwrap();
    (connection, fixture, server)
}

#[tokio::test]
async fn lists_every_page_and_preserves_tool_calls() {
    let (connection, _, server) = connection(Duration::from_secs(2)).await;
    let cancel = CancellationToken::new();
    let tools = connection.tools(&cancel).await.unwrap();
    assert_eq!(
        tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>(),
        ["first", "second"]
    );
    let result = connection
        .call(CallToolRequestParams::new("second"), &cancel)
        .await
        .unwrap();
    assert_eq!(result.content[0].as_text().unwrap().text, "second");
    drop(connection);
    server.abort();
}

#[tokio::test]
async fn cancellation_notifies_the_server() {
    let (connection, fixture, server) = connection(Duration::from_secs(2)).await;
    let cancel = CancellationToken::new();
    let call = connection.call(CallToolRequestParams::new("wait"), &cancel);
    let cancel_call = async {
        fixture.started.notified().await;
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(call, cancel_call);
    assert!(matches!(result, Err(ServiceError::Cancelled { .. })));
    tokio::time::timeout(Duration::from_secs(1), fixture.cancelled.notified())
        .await
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn timeout_notifies_the_server_and_connection_start_can_be_cancelled() {
    let (connection, fixture, server) = connection(Duration::from_millis(100)).await;
    let result = connection
        .call(
            CallToolRequestParams::new("wait"),
            &CancellationToken::new(),
        )
        .await;
    assert!(matches!(result, Err(ServiceError::Timeout { .. })));
    tokio::time::timeout(Duration::from_secs(1), fixture.cancelled.notified())
        .await
        .unwrap();
    server.abort();
    let (io, _held) = tokio::io::duplex(4096);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        Connection::connect(io, Duration::from_secs(1), &cancel).await,
        Err(ConnectionError::Cancelled)
    ));
}

#[tokio::test]
async fn absent_credentials_do_not_start_authorization() {
    let authorization = stored_authorization(
        "https://example.invalid/mcp",
        reqwest::Client::new(),
        rmcp::transport::auth::InMemoryCredentialStore::new(),
    )
    .await
    .unwrap();
    assert!(authorization.is_none());
}
