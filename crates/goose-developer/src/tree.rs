use crate::filesystem::{FileKind, FileSystem, LocalFileSystem};
use std::collections::BTreeMap;
#[cfg(test)]
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TreeParams {
    pub path: String,
    #[serde(default = "default_depth")]
    pub depth: u32,
}

fn default_depth() -> u32 {
    2
}

pub struct TreeTool {
    filesystem: Arc<dyn FileSystem>,
}

impl TreeTool {
    pub fn new() -> Self {
        Self::with_filesystem(Arc::new(LocalFileSystem))
    }

    pub fn with_filesystem(filesystem: Arc<dyn FileSystem>) -> Self {
        Self { filesystem }
    }

    pub fn tree(&self, params: TreeParams) -> CallToolResult {
        self.tree_with_cwd(params, None)
    }

    pub fn tree_with_cwd(&self, params: TreeParams, working_dir: Option<&Path>) -> CallToolResult {
        let path = PathBuf::from(&params.path);
        let root = if path.is_absolute() {
            path
        } else {
            working_dir
                .map(Path::to_path_buf)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."))
                .join(path)
        };
        self.tree_at(root, params.depth)
    }

    fn tree_at(&self, root: PathBuf, depth: u32) -> CallToolResult {
        let max_depth = (depth != 0).then_some(depth as usize);
        let mut tree = match collect_tree(self.filesystem.as_ref(), &root, max_depth) {
            Ok(tree) => tree,
            Err(error) => {
                return CallToolResult::error(vec![ContentBlock::text(format!(
                    "Could not list {}: {error}",
                    root.display()
                ))])
            }
        };
        tree.compute_total_lines();

        let mut output = String::new();
        tree.render_into(0, &mut output);
        if output.is_empty() {
            output.push_str("(empty directory)");
        }

        CallToolResult::success(vec![ContentBlock::text(output)])
    }
}

impl Default for TreeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct DirectoryNode {
    dirs: BTreeMap<String, DirectoryNode>,
    files: BTreeMap<String, usize>,
    total_lines: usize,
}

impl DirectoryNode {
    fn insert_dir(&mut self, components: &[String]) {
        let mut node = self;
        for component in components {
            node = node.dirs.entry(component.clone()).or_default();
        }
    }

    fn insert_file(&mut self, components: &[String], line_count: usize) {
        if components.is_empty() {
            return;
        }

        let mut node = self;
        for component in &components[..components.len() - 1] {
            node = node.dirs.entry(component.clone()).or_default();
        }

        let filename = components[components.len() - 1].clone();
        node.files.insert(filename, line_count);
    }

    fn compute_total_lines(&mut self) -> usize {
        let dir_lines: usize = self
            .dirs
            .values_mut()
            .map(DirectoryNode::compute_total_lines)
            .sum();
        let file_lines: usize = self.files.values().copied().sum();
        self.total_lines = dir_lines + file_lines;
        self.total_lines
    }

    fn render_into(&self, depth: usize, out: &mut String) {
        let indent = "  ".repeat(depth);

        for (name, dir) in &self.dirs {
            out.push_str(&format!(
                "{}{}/  {}\n",
                indent,
                name,
                format_lines(dir.total_lines)
            ));
            dir.render_into(depth + 1, out);
        }

        for (name, line_count) in &self.files {
            out.push_str(&format!(
                "{}{}  {}\n",
                indent,
                name,
                format_lines(*line_count)
            ));
        }
    }
}

fn add_ignores(files: &dyn FileSystem, directory: &Path, rules: &mut Vec<Gitignore>) {
    for name in [".git/info/exclude", ".gitignore", ".ignore"] {
        let path = directory.join(name);
        if let Ok(text) = files.read_to_string(&path) {
            let mut builder = GitignoreBuilder::new(directory);
            for line in text.lines() {
                let _ = builder.add_line(Some(path.clone()), line);
            }
            if let Ok(rule) = builder.build() {
                rules.push(rule);
            }
        }
    }
}

