use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use super::filesystem::HintFileSystem;
use super::import_files::read_referenced_files;

pub const GOOSE_HINTS_FILENAME: &str = ".goosehints";
pub const AGENTS_MD_FILENAME: &str = "AGENTS.md";

#[derive(Default)]
pub struct SubdirectoryHintTracker {
    loaded_dirs: HashSet<PathBuf>,
    pending_dirs: Vec<PathBuf>,
    hints_filenames: Vec<String>,
}

impl SubdirectoryHintTracker {
    pub fn new(hints_filenames: Vec<String>) -> Self {
        Self {
            loaded_dirs: HashSet::new(),
            pending_dirs: Vec::new(),
            hints_filenames,
        }
    }

    pub fn record_tool_arguments(
        &mut self,
        arguments: &Option<serde_json::Map<String, serde_json::Value>>,
        working_dir: &Path,
    ) {
        let args = match arguments.as_ref() {
            Some(a) => a,
            None => return,
        };

        if let Some(path_str) = args.get("path").and_then(|v| v.as_str()) {
            if let Some(dir) = resolve_to_parent_dir(path_str, working_dir) {
                self.pending_dirs.push(dir);
            }
        }

        if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
            for token in shell_words::split(cmd).unwrap_or_default() {
                if token.starts_with('-') {
                    continue;
                }
                if token.contains(std::path::MAIN_SEPARATOR) || token.contains('.') {
                    if let Some(dir) = resolve_to_parent_dir(&token, working_dir) {
                        self.pending_dirs.push(dir);
                    }
                }
            }
        }
    }

    pub fn load_new_hints(
        &mut self,
        fs: &dyn HintFileSystem,
        working_dir: &Path,
    ) -> Vec<(String, String)> {
        let pending = std::mem::take(&mut self.pending_dirs);
        if pending.is_empty() {
            return Vec::new();
        }

        let Ok(working_dir) = fs.canonicalize(working_dir) else {
            return Vec::new();
        };

        let mut results = Vec::new();
        for dir in pending {
            let Ok(dir) = fs.canonicalize(&dir) else {
                continue;
            };
            if !dir.starts_with(&working_dir) || dir == working_dir {
                continue;
            }
            if self.loaded_dirs.contains(&dir) {
                continue;
            }
            if let Some(content) =
                load_hints_from_directory(fs, &dir, &working_dir, &self.hints_filenames)
            {
                let key = format!("subdir_hints:{}", dir.display());
                results.push((key, content));
            }
            self.loaded_dirs.insert(dir);
        }
        results
    }
}

fn resolve_to_parent_dir(token: &str, working_dir: &Path) -> Option<PathBuf> {
    let path = Path::new(token);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        working_dir.join(path)
    };
    resolved.parent().map(|d| d.to_path_buf())
}

fn load_hints_from_directory(
    fs: &dyn HintFileSystem,
    directory: &Path,
    working_dir: &Path,
    hints_filenames: &[String],
) -> Option<String> {
    if !fs.is_dir(directory) || !directory.is_absolute() {
        return None;
    }

    if !directory.starts_with(working_dir) || directory == working_dir {
        return None;
    }

    let git_root = find_git_root(fs, working_dir);
    let import_boundary = git_root.unwrap_or(working_dir);
    let gitignore = Gitignore::empty();

    let mut directories: Vec<PathBuf> = directory
        .ancestors()
        .take_while(|d| d.starts_with(working_dir) && *d != working_dir)
        .map(|d| d.to_path_buf())
        .collect();
    directories.reverse();

    let mut contents = Vec::new();
    for dir in &directories {
        for hints_filename in hints_filenames {
            let hints_path = dir.join(hints_filename);
            if fs.is_file(&hints_path) {
                let mut visited = HashSet::new();
                let expanded = read_referenced_files(
                    fs,
                    &hints_path,
                    import_boundary,
                    &mut visited,
                    0,
                    &gitignore,
                );
                if !expanded.is_empty() {
                    contents.push(expanded);
                }
            }
        }
    }

    if contents.is_empty() {
        None
    } else {
        Some(format!(
            include_str!("prompts/subdirectory.md"),
            directory.display(),
            contents.join("\n")
        ))
    }
}

