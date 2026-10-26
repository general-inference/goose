#![cfg(all(not(target_arch = "wasm32"), feature = "project-context"))]

use goose_agent::project_context::{
    build_gitignore, load_hint_files, HintFileSystem, HintMetadata, SubdirectoryHintTracker,
    AGENTS_MD_FILENAME,
};
use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};

struct MemoryInstructions(BTreeMap<PathBuf, Option<&'static str>>);

impl HintFileSystem for MemoryInstructions {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let mut result = PathBuf::new();
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    result.pop();
                }
                Component::CurDir => {}
                _ => result.push(component),
            }
        }
        self.metadata(&result)?;
        Ok(result)
    }
    fn metadata(&self, path: &Path) -> io::Result<HintMetadata> {
        let content = self.0.get(path).ok_or(io::ErrorKind::NotFound)?;
        Ok(HintMetadata {
            is_file: content.is_some(),
            is_dir: content.is_none(),
            is_symlink: false,
            length: content.map_or(0, |text| text.len() as u64),
        })
    }
    fn symlink_metadata(&self, path: &Path) -> io::Result<HintMetadata> {
        self.metadata(path)
    }
    fn read_text(&self, path: &Path, limit: u64, _: bool) -> io::Result<String> {
        if path.file_name().is_some_and(|name| name == "denied.md") {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let content = self
            .0
            .get(path)
            .and_then(|text| *text)
            .ok_or(io::ErrorKind::NotFound)?;
        if content.len() as u64 > limit {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(content.to_owned())
    }
}

#[test]
fn hints_imports_gitignore_and_nested_discovery_use_only_host_files() {
    let root = std::env::temp_dir().join("gdk-virtual-instructions");
    let files = MemoryInstructions(
        [
            ("", None),
            (".git", None),
            ("src", None),
            ("global", None),
            (
                "AGENTS.md",
                Some("Root rules @allowed.md @ignored.md @denied.md"),
            ),
            (".gitignore", Some("\u{feff}ignored.md\n")),
            ("allowed.md", Some("Allowed import")),
            ("ignored.md", Some("Ignored secret")),
            ("denied.md", Some("Denied secret")),
            ("src/AGENTS.md", Some("Nested rules @../allowed.md")),
            ("global/AGENTS.md", Some("Global rules @extra.md")),
            ("global/extra.md", Some("Global import")),
        ]
        .into_iter()
        .map(|(path, text)| (root.join(path), text))
        .collect(),
    );
    let filenames = vec![AGENTS_MD_FILENAME.to_owned()];
    let ignores = build_gitignore(&files, &root);
    let local = load_hint_files(&files, &root, &filenames, &ignores, &[]);
    assert!(local.contains("Root rules") && local.contains("Allowed import"));
    assert!(local.contains("@ignored.md") && local.contains("@denied.md"));
    assert!(!local.contains("secret") && !local.contains("Global rules"));
    let with_global = load_hint_files(
        &files,
        &root,
        &filenames,
        &ignores,
        &[root.join("global/AGENTS.md")],
    );
    assert!(with_global.contains("Global rules") && with_global.contains("Global import"));

    let mut tracker = SubdirectoryHintTracker::new(filenames);
    let arguments =
        Some(serde_json::from_value(serde_json::json!({"path": "src/new.rs"})).unwrap());
    tracker.record_tool_arguments(&arguments, &root);
    let nested = tracker.load_new_hints(&files, &root);
    assert_eq!(nested.len(), 1);
    assert!(nested[0].1.contains("Nested rules") && nested[0].1.contains("Allowed import"));
    tracker.record_tool_arguments(&arguments, &root);
    assert!(tracker.load_new_hints(&files, &root).is_empty());
}