fn collect_tree(
    files: &dyn FileSystem,
    root: &Path,
    max_depth: Option<usize>,
) -> std::io::Result<DirectoryNode> {
    // All traversal, ignore-file and content reads use the same host capability.
    let entries = files.read_dir(root)?;
    let mut rules = files.global_ignore().into_iter().collect::<Vec<_>>();
    for ancestor in root.ancestors().collect::<Vec<_>>().into_iter().rev() {
        add_ignores(files, ancestor, &mut rules);
    }
    let mut pending = vec![(root.to_path_buf(), 0, rules, entries)];
    let mut tree = DirectoryNode::default();
    while let Some((directory, depth, mut rules, entries)) = pending.pop() {
        if directory != root {
            add_ignores(files, &directory, &mut rules);
        }
        for entry in entries {
            let path = directory.join(&entry.name);
            let is_dir = entry.kind == FileKind::Directory;
            let matched = rules
                .iter()
                .rev()
                .map(|rule| rule.matched(&path, is_dir))
                .find(|matched| !matched.is_none());
            if matched.as_ref().is_some_and(|matched| matched.is_ignore())
                || (entry.name.to_string_lossy().starts_with('.')
                    && !matched
                        .as_ref()
                        .is_some_and(|matched| matched.is_whitelist()))
            {
                continue;
            }
            let Some(components) = path.strip_prefix(root).ok().and_then(relative_components)
            else {
                continue;
            };
            match entry.kind {
                FileKind::Directory => {
                    tree.insert_dir(&components);
                    if max_depth.is_none_or(|limit| depth < limit) {
                        if let Ok(children) = files.read_dir(&path) {
                            pending.push((path, depth + 1, rules.clone(), children));
                        }
                    }
                }
                FileKind::File => tree.insert_file(&components, count_file_lines(files, &path)),
                FileKind::Symlink | FileKind::Other => {}
            }
        }
    }
    Ok(tree)
}

fn relative_components(path: &Path) -> Option<Vec<String>> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => components.push(value.to_string_lossy().into_owned()),
            _ => return None,
        }
    }

    if components.is_empty() {
        None
    } else {
        Some(components)
    }
}

fn count_file_lines(files: &dyn FileSystem, path: &Path) -> usize {
    match files.read_to_string(path) {
        Ok(content) => content.lines().count(),
        Err(_) => 0,
    }
}

fn format_lines(lines: usize) -> String {
    if lines >= 1000 {
        format!("[{}K]", lines / 1000)
    } else {
        format!("[{}]", lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ContentBlock;
    use tempfile::TempDir;

    fn extract_text(result: &CallToolResult) -> &str {
        match &result.content[0] {
            ContentBlock::Text(t) => &t.text,
            _ => panic!("expected text"),
        }
    }

    fn setup_tree() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join("tests")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
        fs::write(dir.path().join("tests/test.rs"), "#[test]\nfn t() {}\n").unwrap();
        dir
    }

    #[test]
    fn tree_lists_files_and_directories() {
        let dir = setup_tree();
        let tool = TreeTool::new();

        let result = tool.tree(TreeParams {
            path: dir.path().display().to_string(),
            depth: 2,
        });

        let text = extract_text(&result);
        assert!(text.contains("src/"));
        assert!(text.contains("tests/"));
        assert!(text.contains("main.rs"));
    }

    #[test]
    fn tree_respects_depth() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        fs::write(dir.path().join("a/b/c/deep.rs"), "fn deep() {}\n").unwrap();

        let tool = TreeTool::new();
        let result = tool.tree(TreeParams {
            path: dir.path().display().to_string(),
            depth: 1,
        });

        let text = extract_text(&result);
        assert!(text.contains("a/"));
        assert!(text.contains("b/"));
        assert!(!text.contains("deep.rs"));
    }

    #[test]
    fn tree_uses_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored/\n*.log\n").unwrap();
        fs::create_dir_all(dir.path().join("ignored")).unwrap();
        fs::write(dir.path().join("ignored/secret.rs"), "fn secret() {}\n").unwrap();
        fs::write(dir.path().join("visible.rs"), "fn visible() {}\n").unwrap();
        fs::write(dir.path().join("debug.log"), "hidden\n").unwrap();

        let tool = TreeTool::new();
        let result = tool.tree(TreeParams {
            path: dir.path().display().to_string(),
            depth: 2,
        });

        let text = extract_text(&result);
        assert!(text.contains("visible.rs"));
        assert!(!text.contains("ignored"));
        assert!(!text.contains("debug.log"));
    }
}
