use super::super::NativeHintFileSystem;
use super::*;
use std::fs;
use tempfile::TempDir;
#[test]
fn resolve_to_parent_dir_relative() {
    let wd = Path::new("/home/user/project");
    assert_eq!(
        resolve_to_parent_dir("src/main.rs", wd),
        Some(PathBuf::from("/home/user/project/src"))
    );
}

#[test]
fn resolve_to_parent_dir_absolute() {
    let wd = Path::new("/home/user/project");
    assert_eq!(
        resolve_to_parent_dir("/tmp/foo.rs", wd),
        Some(PathBuf::from("/tmp"))
    );
}

#[test]
fn tracker_records_path_argument() {
    let temp_dir = TempDir::new().unwrap();
    let wd = temp_dir.path().join("project");
    let src = wd.join("src");
    fs::create_dir_all(&src).unwrap();
    let mut tracker =
        SubdirectoryHintTracker::new(vec![GOOSE_HINTS_FILENAME.into(), AGENTS_MD_FILENAME.into()]);
    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"path": "src/main.rs"}"#).unwrap();
    tracker.record_tool_arguments(&Some(args), &wd);
    let hints = tracker.load_new_hints(&NativeHintFileSystem, &wd);
    assert!(hints.is_empty());
    assert!(tracker.loaded_dirs.contains(&src.canonicalize().unwrap()));
}

#[test]
fn tracker_records_command_argument() {
    let temp_dir = TempDir::new().unwrap();
    let wd = temp_dir.path().join("project");
    let nested = wd.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let mut tracker =
        SubdirectoryHintTracker::new(vec![GOOSE_HINTS_FILENAME.into(), AGENTS_MD_FILENAME.into()]);
    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"command": "cat nested/doc.md"}"#).unwrap();
    tracker.record_tool_arguments(&Some(args), &wd);
    let hints = tracker.load_new_hints(&NativeHintFileSystem, &wd);
    assert!(hints.is_empty());
    assert!(tracker
        .loaded_dirs
        .contains(&nested.canonicalize().unwrap()));
}

#[test]
fn tracker_skips_flags_in_command() {
    let temp_dir = TempDir::new().unwrap();
    let wd = temp_dir.path().join("project");
    let src = wd.join("src");
    fs::create_dir_all(&src).unwrap();
    let mut tracker =
        SubdirectoryHintTracker::new(vec![GOOSE_HINTS_FILENAME.into(), AGENTS_MD_FILENAME.into()]);
    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"command": "grep -rn pattern src/lib.rs"}"#).unwrap();
    tracker.record_tool_arguments(&Some(args), &wd);
    let _ = tracker.load_new_hints(&NativeHintFileSystem, &wd);
    assert!(tracker.loaded_dirs.contains(&src.canonicalize().unwrap()));
    assert_eq!(tracker.loaded_dirs.len(), 1);
}

#[test]
fn tracker_loads_subdirectory_hints() {
    let temp_dir = TempDir::new().unwrap();
    let project_root = temp_dir.path().to_path_buf();
    let subdir = project_root.join("nested");
    fs::create_dir_all(&subdir).unwrap();
    fs::write(
        subdir.join(GOOSE_HINTS_FILENAME),
        "nested subdirectory hints",
    )
    .unwrap();

    let mut tracker =
        SubdirectoryHintTracker::new(vec![GOOSE_HINTS_FILENAME.into(), AGENTS_MD_FILENAME.into()]);
    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"path": "nested/foo.rs"}"#).unwrap();
    tracker.record_tool_arguments(&Some(args), &project_root);
    let hints = tracker.load_new_hints(&NativeHintFileSystem, &project_root);
    assert_eq!(hints.len(), 1);
    assert!(hints[0].0.contains("nested"));
    assert!(hints[0].1.contains("nested subdirectory hints"));
}

#[test]
fn tracker_deduplicates_directories() {
    let temp_dir = TempDir::new().unwrap();
    let project_root = temp_dir.path().to_path_buf();
    let subdir = project_root.join("nested");
    fs::create_dir_all(&subdir).unwrap();
    fs::write(subdir.join(GOOSE_HINTS_FILENAME), "nested hints").unwrap();

    let mut tracker =
        SubdirectoryHintTracker::new(vec![GOOSE_HINTS_FILENAME.into(), AGENTS_MD_FILENAME.into()]);
    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"path": "nested/foo.rs"}"#).unwrap();
    tracker.record_tool_arguments(&Some(args.clone()), &project_root);
    let hints = tracker.load_new_hints(&NativeHintFileSystem, &project_root);
    assert_eq!(hints.len(), 1);

    tracker.record_tool_arguments(&Some(args), &project_root);
    let hints = tracker.load_new_hints(&NativeHintFileSystem, &project_root);
    assert!(hints.is_empty());
}

