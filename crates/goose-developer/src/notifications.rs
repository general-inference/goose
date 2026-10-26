use rmcp::model::ServerNotification;
use std::sync::Arc;
use tokio::sync::mpsc;

/// A non-blocking notification sink. Callbacks must return promptly.
#[derive(Clone)]
pub struct ToolCallNotificationEmitter(Arc<dyn Fn(ServerNotification) + Send + Sync>);
impl ToolCallNotificationEmitter {
    pub fn new(sender: mpsc::Sender<ServerNotification>) -> Self {
        Self::from_callback(move |notification| {
            let _ = sender.try_send(notification);
        })
    }
    pub fn from_callback(callback: impl Fn(ServerNotification) + Send + Sync + 'static) -> Self {
        Self(Arc::new(callback))
    }
    pub fn emit_best_effort(&self, notification: ServerNotification) {
        (self.0)(notification);
    }
}
