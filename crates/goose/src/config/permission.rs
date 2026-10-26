use crate::config::paths::Paths;
pub use goose_agent::permission::store::PermissionConfig;
pub use goose_agent::permission::PermissionLevel;
use std::sync::{Arc, LazyLock};

static PERMISSION_MANAGER: LazyLock<Arc<PermissionManager>> =
    LazyLock::new(|| Arc::new(PermissionManager::new(Paths::config_dir())));

#[derive(Debug)]
pub struct PermissionManager(goose_agent::permission::store::PermissionManager);

impl PermissionManager {
    pub fn new(config_dir: std::path::PathBuf) -> Self {
        Self(goose_agent::permission::store::PermissionManager::new(
            config_dir,
        ))
    }
    pub fn instance() -> Arc<Self> {
        Arc::clone(&PERMISSION_MANAGER)
    }
}

impl std::ops::Deref for PermissionManager {
    type Target = goose_agent::permission::store::PermissionManager;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