fn find_git_root<'a>(fs: &dyn HintFileSystem, start_dir: &'a Path) -> Option<&'a Path> {
    let mut check_dir = start_dir;

    loop {
        if fs.metadata(&check_dir.join(".git")).is_ok() {
            return Some(check_dir);
        }
        if let Some(parent) = check_dir.parent() {
            check_dir = parent;
        } else {
            break;
        }
    }

    None
}

fn get_local_directories(git_root: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
    match git_root {
        Some(git_root) => {
            let mut directories = Vec::new();
            let mut current_dir = cwd;

            loop {
                directories.push(current_dir.to_path_buf());
                if current_dir == git_root {
                    break;
                }
                if let Some(parent) = current_dir.parent() {
                    current_dir = parent;
                } else {
                    break;
                }
            }
            directories.reverse();
            directories
        }
        None => vec![cwd.to_path_buf()],
    }
}

/// Build a `Gitignore` that includes `.gitignore` files from the git root
/// down to `cwd`, matching git's hierarchical ignore semantics. When there
/// is no git root, only `cwd/.gitignore` is loaded.
pub fn build_gitignore(fs: &dyn HintFileSystem, cwd: &Path) -> Gitignore {
    let git_root = find_git_root(fs, cwd);
    let directories = get_local_directories(git_root, cwd);

    let mut builder = GitignoreBuilder::new(cwd);
    for dir in &directories {
        let gitignore_path = dir.join(".gitignore");
        if fs.is_file(&gitignore_path) {
            if let Ok(content) = fs.read_text(&gitignore_path, u64::MAX, true) {
                for (index, line) in content.lines().enumerate() {
                    let line = if index == 0 {
                        line.trim_start_matches('\u{feff}')
                    } else {
                        line
                    };
                    let _ = builder.add_line(Some(gitignore_path.clone()), line);
                }
            }
        }
    }
    builder.build().unwrap_or_else(|_| {
        GitignoreBuilder::new(cwd)
            .build()
            .expect("Failed to build default gitignore")
    })
}

pub fn load_hint_files(
    fs: &dyn HintFileSystem,
    cwd: &Path,
    hints_filenames: &[String],
    ignore_patterns: &Gitignore,
    global_hints_paths: &[PathBuf],
) -> String {
    let mut global_hints_contents = Vec::with_capacity(hints_filenames.len());
    let mut local_hints_contents = Vec::with_capacity(hints_filenames.len());

    for global_hints_path in global_hints_paths {
        if fs.is_file(global_hints_path) {
            let mut visited = HashSet::new();
            let hints_dir = global_hints_path.parent().unwrap();
            let global_ignore_patterns = GitignoreBuilder::new(hints_dir)
                .build()
                .unwrap_or_else(|_| Gitignore::empty());
            let expanded_content = read_referenced_files(
                fs,
                global_hints_path,
                hints_dir,
                &mut visited,
                0,
                &global_ignore_patterns,
            );
            if !expanded_content.is_empty() {
                global_hints_contents.push(expanded_content);
            }
        }
    }
    let git_root = find_git_root(fs, cwd);
    let local_directories = get_local_directories(git_root, cwd);

    let import_boundary = git_root.unwrap_or(cwd);

    for directory in &local_directories {
        for hints_filename in hints_filenames {
            let hints_path = directory.join(hints_filename);
            if fs.is_file(&hints_path) {
                let mut visited = HashSet::new();
                let expanded_content = read_referenced_files(
                    fs,
                    &hints_path,
                    import_boundary,
                    &mut visited,
                    0,
                    ignore_patterns,
                );
                if !expanded_content.is_empty() {
                    local_hints_contents.push(expanded_content);
                }
            }
        }
    }

    let mut hints = String::new();
    if !global_hints_contents.is_empty() {
        hints.push_str(include_str!("prompts/global.md"));
        hints.push_str(&global_hints_contents.join("\n"));
    }

    if !local_hints_contents.is_empty() {
        if !hints.is_empty() {
            hints.push_str("\n\n");
        }
        hints.push_str(include_str!("prompts/project.md"));
        hints.push_str(&local_hints_contents.join("\n"));
    }

    hints
}

#[cfg(test)]
#[path = "tracker_tests.rs"]
mod tests;
