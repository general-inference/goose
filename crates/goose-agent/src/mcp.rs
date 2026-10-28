//! MCP connections for embedded hosts. Connecting never starts interactive authorization.
//!
//! Hosts supply process policy, HTTP headers, credential storage, and the browser/callback UI.
//! The MCP SDK owns protocol negotiation, OAuth discovery, PKCE, token validation and refresh.

use rmcp::model::{CallToolRequestParams, CallToolResult, ClientConfig, Tool};
use rmcp::service::{
    ClientInitializeError, ClientLifecycleMode, ClientServiceExt, PeerRequestOptions,
    RequestHandle, RunningService,
};
use rmcp::transport::auth::{AuthClient, AuthorizationManager, CredentialStore};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransportConfig, StreamableHttpError,
};
use rmcp::transport::{
    DynamicTransportError, IntoTransport, StreamableHttpClientTransport, TokioChildProcess,
};
use rmcp::{RoleClient, ServiceError};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub use rmcp::transport::auth::{AuthorizationRequest, AuthorizationSession};

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("MCP authentication is required")]
    AuthenticationRequired { challenge: Option<String> },
    #[error("MCP connection timed out")]
    Timeout,
    #[error("MCP connection was cancelled")]
    Cancelled,
    // Transports can put headers, command arguments and server stderr in errors.
    // Embedded hosts should opt in to their own private diagnostics, not leak these to models.
    #[error("MCP connection failed")]
    Unavailable,
}

pub struct Connection {
    service: RunningService<RoleClient, ClientConfig>,
    timeout: Duration,
}

impl Connection {
    pub async fn connect<T, E, A>(
        transport: T,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Self, ConnectionError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + From<std::io::Error> + Send + Sync + 'static,
    {
        let connect = ClientConfig::default().serve_with_lifecycle(
            transport,
            ClientLifecycleMode::Auto {
                preferred_versions: vec![
                    rmcp::model::ProtocolVersion::V_2026_07_28,
                    rmcp::model::ProtocolVersion::V_2025_11_25,
                ],
                legacy_version: Some(rmcp::model::ProtocolVersion::V_2025_11_25),
            },
        );
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ConnectionError::Cancelled),
            result = tokio::time::timeout(timeout, connect) => match result {
                Ok(Ok(service)) => Ok(Self { service, timeout }),
                Ok(Err(error)) => Err(initialize_error(&error)),
                Err(_) => Err(ConnectionError::Timeout),
            }
        }
    }

    /// The host configures environment, cwd and process isolation before calling this.
    /// Stderr is discarded so credential-bearing server diagnostics cannot reach a model.
    pub async fn stdio(
        mut command: tokio::process::Command,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Self, ConnectionError> {
        command
            .kill_on_drop(true)
            .stderr(std::process::Stdio::null());
        let transport =
            TokioChildProcess::new(command).map_err(|_| ConnectionError::Unavailable)?;
        Self::connect(transport, timeout, cancel).await
    }

    /// A credential manager may refresh existing credentials, but never launches sign-in.
    pub async fn http(
        uri: &str,
        http: reqwest::Client,
        authorization: Option<AuthorizationManager>,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Self, ConnectionError> {
        let config = StreamableHttpClientTransportConfig::with_uri(uri);
        match authorization {
            Some(manager) => {
                Self::connect(
                    StreamableHttpClientTransport::with_client(
                        AuthClient::new(http, manager),
                        config,
                    ),
                    timeout,
                    cancel,
                )
                .await
            }
            None => {
                Self::connect(
                    StreamableHttpClientTransport::with_client(http, config),
                    timeout,
                    cancel,
                )
                .await
            }
        }
    }

    pub async fn tools(&self, cancel: &CancellationToken) -> Result<Vec<Tool>, ServiceError> {
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let handle = self
                .service
                .send_cancellable_request(
                    rmcp::model::ClientRequest::ListToolsRequest(
                        rmcp::model::RequestOptionalParam::with_param(
                            rmcp::model::PaginatedRequestParams::default().with_cursor(cursor),
                        ),
                    ),
                    PeerRequestOptions::no_options(),
                )
                .await?;
            let result = await_response(handle, self.timeout, cancel).await?;
            let rmcp::model::ServerResult::ListToolsResult(result) = result else {
                return Err(ServiceError::UnexpectedResponse);
            };
            tools.extend(result.tools);
            cursor = result.next_cursor;
            if cursor.is_none() {
                return Ok(tools);
            }
            if !seen.insert(cursor.clone()) || seen.len() > 1_000 || tools.len() > 10_000 {
                return Err(ServiceError::TransportClosed);
            }
        }
    }

    pub async fn call(
        &self,
        call: CallToolRequestParams,
        cancel: &CancellationToken,
    ) -> Result<CallToolResult, ServiceError> {
        let handle = self
            .service
            .send_cancellable_request(
                rmcp::model::ClientRequest::CallToolRequest(rmcp::model::Request::new(call)),
                PeerRequestOptions::no_options(),
            )
            .await?;
        match await_response(handle, self.timeout, cancel).await? {
            rmcp::model::ServerResult::CallToolResult(result) => Ok(result),
            _ => Err(ServiceError::UnexpectedResponse),
        }
    }
}

