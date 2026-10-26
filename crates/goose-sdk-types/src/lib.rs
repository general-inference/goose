//! Shared Goose types, with optional ACP wire methods.

#[cfg(feature = "acp")]
pub mod custom_notifications;
#[cfg(feature = "acp")]
pub mod custom_requests;
pub mod sources;
