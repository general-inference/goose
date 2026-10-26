use crate::config::paths::Paths;
use goose_agent::project_context::{self as shared, NativeHintFileSystem};
use ignore::gitignore::Gitignore;
pub use shared::{AGENTS_MD_FILENAME, GOOSE_HINTS_FILENAME};
use std::path::{Path, PathBuf};

pub fn get_context_filenames() -> Vec<String> {
    use crate::config::Config;

    Config::global()
        .get_param::<Vec<String>>("CONTEXT_FILE_NAMES")
        .unwrap_or_else(|_| {
            vec![
                GOOSE_HINTS_FILENAME.to_string(),
                AGENTS_MD_FILENAME.to_string(),
            ]
        })
}

#[derive(Default)]
pub struct SubdirectoryHintTracker(shared::SubdirectoryHintTracker);
impl SubdirectoryHintTracker {
    pub fn new() -> Self {
        Self(shared::SubdirectoryHintTracker::new(get_context_filenames()))
    }
    pub fn record_tool_arguments(
        &mut self,
        arguments: &Option<serde_json::Map<String, serde_json::Value>>,
        working_dir: &Path,
    ) {
        self.0.record_tool_arguments(arguments, working_dir);
    }
    pub fn load_new_hints(&mut self, working_dir: &Path) -> Vec<(String, String)> {
        self.0.load_new_hints(&NativeHintFileSystem, working_dir)
    }
}
pub fn build_gitignore(cwd: &Path) -> Gitignore {
    shared::build_gitignore(&NativeHintFileSystem, cwd)
}
pub fn load_hint_files(
    cwd: &Path,
    hints_filenames: &[String],
    ignore_patterns: &Gitignore,
) -> String {
    let mut global_hints_paths: Vec<PathBuf> = hints_filenames
        .iter()
        .map(|name| Paths::in_config_dir(name))
        .collect();
    if hints_filenames
        .iter()
        .any(|name| name == AGENTS_MD_FILENAME)
    {
        global_hints_paths.push(Paths::in_agents_home_dir(AGENTS_MD_FILENAME));
    }

    shared::load_hint_files(
        &NativeHintFileSystem,
        cwd,
        hints_filenames,
        ignore_patterns,
        &global_hints_paths,
    )
}
#[cfg(test)]
#[path = "load_tests.rs"]
mod tests;