/// Load credentials through a host store without discovery or an interactive fallback.
pub async fn stored_authorization<S: CredentialStore + 'static>(
    uri: &str,
    http: reqwest::Client,
    store: S,
) -> Result<Option<AuthorizationManager>, rmcp::transport::auth::AuthError> {
    let mut manager = AuthorizationManager::new(uri).await?;
    manager.with_client(http)?;
    manager.set_credential_store(store);
    if manager.initialize_from_store().await? {
        Ok(Some(manager))
    } else {
        Ok(None)
    }
}

pub fn authentication_required(error: &ServiceError) -> bool {
    match error {
        ServiceError::TransportSend(error) => transport_auth_required(error),
        _ => false,
    }
}

fn initialize_error(error: &ClientInitializeError) -> ConnectionError {
    if error.is_authorization_required() || error.auth_challenge().is_some() {
        return ConnectionError::AuthenticationRequired {
            challenge: error.auth_challenge().map(str::to_owned),
        };
    }
    match error {
        ClientInitializeError::TransportError { error, .. } if transport_auth_required(error) => {
            ConnectionError::AuthenticationRequired { challenge: None }
        }
        _ => ConnectionError::Unavailable,
    }
}

fn transport_auth_required(error: &DynamicTransportError) -> bool {
    error
        .error
        .downcast_ref::<StreamableHttpError<reqwest::Error>>()
        .is_some_and(|error| match error {
            StreamableHttpError::AuthRequired(_) | StreamableHttpError::InsufficientScope(_) => {
                true
            }
            StreamableHttpError::UnexpectedServerResponse(body) => {
                body.starts_with("HTTP 401") || body.starts_with("HTTP 403")
            }
            _ => false,
        })
}

/// Shared by the full Goose client and embedded clients so timeout and cancellation send
/// the same protocol notification before abandoning a pending response.
pub async fn await_response(
    handle: RequestHandle<RoleClient>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<rmcp::model::ServerResult, ServiceError> {
    let receiver = handle.rx;
    let peer = handle.peer;
    let request_id = handle.id;
    tokio::select! {
        result = receiver => result.map_err(|_| ServiceError::TransportClosed)?,
        _ = tokio::time::sleep(timeout) => {
            peer.send_notification(rmcp::model::Notification::new(
                rmcp::model::CancelledNotificationParam::new(Some(request_id), Some("timed out".to_owned())),
            ).into()).await?;
            Err(ServiceError::Timeout { timeout })
        },
        _ = cancel.cancelled() => {
            peer.send_notification(rmcp::model::Notification::new(
                rmcp::model::CancelledNotificationParam::new(Some(request_id), Some("operation cancelled".to_owned())),
            ).into()).await?;
            Err(ServiceError::Cancelled { reason: None })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::transport::streamable_http_client::{AuthRequiredError, InsufficientScopeError};

    #[test]
    fn auth_status_is_typed_and_does_not_expose_transport_diagnostics() {
        for (error, auth) in [
            (
                StreamableHttpError::<reqwest::Error>::AuthRequired(AuthRequiredError::new(
                    "Bearer resource_metadata=\"https://example.invalid/metadata\"".into(),
                )),
                true,
            ),
            (
                StreamableHttpError::InsufficientScope(InsufficientScopeError::new(
                    "Bearer scope=read".into(),
                    Some("read".into()),
                )),
                true,
            ),
            (
                StreamableHttpError::UnexpectedServerResponse("HTTP 401".into()),
                true,
            ),
            (
                StreamableHttpError::UnexpectedServerResponse(
                    "HTTP 500 private diagnostics".into(),
                ),
                false,
            ),
        ] {
            let dynamic = DynamicTransportError::from_parts(
                "fixture",
                std::any::TypeId::of::<()>(),
                Box::new(error),
            );
            let error = ClientInitializeError::TransportError {
                error: dynamic,
                context: "initialize".into(),
            };
            let classified = initialize_error(&error);
            assert_eq!(
                matches!(classified, ConnectionError::AuthenticationRequired { .. }),
                auth
            );
            assert!(!classified.to_string().contains("private diagnostics"));
        }
    }
}
