use goose_agent::skills::tool::{load_skill, load_supporting_file, SkillLoader};
use goose_agent::skills::SourceEntry;
use rmcp::model::CallToolResult;
use serde_json::json;
use std::cell::Cell;
use std::io;
use std::path::Path;

struct Files {
    calls: Cell<usize>,
    denied: bool,
}

impl SkillLoader for Files {
    fn read_supporting_file(
        &self,
        dir: &Path,
        relative: &Path,
        limit: usize,
    ) -> io::Result<String> {
        self.calls.set(self.calls.get() + 1);
        assert_eq!(dir, Path::new("/skills/review"));
        assert_eq!(relative, Path::new("guide.md"));
        if self.denied {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        assert!(limit >= 2);
        Ok("é日".into())
    }
}

fn source() -> SourceEntry {
    SourceEntry {
        name: "review".into(),
        content: "Review $ARGUMENTS".into(),
        path: "/skills/review".into(),
        supporting_files: vec!["/skills/review/guide.md".into()],
        ..Default::default()
    }
}

fn text(result: &CallToolResult) -> &str {
    &result.content[0].as_text().unwrap().text
}

#[test]
fn shares_argument_rendering_and_rejects_invalid_arguments_without_io() {
    let files = Files {
        calls: Cell::new(0),
        denied: false,
    };
    let result = load_skill(
        &[source()],
        json!({"name":"review", "args":"src/main.rs"})
            .as_object()
            .cloned(),
        &files,
        200_000,
    );
    assert_ne!(result.is_error, Some(true));
    assert!(text(&result).contains("Review src/main.rs"));
    for arguments in [
        None,
        json!({"name": 42}).as_object().cloned(),
        json!({"name":"review", "args":42}).as_object().cloned(),
    ] {
        assert_eq!(
            load_skill(&[source()], arguments, &files, 200_000).is_error,
            Some(true)
        );
    }
    assert_eq!(files.calls.get(), 0);
}

#[test]
fn only_discovered_supporting_files_reach_the_host() {
    let files = Files {
        calls: Cell::new(0),
        denied: true,
    };
    for name in ["review/../secret", "review/unknown.md"] {
        let result = load_skill(
            &[source()],
            json!({"name":name}).as_object().cloned(),
            &files,
            200_000,
        );
        assert_eq!(result.is_error, Some(true));
    }
    assert_eq!(files.calls.get(), 0);
    let result = load_skill(
        &[source()],
        json!({"name":"review/guide.md"}).as_object().cloned(),
        &files,
        200_000,
    );
    assert_eq!(result.is_error, Some(true));
    assert!(text(&result).contains("Failed to read"));
    assert_eq!(files.calls.get(), 1);
}

#[test]
fn supporting_file_limit_includes_wrapper_and_counts_characters() {
    let files = Files {
        calls: Cell::new(0),
        denied: false,
    };
    let expected = "# Loaded: review/guide.md\n\né日\n\n---\nFile loaded into context.";
    let read = |limit| {
        load_supporting_file(
            Path::new("/skills/review"),
            Path::new("guide.md"),
            "review/guide.md",
            &files,
            limit,
        )
    };
    assert_eq!(read(expected.chars().count()).unwrap(), expected);
    assert_eq!(read(1).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    assert_eq!(files.calls.get(), 1);
}