#[test]
fn tracker_rejects_parent_traversal_outside_working_directory() {
    let temp_dir = TempDir::new().unwrap();
    let project_root = temp_dir.path().join("project");
    let nested = project_root.join("nested");
    let outside = temp_dir.path().join("outside");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join(GOOSE_HINTS_FILENAME), "outside hints").unwrap();

    let mut tracker =
        SubdirectoryHintTracker::new(vec![GOOSE_HINTS_FILENAME.into(), AGENTS_MD_FILENAME.into()]);
    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"path": "nested/../../outside/file.rs"}"#).unwrap();
    tracker.record_tool_arguments(&Some(args), &project_root);

    assert!(tracker
        .load_new_hints(&NativeHintFileSystem, &project_root)
        .is_empty());
}

#[cfg(unix)]
#[test]
fn tracker_rejects_directory_symlink_outside_working_directory() {
    use std::os::unix::fs::symlink;

    let temp_dir = TempDir::new().unwrap();
    let project_root = temp_dir.path().join("project");
    let outside = temp_dir.path().join("outside");
    fs::create_dir_all(&project_root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join(GOOSE_HINTS_FILENAME), "outside hints").unwrap();
    symlink(&outside, project_root.join("alias")).unwrap();

    let mut tracker =
        SubdirectoryHintTracker::new(vec![GOOSE_HINTS_FILENAME.into(), AGENTS_MD_FILENAME.into()]);
    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"path": "alias/file.rs"}"#).unwrap();
    tracker.record_tool_arguments(&Some(args), &project_root);

    assert!(tracker
        .load_new_hints(&NativeHintFileSystem, &project_root)
        .is_empty());
}

#[cfg(unix)]
#[test]
fn tracker_preserves_in_boundary_symlinks_and_deduplicates_aliases() {
    use std::os::unix::fs::symlink;

    let temp_dir = TempDir::new().unwrap();
    let project_root = temp_dir.path().join("project");
    let real = project_root.join("real");
    fs::create_dir_all(&real).unwrap();
    fs::write(real.join(GOOSE_HINTS_FILENAME), "real hints").unwrap();
    symlink(&real, project_root.join("alias")).unwrap();

    let mut tracker =
        SubdirectoryHintTracker::new(vec![GOOSE_HINTS_FILENAME.into(), AGENTS_MD_FILENAME.into()]);
    let alias_args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"path": "alias/file.rs"}"#).unwrap();
    tracker.record_tool_arguments(&Some(alias_args), &project_root);
    let hints = tracker.load_new_hints(&NativeHintFileSystem, &project_root);
    assert_eq!(hints.len(), 1);
    assert!(hints[0].1.contains("real hints"));

    let real_args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"path": "real/file.rs"}"#).unwrap();
    tracker.record_tool_arguments(&Some(real_args), &project_root);
    assert!(tracker
        .load_new_hints(&NativeHintFileSystem, &project_root)
        .is_empty());
    assert_eq!(tracker.loaded_dirs.len(), 1);
}

#[test]
fn tracker_retries_directory_after_parent_is_created() {
    let temp_dir = TempDir::new().unwrap();
    let project_root = temp_dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();

    let mut tracker =
        SubdirectoryHintTracker::new(vec![GOOSE_HINTS_FILENAME.into(), AGENTS_MD_FILENAME.into()]);
    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"path": "future/file.rs"}"#).unwrap();
    tracker.record_tool_arguments(&Some(args.clone()), &project_root);
    assert!(tracker
        .load_new_hints(&NativeHintFileSystem, &project_root)
        .is_empty());
    assert!(tracker.loaded_dirs.is_empty());

    let future = project_root.join("future");
    fs::create_dir_all(&future).unwrap();
    fs::write(future.join(GOOSE_HINTS_FILENAME), "future hints").unwrap();
    tracker.record_tool_arguments(&Some(args), &project_root);

    let hints = tracker.load_new_hints(&NativeHintFileSystem, &project_root);
    assert_eq!(hints.len(), 1);
    assert!(hints[0].1.contains("future hints"));
}
